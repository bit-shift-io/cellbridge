//! The production bridge loop.
//!
//! This module owns everything that runs while the bridge is up: the WireGuard
//! UDP socket, the [`Tunnel`] pump, the [`Gateway`] and its TCP splices, the
//! packet-level UDP forwarder and its per-flow sockets, and the
//! [`crate::BridgeMonitor`] state machine.
//!
//! # Lifecycle
//!
//! [`start`] builds the whole data path and spawns [`BridgeLoop::run`] on the
//! current `tokio` runtime; [`stop`] signals that task, waits for it, drops every
//! socket, resets the counters and returns the state to
//! [`BridgeState::Idle`]. The lifecycle is process-global: the app has one
//! phone, one tunnel and one bridge. `start` while already running is an error
//! (and does not disturb the running bridge), and `stop` when idle is a no-op.
//!
//! # Loss
//!
//! The only loss signal is [`Tunnel::is_connected`], fed to
//! [`BridgeMonitor::observe`] every loop tick. An error out of
//! [`Tunnel::tick`] — in particular boringtun's `ConnectionExpired` — means
//! "re-handshake", not "gone": it is tolerated, and the monitor's grace period
//! decides when the link is actually lost.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::BridgeConfig;
use crate::gateway::{Gateway, GatewayEvent, Routed};
use crate::stats::{BridgeMonitor, BridgeState, BridgeStatus, LinkEvent, Stats, StatsCounters};
use crate::tcp::{IoOp, TcpSpliceEngine};
use crate::tunnel::{Tunnel, TunnelOutput};
use crate::udp::{FlowId, OutboundUdp, UdpForwarder};

/// Largest UDP datagram we read from either WireGuard or a forwarded flow.
const MAX_DATAGRAM: usize = 65_535;

/// How long the link may be down before its flows are torn down.
///
/// boringtun retries a handshake every `REKEY_TIMEOUT` (5 s), and a routine
/// rekey makes `Tunnel::is_connected` false until the new session completes.
/// A grace shorter than that would drop live flows on a perfectly healthy link;
/// this is deliberately double the rekey cadence, which still surfaces a truly
/// dead uplink in seconds.
const HANDSHAKE_GRACE: Duration = Duration::from_secs(10);

/// How often the tunnel timers, the gateway and the monitor are serviced.
///
/// `TunnelOutput::retry_after` is always `None`, so `Tunnel::tick` has to be
/// driven on a fixed cadence. 100 ms is frequent enough to send the handshake
/// promptly and to keep keepalives on time without being a busy loop.
const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// How often the loop asks boringtun to start a handshake while the link is down.
///
/// boringtun retransmits an in-flight handshake itself every `REKEY_TIMEOUT`
/// (5 s); this is only the fallback for a session that expired silently — its
/// timer path can clear the session without returning an error — and for a
/// handshake attempt that has given up. Two seconds bounds the recovery time
/// without queueing a meaningful number of empty packets.
const HANDSHAKE_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// Idle time after which a forwarded UDP flow's socket is released.
///
/// Kept in step with the forwarder's own idle timeout so the socket table and
/// the forwarder's flow table age out together.
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Hard cap on live forwarded UDP sockets.
const UDP_MAX_FLOWS: usize = 512;

/// Capacity of the reply channel between per-flow readers and the loop.
const UDP_REPLY_QUEUE: usize = 1024;

/// Process-wide state the FRB getters read without touching the async runtime.
struct Shared {
    status: Mutex<BridgeStatus>,
    counters: Arc<StatsCounters>,
    active_flows: AtomicUsize,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            status: Mutex::new(BridgeStatus {
                state: BridgeState::Idle,
                error: None,
            }),
            counters: Arc::new(StatsCounters::new()),
            active_flows: AtomicUsize::new(0),
        }
    }

    fn status(&self) -> BridgeStatus {
        lock(&self.status).clone()
    }

    fn set_status(&self, status: BridgeStatus) {
        *lock(&self.status) = status;
    }
}

