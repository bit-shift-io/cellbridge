//! Shared harness for the loopback integration tests.
//!
//! Every packet here takes the long way round. A client packet is encrypted by
//! the client-side [`Tunnel`], decrypted by the gateway-side [`Tunnel`], handed
//! to the [`Gateway`], and answered by the splice engine or the UDP forwarder
//! over a real loopback socket. The answer is then encrypted by the gateway-side
//! tunnel, decrypted by the client-side tunnel, and inspected as wire bytes.
//!
//! Nothing here reaches into the crate's internals: it is deliberately an
//! external harness, so it proves the public API is sufficient to drive the
//! complete data path.

#![allow(dead_code)] // each test binary uses a subset of the harness

use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant as StdInstant;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use boringtun::x25519::{PublicKey, StaticSecret};
use smoltcp::iface::SocketHandle;
use smoltcp::time::Instant;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use cellbridge::{
    BridgeConfig, BridgeMonitor, BridgeStatus, FlowId, Gateway, GatewayEvent, IoOp, LinkEvent,
    OutboundUdp, Routed, Stats, StatsCounters, TcpSpliceEngine, Tunnel, TunnelError, UdpForwarder,
    parse_udp,
};

pub const MTU: usize = 1420;
pub const GW_ADDR: Ipv4Addr = Ipv4Addr::new(10, 10, 10, 2);
pub const GW_PREFIX: u8 = 24;
pub const TCP_CLIENT_PORT: u16 = 53000;
pub const UDP_CLIENT_PORT: u16 = 53001;
pub const CLIENT_ISN: u32 = 0x1234_5678;

/// The router's masqueraded address as the simulated TCP client.
pub const TCP_CLIENT_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 10, 10, 1)), TCP_CLIENT_PORT);
/// The router's masqueraded address as the simulated UDP client.
pub const UDP_CLIENT_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 10, 10, 1)), UDP_CLIENT_PORT);

pub const FIN: u8 = 0x01;
pub const SYN: u8 = 0x02;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;

/// Upper bound on a drive loop, so a regression fails rather than hangs.
pub const DRIVE_TIMEOUT: Duration = Duration::from_secs(10);
/// The tunnel has no timer deadline (boringtun exposes none), so tick on a
/// fixed, short cadence.
pub const TICK_INTERVAL: Duration = Duration::from_millis(10);
/// Idle timeout for the UDP flow table in the harness.
pub const UDP_FLOW_IDLE: Duration = Duration::from_secs(30);
/// How many UDP flows the harness tracks at once.
pub const UDP_MAX_FLOWS: usize = 16;

// ---------------------------------------------------------------------------
// The two-tunnel wire.
// ---------------------------------------------------------------------------

fn config(private: [u8; 32], peer_public: [u8; 32]) -> BridgeConfig {
    BridgeConfig {
        private_key: STANDARD.encode(private),
        address: "10.10.10.2/24".to_string(),
        peer_public_key: STANDARD.encode(peer_public),
        preshared_key: None,
        endpoint: "192.0.2.1:51820".to_string(),
        mtu: MTU as u16,
    }
}

/// Inner IP packets, as they cross the tunnel.
pub type Packets = Vec<Vec<u8>>;

/// Two WireGuard tunnels facing each other, with encrypted datagrams waiting to
/// cross in each direction.
pub struct Wire {
    /// The client-facing end. Packets injected here appear at the gateway.
    pub client_side: Tunnel,
    /// The gateway-facing end. Packets injected here appear at the client.
    pub gateway_side: Tunnel,
    /// Encrypted datagrams waiting to be decrypted by `client_side`.
    to_client: Vec<Vec<u8>>,
    /// Encrypted datagrams waiting to be decrypted by `gateway_side`.
    to_gateway: Vec<Vec<u8>>,
}

