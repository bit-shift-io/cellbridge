//! The TCP splice engine.
//!
//! The [`Gateway`] terminates the LAN client's TCP connection in smoltcp. This
//! module completes the path: on accept it starts dialling the destination the
//! client asked for with an ordinary TCP socket, then copies bytes between that
//! real socket and the smoltcp connection.
//!
//! The engine owns the real sockets. A caller drives it from its poll loop:
//!
//! 1. `gateway.poll(...)` runs the smoltcp state machine;
//! 2. [`TcpSpliceEngine::pump`] moves bytes across every splice and returns the
//!    work the caller must apply to the gateway (see [`IoOp`]);
//! 3. the caller applies those ops (`Gateway::tcp_send` / `Gateway::tcp_close`)
//!    and polls again so they reach the wire.
//!
//! Nothing on the poll path blocks: the dial runs asynchronously and is only
//! consulted (never waited on), so a destination that never answers cannot stall
//! other flows or the tunnel loop. A splice whose destination could not be
//! dialled, or whose real socket fails, is torn down by resetting the
//! client-facing socket rather than left hanging.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp::RecvError;

use crate::gateway::Gateway;

/// Scratch size for a single move between the two halves.
///
/// Also caps how much one splice step offers, so a single returned
/// [`IoOp::Send`] always fits in a connection's 8 KiB smoltcp transmit buffer.
const CHUNK: usize = 4096;

/// Work a splice needs the caller to apply to the [`Gateway`].
///
/// The engine owns the real sockets, but the smoltcp side lives in the
/// [`Gateway`] — and [`TcpSpliceEngine::on_readable`] is not handed one — so
/// peer-to-client bytes, closes and resets come back for the caller to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoOp {
    /// Queue `data` on the smoltcp connection: `Gateway::tcp_send`.
    Send {
        /// The smoltcp connection socket.
        handle: SocketHandle,
        /// Bytes read from the real peer socket.
        data: Vec<u8>,
    },
    /// Gracefully close the smoltcp connection: `Gateway::tcp_close`.
    Close {
        /// The smoltcp connection socket.
        handle: SocketHandle,
    },
    /// Reset the smoltcp connection: `Gateway::tcp_abort`.
    ///
    /// Returned when the real socket fails, so the client sees a reset rather
    /// than what would look like a clean end of response.
    Reset {
        /// The smoltcp connection socket.
        handle: SocketHandle,
    },
}

/// One in-flight splice: the client-facing smoltcp socket and the real socket.
struct Flow {
    /// The real socket to the destination, once the dial has completed.
    peer: Option<TcpStream>,
    /// The background dial, while it is still in flight.
    dialing: Option<Receiver<std::io::Result<TcpStream>>>,
    /// Bytes dequeued from smoltcp that the peer socket has not accepted yet.
    pending: Vec<u8>,
    /// The client's write half closed; the peer socket was told with a FIN.
    client_closed: bool,
    /// The peer's read half ended: EOF, a read error, or a failed dial.
    peer_closed: bool,
}

/// The outcome of consulting a flow's dial during a poll step.
enum Dial {
    /// The real socket is connected and ready.
    Ready,
    /// The dial is still in flight.
    Pending,
    /// The dial failed (or the flow is gone); the client must be reset.
    Failed,
}

impl Flow {
    /// Advance this flow's dial without ever waiting on it.
    fn advance_dial(&mut self) -> Dial {
        if self.peer.is_some() {
            return Dial::Ready;
        }
        match self.dialing.as_mut().map(|rx| rx.try_recv()) {
            Some(Ok(Ok(stream))) => {
                self.peer = Some(stream);
                self.dialing = None;
                Dial::Ready
            }
            // An error from `try_recv` (sender gone) or from the dial itself.
            Some(Ok(Err(_))) | Some(Err(TryRecvError::Disconnected)) => Dial::Failed,
            Some(Err(TryRecvError::Empty)) => Dial::Pending,
            // No peer and no dial: the flow was created broken.
            None => Dial::Failed,
        }
    }
}