/// Lock a mutex, recovering the data if a previous holder panicked.
///
/// A poisoned status mutex must not turn a status read into a second panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A running bridge task.
struct Running {
    stop: oneshot::Sender<()>,
    join: JoinHandle<()>,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

/// Serializes `start`/`stop` so two calls cannot race for the single slot.
static LIFECYCLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The one running bridge, if any. Taken and replaced by `start`/`stop`.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

fn shared() -> &'static Arc<Shared> {
    SHARED.get_or_init(|| Arc::new(Shared::new()))
}

/// Whether a bridge task currently owns the data path.
///
/// Exposed to this module's tests; the FRB surface reports lifecycle through
/// [`status`] instead.
pub(crate) fn is_running() -> bool {
    lock(&RUNNING).is_some()
}

/// The current lifecycle state, readable while the loop is running.
pub fn status() -> BridgeStatus {
    shared().status()
}

/// The byte and flow counters, readable while the loop is running.
pub fn stats() -> Stats {
    let shared = shared();
    shared
        .counters
        .snapshot(shared.active_flows.load(Ordering::Relaxed))
}

/// Start the bridge.
///
/// `wg_socket_fd` comes from Android: `Some(fd)` is a socket already bound to
/// the Wi-Fi network, and **ownership transfers to Rust** — the caller must
/// have detached it (Kotlin: `ParcelFileDescriptor.detachFd()`) and must not
/// close it afterwards. `None` binds an ordinary UDP socket, which is the
/// desktop/Linux development path.
///
/// The socket is left unconnected: datagrams are sent with `send_to(endpoint)`
/// and accepts are filtered against the peer address. That keeps an adopted
/// descriptor bound to a specific interface address, or one that is already
/// connected, from tripping a hard `connect` error at startup.
pub async fn start(cfg: BridgeConfig, wg_socket_fd: Option<i32>) -> Result<(), String> {
    let _lifecycle = LIFECYCLE.lock().await;
    if is_running() {
        return Err("the bridge is already running".to_string());
    }

    let endpoint: SocketAddr = cfg
        .endpoint
        .parse()
        .map_err(|err| format!("invalid endpoint `{}`: {err}", cfg.endpoint))?;
    let (address, prefix_len) = parse_address(&cfg.address)?;

    // Build everything fallible before touching the shared status, so a bad
    // configuration leaves the app in `Idle` rather than a stuck `Connecting`.
    let tunnel = Tunnel::new(&cfg).map_err(|err| err.to_string())?;
    let std_socket = match wg_socket_fd {
        Some(fd) => adopt_socket(fd)
            .map_err(|err| format!("cannot adopt WireGuard socket fd {fd}: {err}"))?,
        None => bind_tunnel_socket(endpoint)
            .map_err(|err| format!("cannot bind the WireGuard socket: {err}"))?,
    };
    let tunnel_socket = UdpSocket::from_std(std_socket)
        .map_err(|err| format!("cannot register the WireGuard socket: {err}"))?;

    let shared = shared();
    shared.counters.reset();
    shared.active_flows.store(0, Ordering::Relaxed);

    let counters = Arc::clone(&shared.counters);
    let (reply_tx, reply_rx) = mpsc::channel(UDP_REPLY_QUEUE);
    let (stop_tx, stop_rx) = oneshot::channel();

    let bridge = BridgeLoop {
        tunnel,
        gateway: Gateway::new(address, prefix_len, usize::from(cfg.mtu)),
        engine: TcpSpliceEngine::with_counters(Arc::clone(&counters)),
        udp: UdpForwarder::with_counters(UDP_MAX_FLOWS, UDP_IDLE_TIMEOUT, Arc::clone(&counters)),
        monitor: BridgeMonitor::new(HANDSHAKE_GRACE),
        tunnel_socket,
        endpoint,
        reply_tx,
        reply_rx,
        flows: HashMap::new(),
        shared: Arc::clone(shared),
        mtu: usize::from(cfg.mtu),
        last_handshake_request: None,
    };

    shared.set_status(BridgeStatus {
        state: BridgeState::Connecting,
        error: None,
    });
    let join = tokio::spawn(bridge.run(stop_rx));
    *lock(&RUNNING) = Some(Running {
        stop: stop_tx,
        join,
    });
    Ok(())
}