impl Wire {
    pub fn new() -> Wire {
        let client_secret = [0x11; 32];
        let gateway_secret = [0x22; 32];
        let client_public = PublicKey::from(&StaticSecret::from(client_secret)).to_bytes();
        let gateway_public = PublicKey::from(&StaticSecret::from(gateway_secret)).to_bytes();

        Wire {
            client_side: Tunnel::new(&config(client_secret, gateway_public))
                .expect("client tunnel"),
            gateway_side: Tunnel::new(&config(gateway_secret, client_public))
                .expect("gateway tunnel"),
            to_client: Vec::new(),
            to_gateway: Vec::new(),
        }
    }

    /// Encapsulate an inner packet on the client side; it is now on its way to
    /// the gateway side.
    pub fn send_from_client(&mut self, packet: &[u8]) -> Result<(), TunnelError> {
        let out = self.client_side.encapsulate(packet)?;
        assert!(
            out.to_tunnel.is_empty(),
            "encapsulating never yields local plaintext"
        );
        self.to_gateway.extend(out.to_network);
        Ok(())
    }

    /// Encapsulate an inner packet on the gateway side; it is now on its way
    /// back to the client.
    pub fn send_from_gateway(&mut self, packet: &[u8]) -> Result<(), TunnelError> {
        let out = self.gateway_side.encapsulate(packet)?;
        assert!(
            out.to_tunnel.is_empty(),
            "encapsulating never yields local plaintext"
        );
        self.to_client.extend(out.to_network);
        Ok(())
    }

    /// Run every waiting encrypted datagram through the receiving tunnel.
    ///
    /// Returns the inner packets decrypted by the gateway side (client → stack)
    /// and by the client side (stack → client).
    pub fn deliver(&mut self) -> Result<(Packets, Packets), TunnelError> {
        let mut to_gateway = Vec::new();
        let mut to_client = Vec::new();

        let mut replies_to_client = Vec::new();
        for datagram in std::mem::take(&mut self.to_gateway) {
            let out = self.gateway_side.handle_udp(&datagram)?;
            replies_to_client.extend(out.to_network);
            to_gateway.extend(out.to_tunnel);
        }
        self.to_client.extend(replies_to_client);

        let mut replies_to_gateway = Vec::new();
        for datagram in std::mem::take(&mut self.to_client) {
            let out = self.client_side.handle_udp(&datagram)?;
            replies_to_gateway.extend(out.to_network);
            to_client.extend(out.to_tunnel);
        }
        self.to_gateway.extend(replies_to_gateway);

        Ok((to_gateway, to_client))
    }

    pub fn tick(&mut self) -> Result<(), TunnelError> {
        let client = self.client_side.tick()?;
        self.to_gateway.extend(client.to_network);
        let gateway = self.gateway_side.tick()?;
        self.to_client.extend(gateway.to_network);
        Ok(())
    }