/// Splices smoltcp TCP connections to real, cellular-bound sockets.
#[derive(Default)]
pub struct TcpSpliceEngine {
    flows: HashMap<SocketHandle, Flow>,
    /// Handles whose splice has ended, drained by [`Self::closed_handles`].
    finished: RefCell<Vec<SocketHandle>>,
}

impl TcpSpliceEngine {
    /// Create an engine with no splices.
    pub fn new() -> Self {
        TcpSpliceEngine::default()
    }

    /// Begin splicing the accepted connection `handle`.
    ///
    /// `_client` is the router's masqueraded source and is not needed: the real
    /// socket is dialled to `dst`, the destination the client asked for. The
    /// dial runs asynchronously — this returns immediately, so an unreachable
    /// destination cannot stall the poll loop — and the result is picked up by
    /// the next [`Self::pump`] or [`Self::on_readable`]. A failed dial resets
    /// the client-facing socket instead of leaving it hanging.
    ///
    /// Must be called from within a `tokio` runtime: the dial is a spawned task.
    pub fn on_accept(&mut self, handle: SocketHandle, _client: SocketAddr, dst: SocketAddr) {
        let (tx, rx) = mpsc::channel();
        tokio::spawn(async move {
            let result = tokio::net::TcpStream::connect(dst)
                .await
                .and_then(tokio::net::TcpStream::into_std);
            let _ = tx.send(result);
        });
        self.flows.insert(
            handle,
            Flow {
                peer: None,
                dialing: Some(rx),
                pending: Vec::new(),
                client_closed: false,
                peer_closed: false,
            },
        );
    }

    /// Drive every splice, returning the work the caller must apply.
    pub fn pump(&mut self, gw: &mut Gateway) -> Vec<IoOp> {
        let mut ops = Vec::new();
        for handle in self.flows.keys().copied().collect::<Vec<_>>() {
            match self.flows.get_mut(&handle).map(Flow::advance_dial) {
                // Never wait on a dial: keep serving the other flows.
                Some(Dial::Pending) | None => {}
                Some(Dial::Failed) => {
                    if let Some(flow) = self.flows.get_mut(&handle) {
                        Self::tear_down(flow, handle, &mut ops);
                    }
                    self.reap(handle);
                }
                Some(Dial::Ready) => {
                    self.drain_from_client(gw, handle, &mut ops);
                    self.read_from_peer(handle, &mut ops);
                    self.reap(handle);
                }
            }
        }
        ops
    }

    /// Move the bytes waiting on the real socket for `handle` to the client.
    ///
    /// This is the reactor-driven half of [`Self::pump`]: call it when the real
    /// socket becomes readable and apply the returned [`IoOp`]s to the gateway.
    pub fn on_readable(&mut self, handle: SocketHandle) -> Vec<IoOp> {
        let mut ops = Vec::new();
        match self.flows.get_mut(&handle).map(Flow::advance_dial) {
            Some(Dial::Ready) => self.read_from_peer(handle, &mut ops),
            Some(Dial::Failed) => {
                if let Some(flow) = self.flows.get_mut(&handle) {
                    Self::tear_down(flow, handle, &mut ops);
                }
            }
            Some(Dial::Pending) | None => {}
        }
        self.reap(handle);
        ops
    }

    /// Copy whatever the peer socket has ready into the client-facing half.
    fn read_from_peer(&mut self, handle: SocketHandle, ops: &mut Vec<IoOp>) {
        if let Some(flow) = self.flows.get_mut(&handle)
            && !flow.peer_closed
        {
            let mut buf = [0u8; CHUNK];
            let read = flow.peer.as_mut().map(|stream| stream.read(&mut buf));
            match read {
                Some(Ok(0)) => {
                    flow.peer_closed = true;
                    ops.push(IoOp::Close { handle });
                }
                Some(Ok(n)) => ops.push(IoOp::Send {
                    handle,
                    data: buf[..n].to_vec(),
                }),
                Some(Err(err)) if err.kind() == ErrorKind::WouldBlock => {}
                Some(Err(_)) | None => Self::tear_down(flow, handle, ops),
            }
        }
    }