/// Stop the bridge.
///
/// Waits for the loop task to exit, drops every tunnel and flow socket, zeroes
/// the counters and returns to [`BridgeState::Idle`]. Safe to call when the
/// bridge is not running.
pub async fn stop() -> Result<(), String> {
    let _lifecycle = LIFECYCLE.lock().await;
    let running = lock(&RUNNING).take();

    let mut task_result = Ok(());
    if let Some(running) = running {
        let _ = running.stop.send(());
        if running.join.await.is_err() {
            task_result = Err("the bridge task stopped unexpectedly".to_string());
        }
    }

    let shared = shared();
    shared.active_flows.store(0, Ordering::Relaxed);
    shared.counters.reset();
    shared.set_status(BridgeStatus {
        state: BridgeState::Idle,
        error: None,
    });
    task_result
}

/// Parse `"a.b.c.d/prefix"` into the gateway interface address.
fn parse_address(cidr: &str) -> Result<(Ipv4Addr, u8), String> {
    let (address, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| format!("invalid address `{cidr}`: expected CIDR, e.g. 10.7.0.2/24"))?;
    let address: Ipv4Addr = address
        .trim()
        .parse()
        .map_err(|err| format!("invalid address `{cidr}`: {err}"))?;
    let prefix: u8 = prefix
        .trim()
        .parse()
        .map_err(|err| format!("invalid prefix in `{cidr}`: {err}"))?;
    if prefix > 32 {
        return Err(format!("invalid prefix in `{cidr}`: {prefix} > 32"));
    }
    Ok((address, prefix))
}

/// Adopt a file descriptor handed over by Android.
///
/// # Safety and ownership
///
/// The caller transfers ownership: the fd must be a valid, open, non-blocking
/// UDP socket that the caller has detached (never closed). This function wraps
/// it in a [`std::net::UdpSocket`], so dropping the resulting socket closes it
/// exactly once.
fn adopt_socket(fd: i32) -> io::Result<std::net::UdpSocket> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;

        if fd < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a socket fd cannot be negative",
            ));
        }
        // SAFETY: the caller's contract (see above) is that `fd` is a valid,
        // open socket whose ownership they have given up.
        let socket = unsafe { std::net::UdpSocket::from_raw_fd(fd) };
        socket.set_nonblocking(true)?;
        Ok(socket)
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "socket fd transfer is only supported on unix",
        ))
    }
}