    /// Complete the WireGuard handshake by bouncing datagrams until both sides
    /// report a live session.
    pub async fn connect(&mut self) {
        // boringtun starts the handshake lazily: encapsulating queues the packet
        // and emits the initiation.
        self.send_from_client(&probe_packet())
            .expect("start handshake");

        let deadline = StdInstant::now() + Duration::from_secs(5);
        while !(self.client_side.is_connected() && self.gateway_side.is_connected()) {
            assert!(
                StdInstant::now() < deadline,
                "WireGuard handshake did not complete"
            );
            self.tick().expect("handshake tick");
            self.deliver().expect("handshake delivery");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // The probe was queued behind the handshake; flush and discard it so it
        // cannot show up as stray traffic in the test proper.
        for _ in 0..8 {
            self.tick().expect("flush tick");
            let (to_gateway, to_client) = self.deliver().expect("flush delivery");
            assert!(
                to_client.is_empty(),
                "no client traffic during handshake flush"
            );
            drop(to_gateway);
        }
    }
}

/// A minimal valid IPv4 packet used only to trigger and complete the handshake.
fn probe_packet() -> Vec<u8> {
    let mut pkt = vec![0u8; 20];
    pkt[0] = 0x45;
    pkt[2..4].copy_from_slice(&20u16.to_be_bytes());
    pkt[8] = 64;
    pkt[9] = 59; // no next header: nothing in the stack will answer it
    pkt[12..16].copy_from_slice(&[10, 10, 10, 1]);
    pkt[16..20].copy_from_slice(&[192, 0, 2, 99]);
    pkt
}

// ---------------------------------------------------------------------------
// The simulated client's TCP stack.
// ---------------------------------------------------------------------------

/// 16-bit one's-complement checksum, as used by IPv4 and TCP.
fn checksum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let (words, remainder) = bytes.as_chunks::<2>();
    for word in words {
        sum += u32::from(u16::from_be_bytes(*word));
    }
    if let [last] = remainder {
        sum += u32::from(*last) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// A hand-built IPv4/TCP packet with valid checksums.
pub fn tcp_packet(
    src: SocketAddr,
    dst: SocketAddr,
    seq: u32,
    ack: Option<u32>,
    flags: u8,
    payload: &[u8],
) -> Vec<u8> {
    let (IpAddr::V4(src_ip), IpAddr::V4(dst_ip)) = (src.ip(), dst.ip()) else {
        panic!("the test builds IPv4 packets only");
    };
    let src_ip = src_ip.octets();
    let dst_ip = dst_ip.octets();

    let tcp_len = 20 + payload.len();
    let total_len = 20 + tcp_len;
    let mut pkt = vec![0u8; total_len];

    pkt[0] = 0x45;
    pkt[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
    pkt[8] = 64;
    pkt[9] = 6;
    pkt[12..16].copy_from_slice(&src_ip);
    pkt[16..20].copy_from_slice(&dst_ip);
    let ip_checksum = checksum(&pkt[..20]);
    pkt[10..12].copy_from_slice(&ip_checksum.to_be_bytes());

    let tcp = &mut pkt[20..];
    tcp[0..2].copy_from_slice(&src.port().to_be_bytes());
    tcp[2..4].copy_from_slice(&dst.port().to_be_bytes());
    tcp[4..8].copy_from_slice(&seq.to_be_bytes());
    if let Some(ack) = ack {
        tcp[8..12].copy_from_slice(&ack.to_be_bytes());
    }
    tcp[12] = 5 << 4;
    tcp[13] = flags;
    tcp[14..16].copy_from_slice(&65535u16.to_be_bytes());
    tcp[20..].copy_from_slice(payload);

    let mut pseudo = Vec::with_capacity(12 + tcp_len);
    pseudo.extend_from_slice(&src_ip);
    pseudo.extend_from_slice(&dst_ip);
    pseudo.push(0);
    pseudo.push(6);
    pseudo.extend_from_slice(&(tcp_len as u16).to_be_bytes());
    pseudo.extend_from_slice(&pkt[20..]);
    let tcp_checksum = checksum(&pseudo);
    pkt[36..38].copy_from_slice(&tcp_checksum.to_be_bytes());

    pkt
}

/// A decoded IPv4/TCP packet, as observed on the wire.
#[derive(Debug)]
pub struct TcpSegment {
    pub src: SocketAddr,
    pub dst: SocketAddr,
    pub seq: u32,
    pub ack: Option<u32>,
    pub flags: u8,
    pub payload: Vec<u8>,
}

pub fn parse_tcp(pkt: &[u8]) -> Option<TcpSegment> {
    if pkt.len() < 20 || pkt[0] >> 4 != 4 {
        return None;
    }
    let ip_header_len = usize::from(pkt[0] & 0x0f) * 4;
    if ip_header_len < 20 || pkt.len() < ip_header_len + 20 || pkt[9] != 6 {
        return None;
    }

    let tcp = &pkt[ip_header_len..];
    let tcp_header_len = usize::from(tcp[12] >> 4) * 4;
    if tcp_header_len < 20 || tcp.len() < tcp_header_len {
        return None;
    }

    let src = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15])),
        u16::from_be_bytes([tcp[0], tcp[1]]),
    );
    let dst = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(pkt[16], pkt[17], pkt[18], pkt[19])),
        u16::from_be_bytes([tcp[2], tcp[3]]),
    );
    let flags = tcp[13];
    Some(TcpSegment {
        src,
        dst,
        seq: u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]),
        ack: (flags & ACK != 0).then(|| u32::from_be_bytes([tcp[8], tcp[9], tcp[10], tcp[11]])),
        flags,
        payload: tcp[tcp_header_len..].to_vec(),
    })
}

