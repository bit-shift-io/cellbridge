//! The userspace IPv4 gateway.
//!
//! Decrypted inner IP packets arrive from the WireGuard tunnel addressed to
//! *real public IPs* (the router masquerades only the source), so the gateway
//! runs an [`Interface`] in **any-IP** mode: it accepts connections to whatever
//! destination the client asked for. Its own address is only used as the source
//! of packets the stack originates itself (ICMP echo replies).

use std::collections::HashMap;
use std::mem;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::{icmp, tcp};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint};

use crate::device::QueueDevice;

/// Receive/transmit buffer size for each TCP socket.
const TCP_BUFFER_SIZE: usize = 8192;

/// Upper bound on the idle listening sockets the gateway keeps.
///
/// smoltcp needs one listening socket per destination port, and a socket holds
/// ~16 KiB of buffers, so the pool has to be bounded: past this many idle
/// listeners the least recently used one is closed to make room for the port
/// that needs it. Nothing is lost by reaping — a listener is recreated on
/// demand, so the only cost is the buffers. This is a steady-state invariant,
/// enforced again after every [`Gateway::poll`] so a batch of more than this
/// many ports cannot leave the pool sitting above it.
const MAX_IDLE_LISTENERS: usize = 64;

/// The IPv4 protocol number for UDP.
const UDP_PROTOCOL: u8 = 17;

/// How [`Gateway::route`] dispatched an inbound packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routed {
    /// A non-UDP IPv4 packet was queued for smoltcp (TCP, ICMP, ...).
    Tcp,
    /// A UDP packet was diverted from smoltcp for the caller to hand to
    /// [`crate::UdpForwarder`].
    Udp,
    /// The packet was malformed or not IPv4 and was discarded.
    Dropped,
}

/// Something the gateway wants the caller to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayEvent {
    /// A TCP connection was accepted from `client` to `dst`.
    TcpAccepted {
        /// The smoltcp handle of the connection socket.
        handle: SocketHandle,
        /// The remote (client) endpoint.
        client: SocketAddr,
        /// The local endpoint — the real destination the client dialled.
        dst: SocketAddr,
    },
}

/// An any-IP smoltcp stack that terminates TCP to arbitrary destinations.
///
/// smoltcp can only listen on a *concrete* port — `accepts()` matches the
/// destination port exactly, and `listen()` rejects port zero — so the gateway
/// keeps one idle listening socket per destination port it has seen a SYN for.
/// When one accepts, it becomes the connection and a fresh listener is created
/// the next time a SYN for that port arrives.
///
/// The idle pool is bounded: creating one for a new port first closes the least
/// recently used one, and once a poll has drained the batch of SYNs that
/// prompted the growth the pool is trimmed back to [`MAX_IDLE_LISTENERS`]. A
/// stream of SYNs to fresh ports — a scan, or a SYN the stack drops — therefore
/// cannot grow the socket set without bound.
pub struct Gateway {
    device: QueueDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
    /// Idle listening sockets not yet connected.
    listeners: Vec<Listener>,
    /// Destination ports of pure SYNs seen since the last poll, so a listener
    /// is available for each of them before the stack runs.
    pending_syns: Vec<u16>,
}

/// One idle listening socket, and when it last absorbed a SYN.
struct Listener {
    /// The destination port it listens on.
    port: u16,
    /// The smoltcp socket, created in the `Listen` state.
    handle: SocketHandle,
    /// When this listener last absorbed a SYN, for reaping the least recently
    /// used one once the pool is full.
    last_used: Instant,
}