/// Bind the ordinary UDP socket used when no Android fd is supplied.
fn bind_tunnel_socket(endpoint: SocketAddr) -> io::Result<std::net::UdpSocket> {
    let bind = if endpoint.is_ipv4() {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    } else {
        "[::]:0".parse().expect("a valid IPv6 wildcard address")
    };
    let socket = std::net::UdpSocket::bind(bind)?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

/// A flow's outbound socket plus the task reading its replies.
struct FlowSockets {
    socket: Arc<UdpSocket>,
    reader: JoinHandle<()>,
    last_seen: Instant,
}

/// A reply, or a fatal read error, from one forwarded flow.
enum UdpEvent {
    Datagram { flow: FlowId, payload: Vec<u8> },
    Closed(FlowId),
}

/// The state owned by the single bridge task.
struct BridgeLoop {
    tunnel: Tunnel,
    gateway: Gateway,
    engine: TcpSpliceEngine,
    udp: UdpForwarder,
    monitor: BridgeMonitor,
    tunnel_socket: UdpSocket,
    /// The WireGuard peer endpoint, used for every send and to filter receives.
    endpoint: SocketAddr,
    reply_tx: mpsc::Sender<UdpEvent>,
    reply_rx: mpsc::Receiver<UdpEvent>,
    flows: HashMap<FlowId, FlowSockets>,
    shared: Arc<Shared>,
    mtu: usize,
    /// When the loop last asked boringtun to start a handshake while down.
    ///
    /// `Tunnel::tick` retransmits an in-flight handshake, but a session can
    /// expire without `tick` returning an error, and a handshake attempt can
    /// give up, so the loop occasionally asks for a fresh one.
    last_handshake_request: Option<Instant>,
}

impl BridgeLoop {
    /// Drive the data path until `stop` fires.
    async fn run(mut self, mut stop: oneshot::Receiver<()>) {
        // Enter `Connecting` before the first publish. Without this the monitor
        // would stay `Idle`, `observe` would early-return, and the loop would
        // overwrite the state `start` set with `Idle` on its very first tick.
        self.monitor.start();

        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut datagram = vec![0u8; MAX_DATAGRAM];

        loop {
            let gateway_delay = self
                .gateway
                .poll_delay(smoltcp::time::Instant::now())
                .map(|delay| Duration::from_micros(delay.total_micros()));

            tokio::select! {
                _ = &mut stop => break,
                received = self.tunnel_socket.recv_from(&mut datagram) => {
                    if let Ok((n, from)) = received && from == self.endpoint {
                        // A decrypt error means "wait for the re-handshake"; it
                        // is never a reason to tear the link down.
                        if let Ok(output) = self.tunnel.handle_udp(&datagram[..n]) {
                            self.absorb_tunnel(output).await;
                        }
                    }
                }
                event = self.reply_rx.recv() => {
                    match event {
                        Some(UdpEvent::Datagram { flow, payload }) => {
                            self.on_udp_reply(flow, &payload).await;
                        }
                        Some(UdpEvent::Closed(flow)) => self.drop_flow(flow),
                        None => break,
                    }
                }
                _ = ticker.tick() => {
                    self.service_tunnel_timers().await;
                    self.expire_udp();
                }
                _ = tokio::time::sleep(gateway_delay.unwrap_or_default()), if gateway_delay.is_some() => {}
            }

            self.service_gateway().await;
        }

        self.shutdown();
    }

    /// Advance WireGuard timers and make sure a handshake is under way.
    ///
    /// An error from `tick` (including `ConnectionExpired`) means "start the
    /// handshake again", never "the link is lost": loss is decided solely by
    /// `Tunnel::is_connected` in [`Self::service_gateway`].
    async fn service_tunnel_timers(&mut self) {
        if let Ok(output) = self.tunnel.tick() {
            self.absorb_tunnel(output).await;
        }

        if self.tunnel.is_connected() {
            self.last_handshake_request = None;
            return;
        }

        let due = self
            .last_handshake_request
            .is_none_or(|at| at.elapsed() >= HANDSHAKE_RETRY_INTERVAL);
        if due {
            // boringtun initiates a handshake from `encapsulate`; an empty
            // payload starts one without injecting an inner packet.
            if let Ok(output) = self.tunnel.encapsulate(&[]) {
                self.last_handshake_request = Some(Instant::now());
                self.absorb_tunnel(output).await;
            }
        }
    }

    /// Send the tunnel's network datagrams and route its decrypted packets.
    async fn absorb_tunnel(&mut self, output: TunnelOutput) {
        for datagram in output.to_network {
            let _ = self.tunnel_socket.send_to(&datagram, self.endpoint).await;
        }
        for packet in output.to_tunnel {
            self.route_inner(packet).await;
        }
    }

    /// Demultiplex one decrypted inner IP packet.
    async fn route_inner(&mut self, packet: Vec<u8>) {
        // UDP is diverted before smoltcp; keep the bytes only on that path so
        // the common TCP case does not pay for a clone.
        let udp_packet = is_ipv4_udp(&packet).then(|| packet.clone());
        let routed = self.gateway.route(packet);

        if routed == Routed::Udp
            && let Some(bytes) = udp_packet
            && let Some(outbound) = self.udp.handle_outbound(&bytes)
        {
            self.send_udp(outbound).await;
        }
    }

    /// Send one forwarded UDP datagram, creating its source-port-preserving
    /// socket on first use.
    async fn send_udp(&mut self, outbound: OutboundUdp) {
        if !self.flows.contains_key(&outbound.flow) {
            self.open_flow(outbound.flow).await;
        }
        let Some(flow) = self.flows.get_mut(&outbound.flow) else {
            return;
        };
        flow.last_seen = Instant::now();
        let _ = flow.socket.send(&outbound.payload).await;
    }

    /// Bind a socket on the client's source port and start reading replies.
    ///
    /// The port is the one the LAN client used, and it is preserved on the wire
    /// as the spec requires. A per-flow socket may share that port with another
    /// flow (same client port, different destination), so it is opened with
    /// `SO_REUSEADDR`/`SO_REUSEPORT`; `connect` makes the kernel demultiplex
    /// replies by the full four-tuple.
    async fn open_flow(&mut self, flow: FlowId) {
        while self.flows.len() >= UDP_MAX_FLOWS && self.evict_oldest_flow() {}

        let socket = match bind_flow_socket(flow.client.port()) {
            Ok(socket) => match UdpSocket::from_std(socket) {
                Ok(socket) => socket,
                Err(_) => return,
            },
            Err(_) => return,
        };
        if socket.connect(flow.dst).await.is_err() {
            return;
        }

        let socket = Arc::new(socket);
        let reader = spawn_reader(Arc::clone(&socket), self.reply_tx.clone(), flow);
        self.flows.insert(
            flow,
            FlowSockets {
                socket,
                reader,
                last_seen: Instant::now(),
            },
        );
    }

    /// Build and encapsulate the reply to one forwarded flow.
    async fn on_udp_reply(&mut self, flow: FlowId, payload: &[u8]) {
        if let Some(sockets) = self.flows.get_mut(&flow) {
            sockets.last_seen = Instant::now();
        }
        // The capped variant: an oversized reply is dropped here rather than
        // handed to the tunnel only to be lost there.
        if let Some(reply) = self.udp.handle_reply_within_mtu(flow, payload, self.mtu) {
            self.encapsulate_and_send(&reply).await;
        }
    }

    /// Run the smoltcp stack, the splices and the monitor, then flush TX.
    async fn service_gateway(&mut self) {
        let now = smoltcp::time::Instant::now();

        for event in self.gateway.poll(now) {
            let GatewayEvent::TcpAccepted {
                handle,
                client,
                dst,
            } = event;
            self.engine.on_accept(handle, client, dst);
        }

        for op in self.engine.pump(&mut self.gateway) {
            apply_op(&mut self.gateway, op);
        }

        // A second poll flushes the FIN/RST the applied ops queued, so a
        // finished socket can be removed without truncating its last segment.
        self.gateway.poll(now);
        let packets = self.gateway.take_tx();
        for handle in self.engine.closed_handles() {
            self.gateway.tcp_remove(handle);
        }
        for packet in packets {
            self.encapsulate_and_send(&packet).await;
        }

        if self
            .monitor
            .observe(self.tunnel.is_connected(), Instant::now())
            == LinkEvent::Lost
        {
            self.handle_loss().await;
        }

        self.publish();
    }

    /// Tear every flow down once the monitor has declared the link lost.
    async fn handle_loss(&mut self) {
        let now = smoltcp::time::Instant::now();
        let mut packets = Vec::new();

        for op in self.engine.tear_down_all() {
            apply_op(&mut self.gateway, op);
        }
        self.gateway.poll(now);
        packets.extend(self.gateway.take_tx());
        for handle in self.engine.closed_handles() {
            self.gateway.tcp_remove(handle);
        }

        for flow in self.udp.tear_down_all() {
            self.drop_flow(flow);
        }

        for packet in packets {
            self.encapsulate_and_send(&packet).await;
        }
    }

    /// Encapsulate one inner packet and put the result on the WireGuard wire.
    async fn encapsulate_and_send(&mut self, packet: &[u8]) {
        if let Ok(output) = self.tunnel.encapsulate(packet) {
            for datagram in output.to_network {
                let _ = self.tunnel_socket.send_to(&datagram, self.endpoint).await;
            }
        }
    }

    /// Expire UDP flows from both the forwarder's table and the socket table.
    fn expire_udp(&mut self) {
        let now = Instant::now();
        self.udp.expire(now);
        self.flows.retain(|_, sockets| {
            if now.saturating_duration_since(sockets.last_seen) >= UDP_IDLE_TIMEOUT {
                sockets.reader.abort();
                false
            } else {
                true
            }
        });
    }

    /// Drop one flow's socket and its forwarder bookkeeping together.
    fn drop_flow(&mut self, flow: FlowId) {
        release_flow(&mut self.udp, &mut self.flows, flow);
    }

    /// Evict the least recently used flow socket; false when there is none.
    fn evict_oldest_flow(&mut self) -> bool {
        let Some(flow) = self
            .flows
            .iter()
            .min_by_key(|(_, sockets)| sockets.last_seen)
            .map(|(flow, _)| *flow)
        else {
            return false;
        };
        self.drop_flow(flow);
        true
    }

    /// Publish the current status and live flow count for the FRB getters.
    fn publish(&self) {
        self.shared
            .active_flows
            .store(self.engine.flow_count() + self.udp.len(), Ordering::Relaxed);
        self.shared.set_status(self.monitor.status());
    }

    /// Release every flow and reader when the loop exits.
    fn shutdown(mut self) {
        for (_, sockets) in self.flows.drain() {
            sockets.reader.abort();
        }
        self.udp.tear_down_all();
        self.engine.tear_down_all();
    }
}

/// Drop a flow's socket and its forwarder bookkeeping together.
///
/// Keeping the two in step is what makes `active_flows` honest: a socket that
/// is gone must stop being counted immediately, not linger in the forwarder's
/// table until the next idle expiry.
fn release_flow(udp: &mut UdpForwarder, flows: &mut HashMap<FlowId, FlowSockets>, flow: FlowId) {
    if let Some(sockets) = flows.remove(&flow) {
        sockets.reader.abort();
    }
    udp.remove(&flow);
}

/// Apply a splice operation to the gateway.
fn apply_op(gateway: &mut Gateway, op: IoOp) {
    match op {
        IoOp::Close { handle } => gateway.tcp_close(handle),
        IoOp::Reset { handle } => gateway.tcp_abort(handle),
    }
}

/// Whether a packet is structurally a candidate IPv4/UDP datagram.
///
/// [`Gateway::route`] performs the authoritative validation; this only avoids
/// cloning non-UDP packets on the tunnel's hot path.
fn is_ipv4_udp(packet: &[u8]) -> bool {
    packet.len() >= 20 && packet[0] >> 4 == 4 && packet[9] == 17
}

/// Bind a non-blocking datagram socket on `port` that may be shared by flows.
fn bind_flow_socket(port: u16) -> io::Result<std::net::UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    {
        // Best effort: kernels without SO_REUSEPORT can still serve one flow per
        // source port, and failing here would break UDP forwarding entirely.
        let _ = socket.set_reuse_port(true);
    }
    socket.bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port).into())?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