/// The client half of the TCP connection, driven only by wire packets.
pub struct TcpClient {
    client: SocketAddr,
    dst: SocketAddr,
    isn: u32,
    next_client_seq: u32,
    expected_server_seq: u32,
    established: bool,
    fin_sent: bool,
    fin_acked: bool,
    stay_open: bool,
    received: Vec<u8>,
}

impl TcpClient {
    pub fn new(client: SocketAddr, dst: SocketAddr) -> TcpClient {
        TcpClient {
            client,
            dst,
            isn: CLIENT_ISN,
            next_client_seq: CLIENT_ISN.wrapping_add(1),
            expected_server_seq: 0,
            established: false,
            fin_sent: false,
            fin_acked: false,
            stay_open: false,
            received: Vec::new(),
        }
    }

    /// Keep the client's write half open after the echo arrives.
    ///
    /// By default the client half-closes as soon as it has seen the echoed
    /// payload, which lets the splice finish. A test that needs a long-lived
    /// flow — to watch it being torn down, say — calls this instead, so the
    /// connection is still open when the test wants it to be.
    pub fn keep_open(&mut self) {
        self.stay_open = true;
    }

    pub fn syn(&self) -> Vec<u8> {
        tcp_packet(self.client, self.dst, self.isn, None, SYN, &[])
    }

    /// Half-close the client's write side, returning the FIN to put on the
    /// wire.
    ///
    /// Used by a test whose client [`Self::keep_open`] was kept live for a
    /// while: the connection is then finished deliberately rather than as soon
    /// as the echo arrived.
    pub fn half_close(&mut self) -> Vec<u8> {
        self.fin_sent = true;
        let pkt = tcp_packet(
            self.client,
            self.dst,
            self.next_client_seq,
            Some(self.expected_server_seq),
            FIN | ACK,
            &[],
        );
        self.next_client_seq = self.next_client_seq.wrapping_add(1);
        pkt
    }

    /// Whether the payload this client sent has come back.
    pub fn echoed(&self) -> bool {
        self.received.windows(5).any(|w| w == b"hello")
    }

    /// Whether the connection ran to completion: the echo arrived and the
    /// client's half-close was acknowledged.
    pub fn finished(&self) -> bool {
        self.fin_acked && self.echoed()
    }

    /// React to one gateway → client segment, returning the client packets that
    /// should be put back on the wire.
    pub fn on_segment(&mut self, segment: &TcpSegment) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if segment.dst != self.client || segment.src != self.dst {
            return out;
        }

        if !self.established && (segment.flags & (SYN | ACK)) == (SYN | ACK) {
            assert_eq!(
                segment.ack,
                Some(self.isn.wrapping_add(1)),
                "the SYN-ACK acknowledges the client's ISN"
            );
            self.established = true;
            self.expected_server_seq = segment.seq.wrapping_add(1);
            // Acknowledge the SYN and send the request in one segment.
            out.push(tcp_packet(
                self.client,
                self.dst,
                self.next_client_seq,
                Some(self.expected_server_seq),
                PSH | ACK,
                b"hello",
            ));
            self.next_client_seq = self.next_client_seq.wrapping_add(5);
        }

