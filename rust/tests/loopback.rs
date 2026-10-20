//! Full loopback integration: WireGuard on both ends, the gateway in the
//! middle, and real TCP/UDP sockets on the far side.
//!
//! Every packet here takes the long way round. A client packet is encrypted by
//! the client-side [`Tunnel`], decrypted by the gateway-side [`Tunnel`], handed
//! to the [`Gateway`], and answered by the splice engine or the UDP forwarder
//! over a real loopback socket. The answer is then encrypted by the gateway-side
//! tunnel, decrypted by the client-side tunnel, and inspected as wire bytes.
//!
//! Nothing in this file reaches into the crate's internals: it is deliberately
//! an external test, so it proves the public API is sufficient to drive the
//! complete data path.

use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use std::time::Instant as StdInstant;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use boringtun::x25519::{PublicKey, StaticSecret};
use smoltcp::time::Instant;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use cellbridge::{
    BridgeConfig, Gateway, GatewayEvent, IoOp, Routed, TcpSpliceEngine, Tunnel, TunnelError,
    UdpForwarder, build_udp_reply, parse_udp,
};

const MTU: usize = 1420;
const GW_ADDR: Ipv4Addr = Ipv4Addr::new(10, 10, 10, 2);
const GW_PREFIX: u8 = 24;
const TCP_CLIENT_PORT: u16 = 53000;
const UDP_CLIENT_PORT: u16 = 53001;
const CLIENT_ISN: u32 = 0x1234_5678;

const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
const PSH: u8 = 0x08;
const ACK: u8 = 0x10;

/// Upper bound on the whole drive loop, so a regression fails rather than hangs.
const DRIVE_TIMEOUT: Duration = Duration::from_secs(10);
/// The tunnel has no timer deadline (boringtun exposes none), so tick on a
/// fixed, short cadence.
const TICK_INTERVAL: Duration = Duration::from_millis(10);

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
type Packets = Vec<Vec<u8>>;

/// Two WireGuard tunnels facing each other, with encrypted datagrams waiting to
/// cross in each direction.
struct Wire {
    /// The client-facing end. Packets injected here appear at the gateway.
    client_side: Tunnel,
    /// The gateway-facing end. Packets injected here appear at the client.
    gateway_side: Tunnel,
    /// Encrypted datagrams waiting to be decrypted by `client_side`.
    to_client: Vec<Vec<u8>>,
    /// Encrypted datagrams waiting to be decrypted by `gateway_side`.
    to_gateway: Vec<Vec<u8>>,
}