impl Gateway {
    /// Build a gateway whose interface owns `address/prefix_len` and whose
    /// device advertises `mtu` as the IP MTU.
    pub fn new(address: Ipv4Addr, prefix_len: u8, mtu: usize) -> Gateway {
        let mut device = QueueDevice::new(mtu);

        let config = Config::new(HardwareAddress::Ip);
        let mut iface = Interface::new(config, &mut device, Instant::ZERO);
        iface.update_ip_addrs(|addrs| {
            let octets = address.octets();
            addrs
                .push(IpCidr::new(
                    IpAddress::v4(octets[0], octets[1], octets[2], octets[3]),
                    prefix_len,
                ))
                .expect("one address always fits");
        });
        iface.set_any_ip(true);

        let mut sockets = SocketSet::new(vec![]);
        // ICMP echo requests to the interface address are answered by smoltcp's
        // `auto-icmp-echo-reply` feature, so this socket is unbound and inert;
        // it is registered per the gateway design and is where ICMP error
        // routing (e.g. per-flow "port unreachable" for the splices) will hang.
        let _icmp = sockets.add(icmp::Socket::new(
            icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]),
            icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]),
        ));

        Gateway {
            device,
            iface,
            sockets,
            listeners: Vec::new(),
            pending_syns: Vec::new(),
        }
    }

    /// Demultiplex an inbound inner IP packet.
    ///
    /// UDP is diverted away from smoltcp: one socket per UDP flow cannot
    /// preserve the client's source port, so the caller hands those packets to
    /// [`crate::UdpForwarder`] instead. Every other well-formed IPv4 packet is
    /// queued for the stack exactly as [`Gateway::push_rx`] does. Malformed and
    /// non-IPv4 packets are dropped.
    ///
    /// The packet is consumed, so a caller that needs the bytes after a
    /// [`Routed::Udp`] result (to pass them to the forwarder) must keep its own
    /// copy.
    pub fn route(&mut self, pkt: Vec<u8>) -> Routed {
        match ipv4_protocol(&pkt) {
            None => Routed::Dropped,
            Some(UDP_PROTOCOL) => Routed::Udp,
            Some(_) => {
                self.push_rx(pkt);
                Routed::Tcp
            }
        }
    }

    /// Queue an inbound IP packet.
    ///
    /// This is the raw queueing primitive; [`Gateway::route`] is the demuxing
    /// entry point the bridge loop uses. Both share this path, so a packet
    /// accepted by `route` is queued identically.
    pub fn push_rx(&mut self, pkt: Vec<u8>) {
        if let Some(port) = syn_dst_port(&pkt) {
            self.pending_syns.push(port);
        }
        self.device.push_rx(pkt);
    }

    /// Advance the stack and report newly accepted TCP connections.
    pub fn poll(&mut self, now: Instant) -> Vec<GatewayEvent> {
        self.open_pending_listeners(now);
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        let events = self.drain_accepts();
        // Every SYN this batch queued has now been consumed or dropped, so any
        // listener still idle can be reaped. The mid-batch reaping spares
        // listeners whose SYN is still queued, which lets one oversized batch
        // push the pool above the cap; trimming here restores the bound so the
        // high-water mark does not persist.
        self.trim_idle_listeners();
        events
    }

    /// Make sure every pending SYN has an idle listener to match against.
    fn open_pending_listeners(&mut self, now: Instant) {
        if self.pending_syns.is_empty() {
            return;
        }

        let pending = mem::take(&mut self.pending_syns);
        let mut idle: HashMap<u16, usize> = HashMap::new();
        for listener in &self.listeners {
            *idle.entry(listener.port).or_insert(0) += 1;
        }

        for port in &pending {
            match idle.get_mut(port) {
                // An idle listener will absorb this SYN; consume it.
                Some(remaining) if *remaining > 0 => {
                    *remaining -= 1;
                    self.note_listener_used(*port, now);
                }
                _ => self.add_listener(*port, &pending, now),
            }
        }
    }

    /// Record that the listener for `port` is about to absorb a SYN.
    ///
    /// The least recently used matching listener is the one kept: several
    /// listeners can share a port when a batch carries more SYNs for it than
    /// there were idle sockets, and refreshing any of them is enough to keep
    /// the reaping honest.
    fn note_listener_used(&mut self, port: u16, now: Instant) {
        let used = self
            .listeners
            .iter_mut()
            .filter(|listener| listener.port == port)
            .min_by_key(|listener| listener.last_used);
        if let Some(listener) = used {
            listener.last_used = now;
        }
    }

    /// Add a fresh socket listening on `port` and record it as idle.
    fn add_listener(&mut self, port: u16, pending: &[u16], now: Instant) {
        self.reap_idle_listener(pending);

        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]),
            tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]),
        );
        socket
            .listen(port)
            .expect("a fresh socket accepts a nonzero listen port");
        let handle = self.sockets.add(socket);
        self.listeners.push(Listener {
            port,
            handle,
            last_used: now,
        });
    }

    /// Close the least recently used idle listener, if the pool is at its cap.
    ///
    /// A listener whose port is in `pending` is spared: a SYN for it is already
    /// queued for the stack, and reaping its listener would drop that SYN. If
    /// every idle listener is spoken for, the pool is allowed to sit above the
    /// cap for the rest of this batch; [`Self::trim_idle_listeners`] brings it
    /// back once the batch has been through the stack.
    fn reap_idle_listener(&mut self, pending: &[u16]) {
        if self.listeners.len() < MAX_IDLE_LISTENERS {
            return;
        }
        let victim = self
            .listeners
            .iter()
            .enumerate()
            .filter(|(_, listener)| !pending.contains(&listener.port))
            .min_by_key(|(_, listener)| listener.last_used)
            .map(|(index, _)| index);
        if let Some(index) = victim {
            let listener = self.listeners.remove(index);
            self.sockets.remove(listener.handle);
        }
    }

    /// Close idle listeners until the pool is back at [`MAX_IDLE_LISTENERS`].
    ///
    /// [`Self::open_pending_listeners`] spares a listener whose SYN is still
    /// queued, so a single batch of more than the cap of distinct ports can
    /// grow the pool past it. By the time this runs the batch has been through
    /// the stack — every SYN in it has either accepted a connection (and left
    /// the pool) or been dropped — so anything still listening is idle and fair
    /// game, least recently used first.
    fn trim_idle_listeners(&mut self) {
        while self.listeners.len() > MAX_IDLE_LISTENERS {
            let victim = self
                .listeners
                .iter()
                .enumerate()
                .min_by_key(|(_, listener)| listener.last_used)
                .map(|(index, _)| index)
                .expect("the pool is nonempty while it is above the cap");
            let listener = self.listeners.remove(victim);
            self.sockets.remove(listener.handle);
        }
    }

    /// Turn listeners that have moved out of the Listen state into events,
    /// dropping the ones that closed without connecting.
    fn drain_accepts(&mut self) -> Vec<GatewayEvent> {
        let mut events = Vec::new();
        let mut idle = Vec::new();

        for listener in mem::take(&mut self.listeners) {
            let Listener {
                port,
                handle,
                last_used,
            } = listener;
            match self.sockets.get::<tcp::Socket>(handle).state() {
                tcp::State::Listen => idle.push(Listener {
                    port,
                    handle,
                    last_used,
                }),
                tcp::State::Closed => {
                    self.sockets.remove(handle);
                }
                _ => {
                    let socket = self.sockets.get::<tcp::Socket>(handle);
                    if let (Some(client), Some(dst)) = (
                        socket.remote_endpoint().map(socket_addr),
                        socket.local_endpoint().map(socket_addr),
                    ) {
                        events.push(GatewayEvent::TcpAccepted {
                            handle,
                            client,
                            dst,
                        });
                    }
                }
            }
        }

        self.listeners = idle;
        events
    }

    /// Remove and return every outbound IP packet the stack has produced.
    pub fn take_tx(&mut self) -> Vec<Vec<u8>> {
        self.device.drain_tx()
    }

    /// Advisory wait before the next call to [`Gateway::poll`].
    ///
    /// Takes `&mut self` because smoltcp's `poll_delay` caches the current time
    /// on the interface.
    pub fn poll_delay(&mut self, now: Instant) -> Option<Duration> {
        self.iface.poll_delay(now, &self.sockets)
    }

    /// Read bytes received on a connection socket.
    pub fn tcp_recv(
        &mut self,
        handle: SocketHandle,
        buf: &mut [u8],
    ) -> Result<usize, tcp::RecvError> {
        self.sockets.get_mut::<tcp::Socket>(handle).recv_slice(buf)
    }

    /// Queue bytes to send on a connection socket.
    pub fn tcp_send(&mut self, handle: SocketHandle, data: &[u8]) -> Result<usize, tcp::SendError> {
        self.sockets.get_mut::<tcp::Socket>(handle).send_slice(data)
    }

    /// The current smoltcp state of a connection socket.
    ///
    /// A connection is reported as accepted as soon as its listener leaves the
    /// `Listen` state, which happens on the SYN — before the handshake is
    /// complete. A splice engine that dials the destination immediately can
    /// therefore outrun the client's final ACK and must be able to tell a
    /// half-open socket from a reset one.
    ///
    /// # Panics
    ///
    /// Panics if `handle` does not refer to a socket in the set.
    pub fn tcp_state(&self, handle: SocketHandle) -> tcp::State {
        self.sockets.get::<tcp::Socket>(handle).state()
    }

    /// Begin a graceful close of a connection socket.
    pub fn tcp_close(&mut self, handle: SocketHandle) {
        self.sockets.get_mut::<tcp::Socket>(handle).close();
    }

    /// Abort a connection socket, sending a reset to the client.
    ///
    /// Used to tear a splice down when the real socket fails: a graceful close
    /// would look like a clean end-of-response, so the client must see a reset
    /// instead.
    pub fn tcp_abort(&mut self, handle: SocketHandle) {
        self.sockets.get_mut::<tcp::Socket>(handle).abort();
    }

    /// Remove a connection socket from the stack, freeing its buffers.
    ///
    /// Intended to be called once a splice has finished with the socket (see
    /// [`crate::TcpSpliceEngine::closed_handles`]); a connection socket holds
    /// ~16 KiB, so leaving it in the set leaks the way R6 warns about.
    ///
    /// # Panics
    ///
    /// Panics if `handle` does not refer to a socket in the set, so each handle
    /// must be removed exactly once.
    pub fn tcp_remove(&mut self, handle: SocketHandle) {
        self.sockets.remove(handle);
    }

    /// How many sockets the stack currently holds.
    ///
    /// Idle listeners, live connections and the ICMP socket all count. Used to
    /// assert that finished connections do not accumulate.
    pub fn socket_count(&self) -> usize {
        self.sockets.iter().count()
    }

    /// How many idle listening sockets the pool holds.
    ///
    /// Listeners are created on demand per destination port and reaped once the
    /// pool reaches [`MAX_IDLE_LISTENERS`], and each poll trims the pool back
    /// to that cap, so this is bounded however many ports are scanned.
    pub fn listener_count(&self) -> usize {
        self.listeners.len()
    }
}