        if self.established && !segment.payload.is_empty() {
            let end = segment.seq.wrapping_add(segment.payload.len() as u32);
            if end > self.expected_server_seq {
                // Skip any bytes a retransmission repeats.
                let skip = usize::try_from(self.expected_server_seq.saturating_sub(segment.seq))
                    .unwrap_or(0);
                if skip < segment.payload.len() {
                    self.received.extend_from_slice(&segment.payload[skip..]);
                    self.expected_server_seq = end;
                }
            }
            if !self.stay_open && !self.fin_sent && self.echoed() {
                // The echo has arrived, so half-close the client -> gateway side.
                out.push(tcp_packet(
                    self.client,
                    self.dst,
                    self.next_client_seq,
                    Some(self.expected_server_seq),
                    FIN | ACK,
                    &[],
                ));
                self.next_client_seq = self.next_client_seq.wrapping_add(1);
                self.fin_sent = true;
            }
        }

        if self.established && (segment.flags & FIN) != 0 && !self.fin_acked {
            let ack = segment
                .seq
                .wrapping_add(segment.payload.len() as u32)
                .wrapping_add(1);
            out.push(tcp_packet(
                self.client,
                self.dst,
                self.next_client_seq,
                Some(ack),
                ACK,
                &[],
            ));
            self.fin_acked = true;
        }

        out
    }
}

/// What the simulated client has seen coming back from the gateway.
#[derive(Debug, Default)]
pub struct ClientSeen {
    /// The UDP reply payload, once one has arrived.
    pub udp_reply: Option<Vec<u8>>,
    /// The reply's source, as parsed from the inner packet.
    pub udp_reply_source: Option<SocketAddr>,
    /// The reply's destination.
    pub udp_reply_dst: Option<SocketAddr>,
}

/// React to one batch of packets that reached the client side: feed TCP
/// segments to the simulated client and record any UDP reply.
pub fn client_react(
    wire: &mut Wire,
    tcp_client: &mut TcpClient,
    tcp_client_addr: SocketAddr,
    udp_client_addr: SocketAddr,
    to_client: &Packets,
    seen: &mut ClientSeen,
) {
    for packet in to_client {
        if packet.get(9) == Some(&6) {
            if let Some(segment) = parse_tcp(packet)
                && segment.dst == tcp_client_addr
            {
                for out in tcp_client.on_segment(&segment) {
                    wire.send_from_client(&out)
                        .expect("encapsulate a client packet");
                }
            }
        } else if let Some(parsed) = parse_udp(packet)
            && parsed.dst == udp_client_addr
        {
            seen.udp_reply_source = Some(parsed.src);
            seen.udp_reply_dst = Some(parsed.dst);
            seen.udp_reply = Some(parsed.payload);
        }
    }
}

// ---------------------------------------------------------------------------
// Real servers on loopback.
// ---------------------------------------------------------------------------

pub async fn tcp_echo_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the TCP echo server");
    let addr = listener.local_addr().expect("TCP echo address");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener
            .accept()
            .await
            .expect("accept the spliced connection");
        let mut buf = [0u8; 256];
        loop {
            let n = stream.read(&mut buf).await.expect("read from the splice");
            if n == 0 {
                break;
            }
            stream.write_all(&buf[..n]).await.expect("echo the payload");
        }
        stream.shutdown().await.expect("half-close after EOF");
    });
    (addr, task)
}

pub async fn udp_echo_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind the UDP echo server");
    let addr = socket.local_addr().expect("UDP echo address");
    let task = tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        let (n, peer) = socket
            .recv_from(&mut buf)
            .await
            .expect("receive one datagram");
        socket
            .send_to(b"pong", peer)
            .await
            .expect("echo the datagram");
        // The payload itself is checked by the client side; receiving it here is
        // enough to prove the outbound half of the path.
        assert_eq!(&buf[..n], b"ping");
    });
    (addr, task)
}