impl Wire {
    fn new() -> Wire {
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
    fn send_from_client(&mut self, packet: &[u8]) -> Result<(), TunnelError> {
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
    fn send_from_gateway(&mut self, packet: &[u8]) -> Result<(), TunnelError> {
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
    fn deliver(&mut self) -> Result<(Packets, Packets), TunnelError> {
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

    fn tick(&mut self) -> Result<(), TunnelError> {
        let client = self.client_side.tick()?;
        self.to_gateway.extend(client.to_network);
        let gateway = self.gateway_side.tick()?;
        self.to_client.extend(gateway.to_network);
        Ok(())
    }

    /// Complete the WireGuard handshake by bouncing datagrams until both sides
    /// report a live session.
    async fn connect(&mut self) {
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
fn tcp_packet(
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
struct TcpSegment {
    src: SocketAddr,
    dst: SocketAddr,
    seq: u32,
    ack: Option<u32>,
    flags: u8,
    payload: Vec<u8>,
}

fn parse_tcp(pkt: &[u8]) -> Option<TcpSegment> {
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
struct TcpClient {
    client: SocketAddr,
    dst: SocketAddr,
    isn: u32,
    next_client_seq: u32,
    expected_server_seq: u32,
    established: bool,
    fin_sent: bool,
    fin_acked: bool,
    received: Vec<u8>,
}

impl TcpClient {
    fn new(client: SocketAddr, dst: SocketAddr) -> TcpClient {
        TcpClient {
            client,
            dst,
            isn: CLIENT_ISN,
            next_client_seq: CLIENT_ISN.wrapping_add(1),
            expected_server_seq: 0,
            established: false,
            fin_sent: false,
            fin_acked: false,
            received: Vec::new(),
        }
    }

    fn syn(&self) -> Vec<u8> {
        tcp_packet(self.client, self.dst, self.isn, None, SYN, &[])
    }

    /// React to one gateway → client segment, returning the client packets that
    /// should be put back on the wire.
    fn on_segment(&mut self, segment: &TcpSegment) -> Vec<Vec<u8>> {
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
            if !self.fin_sent && self.received.windows(5).any(|w| w == b"hello") {
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

    fn finished(&self) -> bool {
        self.fin_acked && self.received.windows(5).any(|w| w == b"hello")
    }
}

// ---------------------------------------------------------------------------
// Real servers on loopback.
// ---------------------------------------------------------------------------

async fn tcp_echo_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
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

async fn udp_echo_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
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
fn apply(gateway: &mut Gateway, ops: Vec<IoOp>) {
    for op in ops {
        match op {
            IoOp::Close { handle } => gateway.tcp_close(handle),
            IoOp::Reset { handle } => gateway.tcp_abort(handle),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tcp_and_udp_roundtrip_through_tunnel() {
    let (tcp_dst, tcp_server) = tcp_echo_server().await;
    let (udp_dst, udp_server) = udp_echo_server().await;

    let mut wire = Wire::new();
    wire.connect().await;

    let mut gateway = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
    let mut splice = TcpSpliceEngine::new();
    let mut forwarder = UdpForwarder::new(16, Duration::from_secs(30));
    let udp_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind the UDP client socket");

    let tcp_client_addr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 10, 10, 1)), TCP_CLIENT_PORT);
    let udp_client_addr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 10, 10, 1)), UDP_CLIENT_PORT);

    let mut tcp_client = TcpClient::new(tcp_client_addr, tcp_dst);
    wire.send_from_client(&tcp_client.syn())
        .expect("inject the SYN");
    let udp_request_packet = build_udp_reply(udp_client_addr, udp_dst, b"ping");
    wire.send_from_client(&udp_request_packet)
        .expect("inject the UDP datagram");

    let mut handles = Vec::new();
    let mut udp_request = None;
    let mut udp_reply: Option<Vec<u8>> = None;
    let mut udp_flow = None;
    let mut udp_reply_source = None;
    let mut udp_reply_dst = None;
    let mut saw_routed_udp = false;
    let mut last_tick = StdInstant::now();

    let deadline = StdInstant::now() + DRIVE_TIMEOUT;
    while !(tcp_client.finished() && udp_reply.is_some()) {
        assert!(
            StdInstant::now() < deadline,
            "loopback did not finish: tcp_finished={}, udp_reply={}",
            tcp_client.finished(),
            udp_reply.is_some()
        );

        let (to_gateway, to_client) = wire.deliver().expect("tunnel delivery");

        for packet in to_gateway {
            match gateway.route(packet.clone()) {
                Routed::Udp => {
                    saw_routed_udp = true;
                    if let Some(out) = forwarder.handle_outbound(&packet) {
                        udp_flow = Some(out.flow);
                        if udp_request.is_none() {
                            udp_request = Some(out);
                        }
                    }
                }
                Routed::Tcp => {}
                Routed::Dropped => panic!("gateway dropped a packet that crossed the tunnel"),
            }
        }

        if let Some(out) = udp_request.take() {
            // The local socket is ephemeral on purpose: the reply's addressing
            // is rebuilt from the `FlowId`, so the source-port fidelity this
            // test pins lives in the inner packet, not in this socket.
            udp_socket
                .send_to(&out.payload, out.dst)
                .await
                .expect("send the UDP payload over a real socket");
        }

        for event in gateway.poll(Instant::now()) {
            let GatewayEvent::TcpAccepted {
                handle,
                client,
                dst,
            } = event;
            assert_eq!(client, tcp_client_addr);
            assert_eq!(dst, tcp_dst);
            splice.on_accept(handle, client, dst);
            handles.push(handle);
        }

        // Reactor half: pull ready peer bytes before pumping, so this path is
        // genuinely exercised rather than bypassed by `pump`'s own read.
        for handle in handles.iter().copied() {
            apply(&mut gateway, splice.on_readable(handle));
        }
        let ops = splice.pump(&mut gateway);
        apply(&mut gateway, ops);
        gateway.poll(Instant::now());

        for packet in gateway.take_tx() {
            wire.send_from_gateway(&packet)
                .expect("encapsulate a gateway packet");
        }
        for handle in splice.closed_handles() {
            handles.retain(|h| *h != handle);
            gateway.tcp_remove(handle);
        }

        let mut buf = vec![0u8; MTU + 64];
        loop {
            match udp_socket.try_recv_from(&mut buf) {
                Ok((n, _from)) => {
                    let flow = udp_flow.expect("a UDP reply needs a tracked flow");
                    if let Some(reply) = forwarder.handle_reply_within_mtu(flow, &buf[..n], MTU) {
                        wire.send_from_gateway(&reply)
                            .expect("encapsulate a UDP reply");
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => panic!("UDP receive failed: {err}"),
            }
        }

        for packet in to_client {
            if packet.get(9) == Some(&6) {
                if let Some(segment) = parse_tcp(&packet)
                    && segment.dst == tcp_client_addr
                {
                    for out in tcp_client.on_segment(&segment) {
                        wire.send_from_client(&out)
                            .expect("encapsulate a client packet");
                    }
                }
            } else if let Some(parsed) = parse_udp(&packet)
                && parsed.dst == udp_client_addr
            {
                udp_reply_source = Some(parsed.src);
                udp_reply_dst = Some(parsed.dst);
                udp_reply = Some(parsed.payload);
            }
        }

        if last_tick.elapsed() >= TICK_INTERVAL {
            wire.tick().expect("tunnel tick");
            last_tick = StdInstant::now();
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    // --- TCP assertions -----------------------------------------------------
    assert!(
        tcp_client.received.windows(5).any(|w| w == b"hello"),
        "the TCP payload round-tripped through the tunnel"
    );
    assert!(
        tcp_client.fin_acked,
        "the spliced connection closed cleanly"
    );

    // --- UDP assertions -----------------------------------------------------
    assert!(
        saw_routed_udp,
        "the gateway demultiplexed UDP before smoltcp"
    );
    assert_eq!(udp_reply.as_deref(), Some(b"pong".as_slice()));
    let source = udp_reply_source.expect("the UDP reply has a source");
    let destination = udp_reply_dst.expect("the UDP reply has a destination");
    assert_eq!(
        source.port(),
        udp_dst.port(),
        "the reply's source port is the destination port that was used"
    );
    assert_eq!(source, udp_dst, "the reply comes from the real destination");
    assert_eq!(
        destination, udp_client_addr,
        "the reply is addressed to the client"
    );

    let flow = udp_flow.expect("the forwarder recorded a flow");
    assert_eq!(
        flow.client.port(),
        UDP_CLIENT_PORT,
        "the flow preserves the client's original source port"
    );
    assert_eq!(flow.client, udp_client_addr);
    assert_eq!(flow.dst.port(), udp_dst.port());
    assert_eq!(flow.dst, udp_dst);

    // --- real servers finished ----------------------------------------------
    tokio::time::timeout(Duration::from_secs(5), tcp_server)
        .await
        .expect("the TCP echo server saw the whole connection")
        .expect("TCP echo server task");
    tokio::time::timeout(Duration::from_secs(5), udp_server)
        .await
        .expect("the UDP echo server saw the datagram")
        .expect("UDP echo server task");
}

#[test]
fn route_hands_udp_to_the_forwarder_and_tcp_to_the_stack() {
    let mut gateway = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
    let client: SocketAddr = "10.10.10.1:53002".parse().unwrap();
    let dst: SocketAddr = "192.0.2.10:443".parse().unwrap();

    assert_eq!(gateway.route(Vec::new()), Routed::Dropped);
    assert_eq!(
        gateway.route(vec![0x60; 40]),
        Routed::Dropped,
        "IPv6 is dropped"
    );

    let udp = build_udp_reply(client, dst, b"datagram");
    assert_eq!(gateway.route(udp), Routed::Udp);
    assert!(
        gateway.poll(Instant::now()).is_empty(),
        "a diverted UDP packet never reaches smoltcp"
    );
    assert!(gateway.take_tx().is_empty());

    let syn = tcp_packet(client, dst, CLIENT_ISN, None, SYN, &[]);
    assert_eq!(gateway.route(syn), Routed::Tcp);
    let events = gateway.poll(Instant::now());
    assert_eq!(events.len(), 1, "the SYN reached smoltcp and was accepted");
}

#[test]
fn udp_replies_larger_than_the_tunnel_mtu_are_dropped() {
    let mut forwarder = UdpForwarder::new(8, Duration::from_secs(30));
    let client: SocketAddr = "10.10.10.1:53003".parse().unwrap();
    let dst: SocketAddr = "192.0.2.20:443".parse().unwrap();
    let request = build_udp_reply(client, dst, b"q");
    let flow = forwarder
        .handle_outbound(&request)
        .expect("a valid request creates a flow")
        .flow;

    // IPv4 (20) + UDP (8) leave this much for the payload.
    const HEADERS: usize = 20 + 8;
    let under = vec![0xab; MTU - HEADERS - 1];
    let at_cap = vec![0xab; MTU - HEADERS];
    let over = vec![0xab; MTU - HEADERS + 1];

    assert_eq!(
        forwarder
            .handle_reply_within_mtu(flow, &under, MTU)
            .expect("a reply just under the MTU fits")
            .len(),
        MTU - 1
    );
    assert_eq!(
        forwarder
            .handle_reply_within_mtu(flow, &at_cap, MTU)
            .expect("a reply exactly at the MTU fits")
            .len(),
        MTU
    );
    assert!(
        forwarder
            .handle_reply_within_mtu(flow, &over, MTU)
            .is_none(),
        "a reply one byte over the tunnel MTU is dropped before encapsulation"
    );
}