/// The protocol number of a structurally valid IPv4 packet.
///
/// Returns `None` for a non-IPv4 packet or one whose header and total length
/// fields do not fit the buffer, so [`Gateway::route`] can drop it without
/// letting smoltcp see a malformed frame.
fn ipv4_protocol(pkt: &[u8]) -> Option<u8> {
    if pkt.len() < 20 || pkt[0] >> 4 != 4 {
        return None;
    }
    let header_len = usize::from(pkt[0] & 0x0f) * 4;
    if header_len < 20 || header_len > pkt.len() {
        return None;
    }
    let total_len = usize::from(u16::from_be_bytes([pkt[2], pkt[3]]));
    if total_len < header_len || total_len > pkt.len() {
        return None;
    }
    Some(pkt[9])
}

/// The destination port of an IPv4 TCP packet carrying a bare SYN, if it is one.
///
/// This is the gateway's demultiplexing seam: it peeks at the IP protocol field
/// (and, for TCP, the SYN flag) before a packet reaches the stack.
fn syn_dst_port(pkt: &[u8]) -> Option<u16> {
    if pkt.len() < 20 || pkt[0] >> 4 != 4 {
        return None;
    }
    let ip_header_len = usize::from(pkt[0] & 0x0f) * 4;
    if ip_header_len < 20 || pkt.len() < ip_header_len + 20 || pkt[9] != 6 {
        return None;
    }

    let tcp = &pkt[ip_header_len..];
    let is_pure_syn = tcp[13] & 0x02 != 0 && tcp[13] & 0x10 == 0 && tcp[13] & 0x04 == 0;
    let dst_port = u16::from_be_bytes([tcp[2], tcp[3]]);
    // Port 0 is not a valid destination and `listen(0)` fails, so it never
    // reaches a listener.
    (is_pure_syn && dst_port != 0).then_some(dst_port)
}