/// Apply the splice engine's terminal operations to the gateway.
pub fn apply(gateway: &mut Gateway, ops: Vec<IoOp>) {
    for op in ops {
        match op {
            IoOp::Close { handle } => gateway.tcp_close(handle),
            IoOp::Reset { handle } => gateway.tcp_abort(handle),
        }
    }
}

/// Send a UDP payload over a real socket, as the bridge does for a flow it has
/// just decoded.
pub async fn send_udp(socket: &tokio::net::UdpSocket, out: &OutboundUdp) {
    socket
        .send_to(&out.payload, out.dst)
        .await
        .expect("send the UDP payload over a real socket");
}

// ---------------------------------------------------------------------------
// The bridge: gateway, splice engine, UDP forwarder and link state machine.
// ---------------------------------------------------------------------------

/// The default link grace period the harness runs with.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(1);

/// The gateway half of the bridge loop, over the two-tunnel wire.
///
/// This is the harness's stand-in for the production loop: [`Self::step`] does
/// one iteration of the data path (deliver, route, poll, pump, encapsulate) and
/// reports the link state, and [`Self::tear_down_all`] is the tunnel-loss path.
pub struct Bridge {
    /// The two-tunnel wire.
    pub wire: Wire,
    /// The userspace IPv4 gateway.
    pub gateway: Gateway,
    /// The TCP splice engine, counting into `counters`.
    pub splice: TcpSpliceEngine,
    /// The UDP forwarder, counting into `counters`.
    pub forwarder: UdpForwarder,
    /// The byte counters both data paths record into.
    pub counters: Arc<StatsCounters>,
    /// The link state machine the loop drives.
    pub monitor: BridgeMonitor,
    /// The real socket UDP payloads leave over.
    pub udp_socket: tokio::net::UdpSocket,
    /// Handles the splice engine is still using.
    pub handles: Vec<SocketHandle>,
    /// Whether a decrypted packet was UDP, so `route` demultiplexed it.
    pub routed_udp: bool,
    /// The most recent UDP flow seen outbound.
    pub udp_flow: Option<FlowId>,
    /// Every `(client, dst)` pair the gateway accepted.
    pub accepted: Vec<(SocketAddr, SocketAddr)>,
    /// The inner packets the last [`Self::tear_down_all`] handed to the tunnel.
    pub torn_down_packets: Packets,
    /// The UDP flows the last [`Self::tear_down_all`] dropped.
    pub torn_down_udp: Vec<FlowId>,
}

impl Bridge {
    pub async fn new() -> Bridge {
        Bridge::new_with_grace(DEFAULT_GRACE).await
    }

    /// As [`Self::new`], with an explicit link grace period.
    pub async fn new_with_grace(grace: Duration) -> Bridge {
        let counters = Arc::new(StatsCounters::new());
        let mut monitor = BridgeMonitor::new(grace);
        // The bridge has been started: `Connecting` until the handshake
        // completes, never `Connected` optimistically.
        monitor.start();
        Bridge {
            wire: Wire::new(),
            gateway: Gateway::new(GW_ADDR, GW_PREFIX, MTU),
            splice: TcpSpliceEngine::with_counters(Arc::clone(&counters)),
            forwarder: UdpForwarder::with_counters(
                UDP_MAX_FLOWS,
                UDP_FLOW_IDLE,
                Arc::clone(&counters),
            ),
            counters,
            monitor,
            udp_socket: tokio::net::UdpSocket::bind("127.0.0.1:0")
                .await
                .expect("bind the UDP client socket"),
            handles: Vec::new(),
            routed_udp: false,
            udp_flow: None,
            accepted: Vec::new(),
            torn_down_packets: Vec::new(),
            torn_down_udp: Vec::new(),
        }
    }

    /// The bridge's counters plus the live flow count, taken from the
    /// structures that own the flows.
    pub fn stats(&self) -> Stats {
        self.counters
            .snapshot(self.splice.flow_count() + self.forwarder.len())
    }