    /// Copy everything the client has sent into the real peer socket.
    ///
    /// Bytes the socket will not take yet are held in the flow and retried, so
    /// nothing is lost when the peer is briefly write-blocked.
    fn drain_from_client(&mut self, gw: &mut Gateway, handle: SocketHandle, ops: &mut Vec<IoOp>) {
        let Some(flow) = self.flows.get_mut(&handle) else {
            return;
        };
        if flow.client_closed {
            return;
        }

        let mut buf = [0u8; CHUNK];
        loop {
            if !flow.pending.is_empty() {
                match flow.peer.as_mut().map(|stream| stream.write(&flow.pending)) {
                    Some(Ok(0)) => {
                        Self::tear_down(flow, handle, ops);
                        return;
                    }
                    Some(Ok(n)) => {
                        flow.pending.drain(..n);
                    }
                    Some(Err(err)) if err.kind() == ErrorKind::WouldBlock => return,
                    Some(Err(_)) | None => {
                        Self::tear_down(flow, handle, ops);
                        return;
                    }
                }
                if !flow.pending.is_empty() {
                    return; // still backed up; do not dequeue more
                }
            }

            match gw.tcp_recv(handle, &mut buf) {
                Ok(0) => return,
                Ok(n) => flow.pending.extend_from_slice(&buf[..n]),
                Err(RecvError::Finished) => {
                    if let Some(stream) = flow.peer.as_ref() {
                        let _ = stream.shutdown(Shutdown::Write);
                    }
                    flow.client_closed = true;
                    return;
                }
                Err(_) => {
                    Self::tear_down(flow, handle, ops);
                    return;
                }
            }
        }
    }

    /// Tear a splice down on error: reset the client so it does not mistake a
    /// failure for a clean end of response.
    fn tear_down(flow: &mut Flow, handle: SocketHandle, ops: &mut Vec<IoOp>) {
        if let Some(stream) = flow.peer.as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        flow.client_closed = true;
        flow.peer_closed = true;
        ops.push(IoOp::Reset { handle });
    }

    /// Once both halves are done, forget the flow and report its handle.
    fn reap(&mut self, handle: SocketHandle) {
        let done = self
            .flows
            .get(&handle)
            .is_some_and(|flow| flow.client_closed && flow.peer_closed);
        if done {
            self.flows.remove(&handle);
            self.finished.borrow_mut().push(handle);
        }
    }

    /// Take the handles whose splice has ended since this was last called.
    ///
    /// Each handle is returned exactly once; the caller should free it with
    /// `Gateway::tcp_remove` (after one more poll, so the final FIN is sent).
    pub fn closed_handles(&self) -> Vec<SocketHandle> {
        self.finished.borrow_mut().drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    use smoltcp::time::Instant;

    use crate::gateway::GatewayEvent;

    const MTU: usize = 1420;
    const GW_ADDR: Ipv4Addr = Ipv4Addr::new(10, 10, 10, 2);
    const GW_PREFIX: u8 = 24;
    const CLIENT_ISN: u32 = 0x1234_5678;

    const FIN: u8 = 0x01;
    const SYN: u8 = 0x02;
    const PSH: u8 = 0x08;
    const ACK: u8 = 0x10;

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
        dst: SocketAddr,
        seq: u32,
        flags: u8,
        payload: Vec<u8>,
    }