/// Convert a smoltcp endpoint into a standard socket address.
fn socket_addr(endpoint: IpEndpoint) -> SocketAddr {
    SocketAddr::new(IpAddr::from(endpoint.addr), endpoint.port)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MTU: usize = 1420;
    const GW_ADDR: Ipv4Addr = Ipv4Addr::new(10, 10, 10, 2);
    const GW_PREFIX: u8 = 24;
    const CLIENT_ISN: u32 = 0x1234_5678;

    const SYN: u8 = 0x02;
    const ACK: u8 = 0x10;
    const PSH: u8 = 0x08;

    fn client() -> SocketAddr {
        "10.10.10.1:53000".parse().unwrap()
    }

    fn dst() -> SocketAddr {
        "1.1.1.1:80".parse().unwrap()
    }

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

    /// A decoded IPv4/TCP packet, as observed on the wire.
    #[derive(Debug)]
    struct Tcp {
        src: SocketAddr,
        dst: SocketAddr,
        seq: u32,
        ack: Option<u32>,
        flags: u8,
        mss: Option<u16>,
        payload: Vec<u8>,
    }

    /// A hand-built IPv4/TCP packet with valid IPv4 and TCP checksums.
    #[allow(clippy::too_many_arguments)]
    fn tcp_packet(
        src: SocketAddr,
        dst: SocketAddr,
        seq: u32,
        ack: Option<u32>,
        flags: u8,
        payload: &[u8],
        mss: Option<u16>,
    ) -> Vec<u8> {
        let (IpAddr::V4(src_ip), IpAddr::V4(dst_ip)) = (src.ip(), dst.ip()) else {
            panic!("tests build IPv4 packets only");
        };
        let src_ip = src_ip.octets();
        let dst_ip = dst_ip.octets();

        let options: Vec<u8> = match mss {
            Some(mss) => vec![2, 4, (mss >> 8) as u8, mss as u8],
            None => Vec::new(),
        };
        let tcp_header_len = 20 + options.len();
        let tcp_len = tcp_header_len + payload.len();
        let total_len = 20 + tcp_len;

        let mut pkt = vec![0u8; total_len];
        pkt[0] = 0x45; // version 4, IHL 5
        pkt[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        pkt[8] = 64; // TTL
        pkt[9] = 6; // TCP
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
        tcp[12] = ((tcp_header_len / 4) as u8) << 4;
        tcp[13] = flags;
        tcp[14..16].copy_from_slice(&65535u16.to_be_bytes()); // window
        tcp[20..tcp_header_len].copy_from_slice(&options);
        tcp[tcp_header_len..].copy_from_slice(payload);

        // TCP checksum over the pseudo-header plus the segment.
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

    fn syn(src: SocketAddr, dst: SocketAddr) -> Vec<u8> {
        tcp_packet(src, dst, CLIENT_ISN, None, SYN, &[], Some(1460))
    }

    fn parse(pkt: &[u8]) -> Tcp {
        let ip_header_len = usize::from(pkt[0] & 0x0f) * 4;
        let src = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15])),
            u16::from_be_bytes([pkt[20], pkt[21]]),
        );
        let dst = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(pkt[16], pkt[17], pkt[18], pkt[19])),
            u16::from_be_bytes([pkt[22], pkt[23]]),
        );

        let tcp = &pkt[ip_header_len..];
        let tcp_header_len = usize::from(tcp[12] >> 4) * 4;
        let flags = tcp[13];
        Tcp {
            src,
            dst,
            seq: u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]),
            ack: (flags & ACK != 0).then(|| u32::from_be_bytes([tcp[8], tcp[9], tcp[10], tcp[11]])),
            flags,
            mss: mss_option(tcp, tcp_header_len),
            payload: tcp[tcp_header_len..].to_vec(),
        }
    }

    fn mss_option(tcp: &[u8], header_len: usize) -> Option<u16> {
        let mut i = 20;
        while i < header_len {
            match tcp[i] {
                0 => break,  // end of options
                1 => i += 1, // no-op
                kind => {
                    let len = usize::from(tcp[i + 1]);
                    if kind == 2 {
                        return Some(u16::from_be_bytes([tcp[i + 2], tcp[i + 3]]));
                    }
                    i += len;
                }
            }
        }
        None
    }

    fn mss_of(pkt: &[u8]) -> u16 {
        parse(pkt).mss.expect("packet carries an MSS option")
    }

    fn expect_tcp_accepted(event: &GatewayEvent) -> (SocketAddr, SocketAddr) {
        match event {
            GatewayEvent::TcpAccepted { client, dst, .. } => (*client, *dst),
        }
    }

    #[test]
    fn accepts_syn_to_arbitrary_destination() {
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let client = client();
        let dst = dst();
        g.push_rx(syn(client, dst));

        let events = g.poll(Instant::now());
        assert_eq!(events.len(), 1, "exactly one connection accepted");
        let (accepted_client, accepted_dst) = expect_tcp_accepted(&events[0]);
        assert_eq!(accepted_client, client);
        assert_eq!(accepted_dst.to_string(), "1.1.1.1:80");

        let tx = g.take_tx();
        assert_eq!(tx.len(), 1, "one SYN-ACK was emitted");
        let reply = parse(&tx[0]);
        assert_eq!(reply.flags, SYN | ACK, "reply is a SYN-ACK");
        assert_eq!(reply.src, dst, "source is the real destination");
        assert_eq!(reply.dst, client, "destination is the client");
        assert_eq!(reply.ack, Some(CLIENT_ISN + 1), "acknowledges the SYN");
        assert_eq!(mss_of(&tx[0]), 1380);
    }

    #[test]
    fn accepts_a_second_arbitrary_address_on_an_arbitrary_port() {
        // 8.8.8.8:443 is neither the interface address nor a fixed port.
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let client: SocketAddr = "10.10.10.1:40000".parse().unwrap();
        let dst: SocketAddr = "8.8.8.8:443".parse().unwrap();
        g.push_rx(syn(client, dst));

        let events = g.poll(Instant::now());
        assert_eq!(events.len(), 1);
        assert_eq!(expect_tcp_accepted(&events[0]).1, dst);
        assert_eq!(parse(&g.take_tx()[0]).src, dst);
    }

    #[test]
    fn accepts_two_connections_to_the_same_port_in_one_batch() {
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let first: SocketAddr = "10.10.10.1:40001".parse().unwrap();
        let second: SocketAddr = "10.10.10.1:40002".parse().unwrap();
        g.push_rx(syn(first, dst()));
        g.push_rx(syn(second, dst()));

        let events = g.poll(Instant::now());
        assert_eq!(events.len(), 2, "both SYNs are accepted");
        let mut clients: Vec<SocketAddr> =
            events.iter().map(|e| expect_tcp_accepted(e).0).collect();
        clients.sort();
        assert_eq!(clients, {
            let mut expected = vec![first, second];
            expected.sort();
            expected
        });
        assert_eq!(g.take_tx().len(), 2, "one SYN-ACK per connection");
    }

    #[test]
    fn advertised_mss_follows_the_device_mtu() {
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, 1400);
        g.push_rx(syn(client(), dst()));
        g.poll(Instant::now());
        assert_eq!(mss_of(&g.take_tx()[0]), 1360);
    }

    #[test]
    fn syn_to_destination_port_zero_is_ignored() {
        // Port 0 is not a valid destination; smoltcp's `listen(0)` fails, so a
        // SYN carrying it must be dropped rather than reach a listener.
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let dst: SocketAddr = "1.1.1.1:0".parse().unwrap();
        g.push_rx(tcp_packet(
            client(),
            dst,
            CLIENT_ISN,
            None,
            SYN,
            &[],
            Some(1460),
        ));

        let events = g.poll(Instant::now());
        assert!(events.is_empty(), "a port-0 SYN is not a valid destination");
    }

    #[test]
    fn non_syn_and_malformed_packets_are_ignored() {
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        g.push_rx(Vec::new()); // empty
        g.push_rx(vec![0u8; 8]); // too short
        g.push_rx(vec![0x60; 20]); // IPv6, wrong version
        // A protocol that is not TCP.
        let mut not_tcp = vec![0u8; 20];
        not_tcp[0] = 0x45;
        not_tcp[2..4].copy_from_slice(&20u16.to_be_bytes());
        not_tcp[9] = 17;
        g.push_rx(not_tcp);
        // A TCP packet that is not a bare SYN.
        g.push_rx(tcp_packet(client(), dst(), 1, Some(1), ACK, &[], None));

        assert!(g.poll(Instant::now()).is_empty(), "nothing is accepted");
    }

    #[test]
    fn idle_listeners_do_not_grow_without_bound() {
        // Every destination port needs its own smoltcp listening socket, and
        // each one holds ~16 KiB. A listener is created for any bare SYN the
        // gateway sees — including one the stack then drops, here because the
        // TCP checksum is wrong — so without reaping it would sit in the socket
        // set forever. A port scan is the cheap way to grow that set without
        // bound.
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let baseline = g.socket_count();

        const PORTS: u16 = 300;
        for i in 0..PORTS {
            let dst: SocketAddr =
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 1024 + i);
            let client: SocketAddr =
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 10, 10, 1)), 40000 + i);
            let mut pkt = syn(client, dst);
            // Break the TCP checksum: smoltcp drops the packet, so the listener
            // created for it is never accepted and never becomes a connection.
            pkt[37] ^= 0xff;
            g.push_rx(pkt);
            assert!(
                g.poll(Instant::now()).is_empty(),
                "a corrupt SYN is not accepted"
            );
            g.take_tx();
        }

        assert!(
            g.listener_count() <= MAX_IDLE_LISTENERS,
            "the idle pool is bounded: {} listeners after {PORTS} ports",
            g.listener_count()
        );
        assert!(
            g.socket_count() <= baseline + MAX_IDLE_LISTENERS,
            "no socket beyond the listener pool is left behind: {} after {PORTS} ports",
            g.socket_count()
        );
    }

    /// A SYN whose listener was reaped must still be accepted once it is made
    /// again: reaping costs a socket, never reachability.
    #[test]
    fn a_reaped_listener_still_accepts_a_later_syn() {
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let first: SocketAddr = "1.1.1.1:1234".parse().unwrap();

        // A SYN the stack drops leaves an idle listener for `first`, the oldest
        // in the pool.
        let mut dropped = syn(client(), first);
        dropped[37] ^= 0xff;
        g.push_rx(dropped);
        g.poll(Instant::now());
        g.take_tx();

        // Fill the pool with other ports, which reaps `first`'s listener.
        for i in 0..MAX_IDLE_LISTENERS as u16 {
            let dst: SocketAddr =
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 2000 + i);
            let mut pkt = syn(client(), dst);
            pkt[37] ^= 0xff;
            g.push_rx(pkt);
            g.poll(Instant::now());
            g.take_tx();
        }
        assert_eq!(g.listener_count(), MAX_IDLE_LISTENERS, "the pool is full");
        assert!(
            g.socket_count() == MAX_IDLE_LISTENERS + 1,
            "the reaped listener's socket is freed (one is the ICMP socket)"
        );

        // The port whose listener was reaped is still reachable.
        g.push_rx(syn(client(), first));
        let events = g.poll(Instant::now());
        assert_eq!(events.len(), 1, "the recreated listener accepts the SYN");
        assert_eq!(expect_tcp_accepted(&events[0]).1, first);
    }

    /// A burst of SYNs to more distinct ports than the pool holds must still
    /// yield a connection for each of them.
    #[test]
    fn a_batch_of_syns_larger_than_the_pool_is_still_served() {
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let ports = MAX_IDLE_LISTENERS + 8;

        for i in 0..ports as u16 {
            let dst: SocketAddr =
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)), 3000 + i);
            g.push_rx(syn(client(), dst));
        }
        let events = g.poll(Instant::now());
        assert_eq!(
            events.len(),
            ports,
            "reaping never takes a listener whose SYN is already queued"
        );
    }

    /// The cap is a steady-state bound, not just a per-`add_listener` one: a
    /// single batch of more ports than the pool holds must not leave the pool
    /// sitting at the batch's high-water mark afterwards.
    ///
    /// This is the case the one-SYN-per-`poll` test cannot see. Here the batch
    /// is of *dropped* SYNs, so no listener leaves the pool by being accepted —
    /// every one of them is still idle when the stack is done, which is exactly
    /// how the pool used to stay at the batch size forever.
    #[test]
    fn a_large_batch_of_dropped_syns_does_not_raise_the_pool_for_good() {
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let baseline = g.socket_count();

        // Three times the cap in one batch, all with a broken checksum so
        // smoltcp drops them and the listeners stay idle.
        let ports = MAX_IDLE_LISTENERS * 3;
        for i in 0..ports as u16 {
            let dst: SocketAddr =
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)), 4000 + i);
            let mut pkt = syn(client(), dst);
            pkt[37] ^= 0xff;
            g.push_rx(pkt);
        }
        assert!(g.poll(Instant::now()).is_empty(), "all SYNs are dropped");

        assert_eq!(
            g.listener_count(),
            MAX_IDLE_LISTENERS,
            "the batch's high-water mark is trimmed back to the cap"
        );
        assert_eq!(
            g.socket_count(),
            baseline + MAX_IDLE_LISTENERS,
            "and the surplus listeners' sockets are actually freed"
        );

        // A later one-port batch only swaps one out for one in, so the pool
        // stays at the cap rather than inheriting the earlier high-water mark.
        let dst: SocketAddr = "203.0.113.9:9999".parse().unwrap();
        let mut pkt = syn(client(), dst);
        pkt[37] ^= 0xff;
        g.push_rx(pkt);
        assert!(g.poll(Instant::now()).is_empty(), "the SYN is dropped");
        assert_eq!(g.listener_count(), MAX_IDLE_LISTENERS, "still at the cap");
        assert_eq!(g.socket_count(), baseline + MAX_IDLE_LISTENERS);
    }

    #[test]
    fn poll_on_an_empty_device_is_a_no_op() {
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        assert!(g.poll(Instant::now()).is_empty());
        assert!(g.take_tx().is_empty());
        assert!(g.poll_delay(Instant::now()).is_none());
    }

    #[test]
    fn handshake_then_send_and_recv() {
        let mut g = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let client = client();
        let dst = dst();

        // SYN -> SYN-ACK.
        g.push_rx(syn(client, dst));
        let events = g.poll(Instant::now());
        let GatewayEvent::TcpAccepted { handle, .. } = events[0];
        let synack = parse(&g.take_tx()[0]);

        // Client ACK completes the handshake.
        g.push_rx(tcp_packet(
            client,
            dst,
            CLIENT_ISN + 1,
            Some(synack.seq + 1),
            ACK,
            &[],
            None,
        ));
        g.poll(Instant::now());

        // Client -> gateway bytes are readable through tcp_recv.
        g.push_rx(tcp_packet(
            client,
            dst,
            CLIENT_ISN + 1,
            Some(synack.seq + 1),
            PSH | ACK,
            b"world",
            None,
        ));
        g.poll(Instant::now());
        let mut buf = [0u8; 32];
        let n = g.tcp_recv(handle, &mut buf).expect("data is readable");
        assert_eq!(&buf[..n], b"world");

        // Gateway -> client bytes are queued through tcp_send and emitted.
        assert_eq!(g.tcp_send(handle, b"hello").unwrap(), 5);
        g.poll(Instant::now());
        let mut data = None;
        for pkt in g.take_tx() {
            let parsed = parse(&pkt);
            if parsed.payload.as_slice() == b"hello" {
                data = Some(parsed);
                break;
            }
        }
        let data = data.expect("the queued bytes were transmitted");
        assert_eq!(data.src, dst);
        assert_eq!(data.dst, client);

        // A graceful close is accepted without panicking.
        g.tcp_close(handle);
        g.poll(Instant::now());
    }
}