/// Read replies for one flow and feed them to the bridge loop.
fn spawn_reader(
    socket: Arc<UdpSocket>,
    replies: mpsc::Sender<UdpEvent>,
    flow: FlowId,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer = vec![0u8; MAX_DATAGRAM];
        loop {
            match socket.recv(&mut buffer).await {
                Ok(n) => {
                    let event = UdpEvent::Datagram {
                        flow,
                        payload: buffer[..n].to_vec(),
                    };
                    if replies.send(event).await.is_err() {
                        return;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                // A connected UDP socket surfaces ICMP port-unreachable as an
                // error. End this reader; the next outbound packet reopens the
                // flow's socket.
                Err(_) => {
                    let _ = replies.send(UdpEvent::Closed(flow)).await;
                    return;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    use super::*;
    use crate::api::{start_bridge, stats, status, stop_bridge};
    use crate::config::BridgeConfig;
    use crate::stats::{BridgeState, BridgeStatus, Stats};

    /// The lifecycle is process-global, so the tests that drive it must not run
    /// concurrently.
    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn config(endpoint: &str) -> BridgeConfig {
        BridgeConfig {
            private_key: STANDARD.encode([0x11; 32]),
            address: "10.7.0.2/24".to_string(),
            peer_public_key: STANDARD.encode([0x22; 32]),
            preshared_key: None,
            endpoint: endpoint.to_string(),
            mtu: 1420,
        }
    }

    /// TEST-NET-1: a syntactically valid endpoint that nothing answers on.
    fn unreachable_endpoint() -> &'static str {
        "192.0.2.1:51820"
    }

    /// A minimal valid inner IPv4/UDP packet, enough for `handle_outbound`.
    fn outbound_udp_packet() -> Vec<u8> {
        let mut packet = vec![0u8; 28];
        packet[0] = 0x45; // IPv4, 20-byte header
        packet[2..4].copy_from_slice(&28u16.to_be_bytes());
        packet[8] = 64;
        packet[9] = 17; // UDP
        packet[12..16].copy_from_slice(&[10, 10, 10, 1]);
        packet[16..20].copy_from_slice(&[1, 1, 1, 1]);
        packet[20..22].copy_from_slice(&53000u16.to_be_bytes());
        packet[22..24].copy_from_slice(&443u16.to_be_bytes());
        packet[24..26].copy_from_slice(&8u16.to_be_bytes());
        packet
    }

    #[tokio::test]
    async fn start_without_a_socket_fd_reports_connecting_and_stop_resets() {
        let _serial = TEST_LOCK.lock().await;
        let _ = stop_bridge().await;

        start_bridge(config(unreachable_endpoint()), None)
            .await
            .expect("the Linux (no fd) path starts");

        // Yield past several loop ticks so this reads what the loop publishes,
        // not the value `start_bridge` wrote before spawning it. The monitor
        // stays `Connecting` until the grace period elapses.
        tokio::time::sleep(TICK_INTERVAL * 3).await;
        assert_eq!(status().state, BridgeState::Connecting);
        assert_eq!(stats(), Stats::default(), "a fresh run starts at zero");

        // Starting twice is an error, not a second loop.
        assert!(
            start_bridge(config(unreachable_endpoint()), None)
                .await
                .is_err(),
            "a running bridge must reject a second start"
        );

        stop_bridge().await.expect("stop succeeds");
        assert_eq!(
            status(),
            BridgeStatus {
                state: BridgeState::Idle,
                error: None,
            }
        );
        assert_eq!(stats(), Stats::default(), "stopping zeroes the counters");
    }

    #[tokio::test]
    async fn the_loop_starts_a_handshake_without_any_inner_traffic() {
        let _serial = TEST_LOCK.lock().await;
        let _ = stop_bridge().await;

        // A stand-in WireGuard peer: it only needs to observe the first packet.
        let peer = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind the stand-in peer");
        let endpoint = peer.local_addr().expect("peer address");

        start_bridge(config(&endpoint.to_string()), None)
            .await
            .expect("start against the stand-in peer");

        let mut packet = [0u8; 2048];
        let (len, _) = tokio::time::timeout(Duration::from_secs(2), peer.recv_from(&mut packet))
            .await
            .expect("the loop initiates a handshake by itself")
            .expect("receive the handshake");
        assert_eq!(len, 148, "a WireGuard handshake initiation");
        assert_eq!(
            packet[..4],
            [1, 0, 0, 0],
            "message type 1 is a handshake initiation"
        );

        stop_bridge().await.expect("stop");
        assert!(!is_running());
    }

    /// A bridge that can never reach its endpoint must not sit in `Connecting`
    /// forever: the monitor's grace period has to run from `start`, which only
    /// happens if the loop calls `BridgeMonitor::start`.
    #[tokio::test]
    async fn a_never_connected_bridge_surfaces_error_after_the_grace() {
        let _serial = TEST_LOCK.lock().await;
        let _ = stop_bridge().await;

        start_bridge(config(unreachable_endpoint()), None)
            .await
            .expect("start");

        tokio::time::sleep(HANDSHAKE_GRACE + Duration::from_secs(1)).await;

        let status = status();
        assert_eq!(
            status.state,
            BridgeState::Error,
            "the never-connected path must surface an error after the grace"
        );
        let error = status.error.expect("an error carries a reason");
        assert!(
            error.contains("handshake"),
            "the reason explains the missing handshake: {error}"
        );
        assert!(
            is_running(),
            "the loop keeps running so it can recover when the peer answers"
        );

        stop_bridge().await.expect("stop");
    }

    #[tokio::test]
    async fn stopping_while_connecting_leaves_no_task_running() {
        let _serial = TEST_LOCK.lock().await;
        let _ = stop_bridge().await;

        start_bridge(config(unreachable_endpoint()), None)
            .await
            .expect("start");
        assert_eq!(status().state, BridgeState::Connecting);

        stop_bridge().await.expect("stop while still connecting");
        assert!(!is_running(), "the loop task is gone");

        // Nothing keeps ticking afterwards: the status and counters stay put.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(status().state, BridgeState::Idle);
        assert_eq!(stats(), Stats::default());

        // And the slot was released cleanly, so a fresh start works.
        start_bridge(config(unreachable_endpoint()), None)
            .await
            .expect("start again after stop");
        stop_bridge().await.expect("stop again");
        assert!(!is_running());
    }

    #[tokio::test]
    async fn releasing_a_flow_keeps_active_flows_honest() {
        let _serial = TEST_LOCK.lock().await;

        let counters = Arc::new(StatsCounters::new());
        let mut udp = UdpForwarder::with_counters(8, UDP_IDLE_TIMEOUT, Arc::clone(&counters));
        let outbound = udp
            .handle_outbound(&outbound_udp_packet())
            .expect("a valid packet is forwarded");

        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("bind"));
        let reader = tokio::spawn(async {});
        let mut flows = HashMap::new();
        flows.insert(
            outbound.flow,
            FlowSockets {
                socket,
                reader,
                last_seen: Instant::now(),
            },
        );
        assert_eq!(counters.snapshot(udp.len()).active_flows, 1);

        release_flow(&mut udp, &mut flows, outbound.flow);

        assert_eq!(udp.len(), 0, "the dropped flow stops counting immediately");
        assert_eq!(
            counters.snapshot(udp.len()).active_flows,
            0,
            "active_flows is honest without waiting for idle expiry"
        );
        assert!(flows.is_empty(), "the socket entry is gone too");
    }

    #[tokio::test]
    async fn a_bad_config_is_an_error_not_a_panic() {
        let _serial = TEST_LOCK.lock().await;
        let _ = stop_bridge().await;

        let mut cfg = config(unreachable_endpoint());
        cfg.private_key = "not base64!".to_string();

        let error = start_bridge(cfg, None)
            .await
            .expect_err("an invalid key must be reported");
        assert!(
            error.contains("private_key"),
            "the error names the bad field: {error}"
        );
        assert!(!is_running(), "a failed start leaves no task behind");
        assert_eq!(status().state, BridgeState::Idle);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_adopted_socket_fd_is_owned_by_rust() {
        use std::os::fd::IntoRawFd;

        let _serial = TEST_LOCK.lock().await;
        let _ = stop_bridge().await;

        // Ownership is handed over exactly as Android does it: the local socket
        // is consumed by `into_raw_fd`, and Rust closes the descriptor once.
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").expect("bind");
        let fd = socket.into_raw_fd();

        start_bridge(config(unreachable_endpoint()), Some(fd))
            .await
            .expect("the adopted-fd path starts");
        assert_eq!(status().state, BridgeState::Connecting);

        stop_bridge().await.expect("stop closes the adopted socket");
        assert!(!is_running());
    }

    #[tokio::test]
    async fn a_negative_socket_fd_is_rejected_not_a_panic() {
        let _serial = TEST_LOCK.lock().await;
        let _ = stop_bridge().await;

        assert!(
            start_bridge(config(unreachable_endpoint()), Some(-1))
                .await
                .is_err(),
            "an invalid descriptor is an error"
        );
        assert!(!is_running());
        assert_eq!(status().state, BridgeState::Idle);
    }

    #[tokio::test]
    async fn stopping_when_idle_is_safe() {
        let _serial = TEST_LOCK.lock().await;

        stop_bridge().await.expect("stop when never started");
        stop_bridge().await.expect("stop twice");
        assert_eq!(status().state, BridgeState::Idle);
        assert_eq!(stats(), Stats::default());
    }
}