    /// A hand-built IPv4/TCP packet with valid IPv4 and TCP checksums.
    fn tcp_packet(
        src: SocketAddr,
        dst: SocketAddr,
        seq: u32,
        ack: Option<u32>,
        flags: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let (IpAddr::V4(src_ip), IpAddr::V4(dst_ip)) = (src.ip(), dst.ip()) else {
            panic!("tests build IPv4 packets only");
        };
        let src_ip = src_ip.octets();
        let dst_ip = dst_ip.octets();

        let tcp_len = 20 + payload.len();
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
        tcp[12] = 5 << 4; // data offset: 20 byte header
        tcp[13] = flags;
        tcp[14..16].copy_from_slice(&65535u16.to_be_bytes()); // window
        tcp[20..].copy_from_slice(payload);

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

    fn parse(pkt: &[u8]) -> Tcp {
        let ip_header_len = usize::from(pkt[0] & 0x0f) * 4;
        let dst = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(pkt[16], pkt[17], pkt[18], pkt[19])),
            u16::from_be_bytes([pkt[22], pkt[23]]),
        );

        let tcp = &pkt[ip_header_len..];
        let tcp_header_len = usize::from(tcp[12] >> 4) * 4;
        Tcp {
            dst,
            seq: u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]),
            flags: tcp[13],
            payload: tcp[tcp_header_len..].to_vec(),
        }
    }

    /// Complete a handshake on `gw` and return the connection handle plus the
    /// sequence numbers the two ends now expect.
    ///
    /// The client half is simulated with hand-built packets whose ACK is taken
    /// from the SYN-ACK the gateway produced.
    fn handshake(
        gw: &mut Gateway,
        client: SocketAddr,
        dst: SocketAddr,
    ) -> (SocketHandle, u32, u32) {
        gw.push_rx(tcp_packet(client, dst, CLIENT_ISN, None, SYN, &[]));
        let events = gw.poll(Instant::now());
        let GatewayEvent::TcpAccepted { handle, .. } = events.into_iter().next().expect("accepted");
        let synack = parse(&gw.take_tx()[0]);
        assert_eq!(synack.flags, SYN | ACK, "reply is a SYN-ACK");
        let server_seq = synack.seq + 1;
        let client_seq = CLIENT_ISN + 1;
        gw.push_rx(tcp_packet(
            client,
            dst,
            client_seq,
            Some(server_seq),
            ACK,
            &[],
        ));
        gw.poll(Instant::now());
        (handle, server_seq, client_seq)
    }

    /// Apply the engine's [`IoOp`]s to the gateway.
    fn apply(gw: &mut Gateway, ops: Vec<IoOp>) {
        for op in ops {
            match op {
                IoOp::Send { handle, data } => {
                    let n = gw
                        .tcp_send(handle, &data)
                        .expect("the connection accepts data");
                    assert_eq!(n, data.len(), "a whole chunk fits in the send buffer");
                }
                IoOp::Close { handle } => gw.tcp_close(handle),
                IoOp::Reset { handle } => gw.tcp_abort(handle),
            }
        }
    }

    /// A `tokio` echo server bound to a loopback port.
    async fn echo_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port");
        let addr = listener.local_addr().expect("local address");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept one connection");
            let (mut read, mut write) = stream.split();
            let mut buf = [0u8; 256];
            loop {
                let n = read.read(&mut buf).await.expect("read");
                if n == 0 {
                    break;
                }
                write.write_all(&buf[..n]).await.expect("write");
            }
            write.shutdown().await.expect("shutdown");
        });
        (addr, task)
    }

    /// Run the gateway/engine loop until `want` has come back, and return every
    /// byte received on the client-facing side.
    async fn drive_until(
        gw: &mut Gateway,
        engine: &mut TcpSpliceEngine,
        client: SocketAddr,
        want: &[u8],
    ) -> Vec<u8> {
        let mut received = Vec::new();
        for _ in 0..1000 {
            gw.poll(Instant::now());
            let ops = engine.pump(gw);
            apply(gw, ops);
            gw.poll(Instant::now());
            for pkt in gw.take_tx() {
                let parsed = parse(&pkt);
                if parsed.dst == client && !parsed.payload.is_empty() {
                    received.extend_from_slice(&parsed.payload);
                }
            }
            if received.windows(want.len()).any(|window| window == want) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        received
    }

    #[tokio::test]
    async fn splices_and_preserves_half_close() {
        let (dst, server) = echo_server().await;
        let client: SocketAddr = "10.10.10.1:53000".parse().unwrap();

        let mut gw = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let mut engine = TcpSpliceEngine::new();
        let (handle, server_seq, mut client_seq) = handshake(&mut gw, client, dst);
        engine.on_accept(handle, client, dst);

        // "hello" is echoed straight back.
        gw.push_rx(tcp_packet(
            client,
            dst,
            client_seq,
            Some(server_seq),
            PSH | ACK,
            b"hello",
        ));
        client_seq += 5;
        let echoed = drive_until(&mut gw, &mut engine, client, b"hello").await;
        assert_eq!(&echoed, b"hello", "the first payload round-trips");

        // The client sends a final message and closes its write half. The echo
        // of the final message must still arrive: the half-close is propagated
        // to the real socket as a FIN, but the reverse direction stays open.
        gw.push_rx(tcp_packet(
            client,
            dst,
            client_seq,
            Some(server_seq),
            PSH | ACK,
            b"world",
        ));
        client_seq += 5;
        gw.push_rx(tcp_packet(
            client,
            dst,
            client_seq,
            Some(server_seq),
            FIN | ACK,
            &[],
        ));
        let echoed = drive_until(&mut gw, &mut engine, client, b"world").await;
        assert!(
            echoed.windows(5).any(|window| window == b"world"),
            "the final payload round-trips after the client half-closes"
        );

        // The echo server only finishes once it has read EOF, which is the
        // half-close propagated to the real socket — awaiting it (with a
        // timeout, so a regression fails instead of hanging) proves the FIN
        // crossed the splice.
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("the echo server saw the client's half-close")
            .expect("echo server task");
    }

    #[tokio::test]
    async fn a_completed_connection_frees_its_gateway_socket() {
        let (dst, server) = echo_server().await;
        let client: SocketAddr = "10.10.10.1:53001".parse().unwrap();

        let mut gw = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let baseline = gw.socket_count();
        let mut engine = TcpSpliceEngine::new();
        let (handle, server_seq, mut client_seq) = handshake(&mut gw, client, dst);
        engine.on_accept(handle, client, dst);
        assert_eq!(
            gw.socket_count(),
            baseline + 1,
            "the connection is a socket on top of the baseline"
        );

        // Send a payload and close the client's write half.
        gw.push_rx(tcp_packet(
            client,
            dst,
            client_seq,
            Some(server_seq),
            PSH | ACK,
            b"bye",
        ));
        client_seq += 3;
        gw.push_rx(tcp_packet(
            client,
            dst,
            client_seq,
            Some(server_seq),
            FIN | ACK,
            &[],
        ));

        let mut closed = Vec::new();
        for _ in 0..1000 {
            gw.poll(Instant::now());
            let ops = engine.pump(&mut gw);
            apply(&mut gw, ops);
            gw.poll(Instant::now());
            gw.take_tx();
            closed = engine.closed_handles();
            if !closed.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(closed, vec![handle], "the finished splice is reported once");

        for handle in closed {
            gw.tcp_remove(handle);
        }
        assert_eq!(
            gw.socket_count(),
            baseline,
            "the connection socket is freed once the splice ends"
        );

        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("the echo server saw the client's half-close")
            .expect("echo server task");
    }

    #[tokio::test]
    async fn a_failed_connect_resets_the_client_socket() {
        // Grab an ephemeral port, then close it so the dial is refused.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead = listener.local_addr().unwrap();
        drop(listener);

        let client: SocketAddr = "10.10.10.1:53002".parse().unwrap();
        let mut gw = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let (handle, ..) = handshake(&mut gw, client, dead);

        let mut engine = TcpSpliceEngine::new();
        engine.on_accept(handle, client, dead);

        // The dial runs on a spawned task, so wait for it to report back.
        let mut ops = Vec::new();
        for _ in 0..1000 {
            ops = engine.pump(&mut gw);
            if !ops.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(
            ops,
            vec![IoOp::Reset { handle }],
            "a refused dial resets the client instead of hanging"
        );
        assert_eq!(engine.closed_handles(), vec![handle]);
    }

    #[tokio::test]
    async fn a_client_reset_tears_the_splice_down() {
        let (dst, _server) = echo_server().await;
        let client: SocketAddr = "10.10.10.1:53003".parse().unwrap();
        const RST: u8 = 0x04;

        let mut gw = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let mut engine = TcpSpliceEngine::new();
        let (handle, server_seq, mut client_seq) = handshake(&mut gw, client, dst);
        engine.on_accept(handle, client, dst);

        // Complete a round-trip first, which also proves the dial finished.
        gw.push_rx(tcp_packet(
            client,
            dst,
            client_seq,
            Some(server_seq),
            PSH | ACK,
            b"hi",
        ));
        client_seq += 2;
        let echoed = drive_until(&mut gw, &mut engine, client, b"hi").await;
        assert_eq!(&echoed, b"hi");

        // The client resets the connection; the splice must tear both ends
        // down rather than keep copying into a socket the stack has dropped.
        gw.push_rx(tcp_packet(
            client,
            dst,
            client_seq,
            Some(server_seq),
            RST | ACK,
            &[],
        ));
        gw.poll(Instant::now());
        let ops = engine.pump(&mut gw);
        assert_eq!(
            ops,
            vec![IoOp::Reset { handle }],
            "a client reset resets the real socket and closes the splice"
        );
        assert_eq!(engine.closed_handles(), vec![handle]);
    }

    /// R7: a dial that never completes must not block the poll path or any
    /// other flow.
    #[tokio::test]
    async fn a_pending_dial_does_not_block_other_flows() {
        let (echo_dst, _server) = echo_server().await;
        // A reserved TEST-NET-1 address on a non-standard port: nothing answers,
        // so the SYN is blackholed and the dial stays in flight.
        let blackhole: SocketAddr = "192.0.2.1:9".parse().unwrap();

        let mut gw = Gateway::new(GW_ADDR, GW_PREFIX, MTU);
        let mut engine = TcpSpliceEngine::new();

        // Connection A dials the blackhole and is left pending.
        let client_a: SocketAddr = "10.10.10.1:53010".parse().unwrap();
        let (handle_a, ..) = handshake(&mut gw, client_a, blackhole);
        let accepted = std::time::Instant::now();
        engine.on_accept(handle_a, client_a, blackhole);
        assert!(
            accepted.elapsed() < Duration::from_millis(100),
            "on_accept must return without waiting for the dial"
        );

        // Connection B reaches the local echo server and must complete normally
        // while A's dial is still in flight.
        let client_b: SocketAddr = "10.10.10.1:53011".parse().unwrap();
        let (handle_b, server_seq_b, client_seq_b) = handshake(&mut gw, client_b, echo_dst);
        engine.on_accept(handle_b, client_b, echo_dst);
        gw.push_rx(tcp_packet(
            client_b,
            echo_dst,
            client_seq_b,
            Some(server_seq_b),
            PSH | ACK,
            b"ping",
        ));
        let echoed = drive_until(&mut gw, &mut engine, client_b, b"ping").await;
        assert_eq!(
            &echoed, b"ping",
            "the second flow is unaffected by the pending dial"
        );

        // A was never waited on, reset, or closed: it is simply still dialling.
        let ops = engine.pump(&mut gw);
        let touches_a = ops.iter().any(|op| {
            let handle = match op {
                IoOp::Send { handle, .. } | IoOp::Close { handle } | IoOp::Reset { handle } => {
                    *handle
                }
            };
            handle == handle_a
        });
        assert!(!touches_a, "the pending dial must produce no work");
        assert!(
            !engine.closed_handles().contains(&handle_a),
            "the pending dial must not be torn down"
        );
    }
}