    /// The link's current state and error.
    pub fn status(&self) -> BridgeStatus {
        self.monitor.status()
    }

    /// One iteration of the gateway half of the loop.
    ///
    /// Returns the inner packets that reached the client side, for the test to
    /// answer, and what this iteration did to the link state.
    pub async fn step(&mut self) -> Result<(Packets, LinkEvent), TunnelError> {
        let (to_gateway, to_client) = self.wire.deliver()?;

        for packet in to_gateway {
            match self.gateway.route(packet.clone()) {
                Routed::Udp => {
                    self.routed_udp = true;
                    if let Some(out) = self.forwarder.handle_outbound(&packet) {
                        self.udp_flow = Some(out.flow);
                        send_udp(&self.udp_socket, &out).await;
                    }
                }
                Routed::Tcp => {}
                Routed::Dropped => panic!("gateway dropped a packet that crossed the tunnel"),
            }
        }

        for event in self.gateway.poll(Instant::now()) {
            let GatewayEvent::TcpAccepted {
                handle,
                client,
                dst,
            } = event;
            self.accepted.push((client, dst));
            self.splice.on_accept(handle, client, dst);
            self.handles.push(handle);
        }

        // Reactor half: pull ready peer bytes before pumping, so this path is
        // genuinely exercised rather than bypassed by `pump`'s own read.
        let handles = self.handles.clone();
        for handle in handles {
            apply(&mut self.gateway, self.splice.on_readable(handle));
        }
        let ops = self.splice.pump(&mut self.gateway);
        apply(&mut self.gateway, ops);
        self.gateway.poll(Instant::now());

        for packet in self.gateway.take_tx() {
            self.wire.send_from_gateway(&packet)?;
        }
        for handle in self.splice.closed_handles() {
            self.handles.retain(|h| *h != handle);
            self.gateway.tcp_remove(handle);
        }

        let mut buf = vec![0u8; MTU + 64];
        loop {
            match self.udp_socket.try_recv_from(&mut buf) {
                Ok((n, _from)) => {
                    let flow = self.udp_flow.expect("a UDP reply needs a tracked flow");
                    if let Some(reply) =
                        self.forwarder.handle_reply_within_mtu(flow, &buf[..n], MTU)
                    {
                        self.wire.send_from_gateway(&reply)?;
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => panic!("UDP receive failed: {err}"),
            }
        }

        let event = self
            .monitor
            .observe(self.wire.gateway_side.is_connected(), StdInstant::now());
        Ok((to_client, event))
    }

    /// Advance the tunnel timers.
    pub fn tick(&mut self) -> Result<(), TunnelError> {
        self.wire.tick()
    }

    /// Tear every flow down — what the loop does when the link is lost.
    ///
    /// Every spliced connection is reset (a graceful close would look like a
    /// clean end of response), the UDP table is emptied, and the gateway
    /// sockets the splices held are freed. Returns the handles the splice
    /// engine reported through `closed_handles`; the inner packets carrying the
    /// resets are kept in [`Self::torn_down_packets`] and the UDP flows that
    /// were dropped in [`Self::torn_down_udp`], for the test to inspect.
    pub fn tear_down_all(&mut self) -> Vec<SocketHandle> {
        let ops = self.splice.tear_down_all();
        self.torn_down_udp = self.forwarder.tear_down_all();

        apply(&mut self.gateway, ops);
        self.gateway.poll(Instant::now());
        self.torn_down_packets = self.gateway.take_tx();

        let handles = self.splice.closed_handles();
        for handle in &handles {
            self.gateway.tcp_remove(*handle);
        }
        self.handles.clear();

        // The resets still go to the wire: the tunnel may be able to carry them
        // once it re-handshakes, and a client that is still there must not be
        // left guessing.
        for packet in &self.torn_down_packets {
            let _ = self.wire.send_from_gateway(packet);
        }

        handles
    }
}
