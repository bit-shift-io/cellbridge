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
pub struct Gateway {
    device: QueueDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
    /// Idle listening sockets not yet connected, with the port they listen on.
    listeners: Vec<(u16, SocketHandle)>,
    /// Destination ports of pure SYNs seen since the last poll, so a listener
    /// is available for each of them before the stack runs.
    pending_syns: Vec<u16>,
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

    /// Queue an inbound IP packet.
    pub fn push_rx(&mut self, pkt: Vec<u8>) {
        if let Some(port) = syn_dst_port(&pkt) {
            self.pending_syns.push(port);
        }
        self.device.push_rx(pkt);
    }

    /// Advance the stack and report newly accepted TCP connections.
    pub fn poll(&mut self, now: Instant) -> Vec<GatewayEvent> {
        self.open_pending_listeners();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        self.drain_accepts()
    }

    /// Make sure every pending SYN has an idle listener to match against.
    fn open_pending_listeners(&mut self) {
        if self.pending_syns.is_empty() {
            return;
        }

        let mut idle: HashMap<u16, usize> = HashMap::new();
        for (port, _) in &self.listeners {
            *idle.entry(*port).or_insert(0) += 1;
        }

        for port in mem::take(&mut self.pending_syns) {
            match idle.get_mut(&port) {
                // An idle listener will absorb this SYN; consume it.
                Some(remaining) if *remaining > 0 => *remaining -= 1,
                _ => self.add_listener(port),
            }
        }
    }

    /// Add a fresh socket listening on `port` and record it as idle.
    fn add_listener(&mut self, port: u16) {
        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]),
            tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]),
        );
        socket
            .listen(port)
            .expect("a fresh socket accepts a nonzero listen port");
        let handle = self.sockets.add(socket);
        self.listeners.push((port, handle));
    }

    /// Turn listeners that have moved out of the Listen state into events,
    /// dropping the ones that closed without connecting.
    fn drain_accepts(&mut self) -> Vec<GatewayEvent> {
        let mut events = Vec::new();
        let mut idle = Vec::new();

        for (port, handle) in mem::take(&mut self.listeners) {
            match self.sockets.get::<tcp::Socket>(handle).state() {
                tcp::State::Listen => idle.push((port, handle)),
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

    /// Begin a graceful close of a connection socket.
    pub fn tcp_close(&mut self, handle: SocketHandle) {
        self.sockets.get_mut::<tcp::Socket>(handle).close();
    }
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
