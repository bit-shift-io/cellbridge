//! UDP at the packet level.
//!
//! TCP is terminated in smoltcp (see [`crate::Gateway`]), but UDP deliberately
//! is not: one smoltcp UDP socket per flow cannot preserve the client's source
//! port across flows, and protocols such as QUIC depend on that port staying
//! stable. Instead, this module decodes the inner IPv4/UDP packet the phone
//! receives over the tunnel, keeps a bounded table of flows keyed by
//! `(client, destination)`, and builds the reply packet.
//!
//! It owns no sockets: the caller performs the real I/O. [`UdpForwarder`] is
//! synchronous and works purely on byte slices.
//!
//! # Non-goals
//!
//! IPv4 fragmentation is **not** handled: [`parse_udp`] rejects packets with a
//! non-zero fragment offset or the more-fragments flag, rather than pretending
//! to reassemble them. The tunnel MTU is set by the WireGuard link and UDP
//! senders keep datagrams within it, so fragments are dropped.
//!
//! Inbound checksums are not verified. The reply path, however, always emits a
//! correct IPv4 header checksum and a UDP checksum covering the pseudo-header.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::stats::StatsCounters;

/// An IPv4 header with no options.
const IPV4_HEADER_LEN: usize = 20;
/// A UDP header.
const UDP_HEADER_LEN: usize = 8;
/// The IPv4 protocol number for UDP.
const UDP_PROTOCOL: u8 = 17;
/// Largest IPv4 total length representable in its 16-bit field.
const MAX_IPV4_PACKET_LEN: usize = u16::MAX as usize;
/// Largest UDP payload that fits one unfragmented IPv4 packet.
const MAX_UDP_PAYLOAD: usize = MAX_IPV4_PACKET_LEN - IPV4_HEADER_LEN - UDP_HEADER_LEN;
/// TTL of reply packets, matching the stacks around this app.
const REPLY_TTL: u8 = 64;
/// Bit 13 of the flags/fragment field: more fragments follow.
const IPV4_FLAG_MF: u16 = 0x2000;
/// The low 13 bits of the flags/fragment field: fragment offset in 8-byte units.
const IPV4_FRAGMENT_OFFSET: u16 = 0x1fff;

/// Identifies one UDP flow from the phone's point of view.
///
/// `client` is the source the phone sees — the router's masqueraded address,
/// typically `10.10.10.1:<client port>`. The port in particular is what the
/// caller binds its outbound socket to, so it must be preserved exactly.
/// `dst` is the real destination the client asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlowId {
    /// The masqueraded client endpoint (`10.10.10.1:<port>`).
    pub client: SocketAddr,
    /// The real destination endpoint.
    pub dst: SocketAddr,
}

/// A decoded inner IPv4/UDP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUdp {
    /// The packet's source endpoint.
    pub src: SocketAddr,
    /// The packet's destination endpoint.
    pub dst: SocketAddr,
    /// The UDP payload, with padding beyond the UDP length excluded.
    pub payload: Vec<u8>,
}

/// One packet the caller must send to the real destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundUdp {
    /// The flow the packet belongs to. Bind the outbound socket to
    /// `flow.client.port()` so the source port survives.
    pub flow: FlowId,
    /// The real destination to send to (the same as `flow.dst`).
    pub dst: SocketAddr,
    /// The UDP payload to send.
    pub payload: Vec<u8>,
}

/// Bookkeeping for one tracked flow.
struct Flow {
    /// When this flow was last sent or received on, for idle expiry and for
    /// choosing the flow to evict.
    last_seen: Instant,
    /// Insertion sequence, which breaks ties between two flows whose latest
    /// activity landed on the same clock tick.
    seq: u64,
}

/// Forwards UDP flows at the packet level, without owning any sockets.
///
/// The table is bounded in two independent ways:
///
/// * `max_flows` is a hard cap. Inserting a new flow when the table is full
///   evicts the **least recently active** flow, judged by the same
///   [`Flow::last_seen`] that idle expiry uses.
/// * [`Self::expire`] drops flows idle for at least `idle_timeout`.
///
/// Eviction is by activity rather than by insertion order because a flow that
/// is still receiving (a QUIC connection downloading, say) would otherwise be
/// the first to go: the table would stop tracking flows that are demonstrably
/// alive, and the bridge's `active_flows` would under-report what is running.
/// Ties fall back to insertion order, so a table whose flows are equally fresh
/// still evicts the oldest guest.
///
/// An evicted flow's replies can still be built by [`Self::handle_reply`]: the
/// [`FlowId`] carries everything the reply needs, so table pressure cannot
/// blackhole a socket that is still alive.
pub struct UdpForwarder {
    max_flows: usize,
    idle_timeout: Duration,
    flows: HashMap<FlowId, Flow>,
    /// Every tracked flow keyed by its latest activity, least recent first, so
    /// the eviction victim is always the first entry.
    by_activity: BTreeMap<(Instant, u64), FlowId>,
    next_seq: u64,
    /// Payload byte counters shared with the rest of the bridge.
    counters: Arc<StatsCounters>,
}

impl UdpForwarder {
    /// Create a forwarder holding at most `max_flows` flows, expiring flows
    /// that have been idle for `idle_timeout`, and counting its payload bytes
    /// in private counters of its own.
    ///
    /// A `max_flows` of zero drops every packet.
    pub fn new(max_flows: usize, idle_timeout: Duration) -> UdpForwarder {
        UdpForwarder {
            max_flows,
            idle_timeout,
            flows: HashMap::new(),
            by_activity: BTreeMap::new(),
            next_seq: 0,
            counters: Arc::new(StatsCounters::new()),
        }
    }

    /// Create a forwarder that records its payload bytes in `counters`.
    ///
    /// Used by the bridge so the UDP and TCP paths feed one shared
    /// [`crate::StatsCounters`].
    pub fn with_counters(
        max_flows: usize,
        idle_timeout: Duration,
        counters: Arc<StatsCounters>,
    ) -> UdpForwarder {
        UdpForwarder {
            max_flows,
            idle_timeout,
            flows: HashMap::new(),
            by_activity: BTreeMap::new(),
            next_seq: 0,
            counters,
        }
    }

    /// Decode an inner IPv4/UDP packet and record its flow.
    ///
    /// Returns `None` for a malformed, fragmented or non-UDP packet, and when
    /// the table has no capacity. A payload that is forwarded is counted as
    /// sent toward its real destination.
    pub fn handle_outbound(&mut self, ip_packet: &[u8]) -> Option<OutboundUdp> {
        let parsed = parse_udp(ip_packet)?;
        let flow = FlowId {
            client: parsed.src,
            dst: parsed.dst,
        };
        self.track(flow, Instant::now())?;
        self.counters.add_tx(parsed.payload.len());
        Some(OutboundUdp {
            flow,
            dst: parsed.dst,
            payload: parsed.payload,
        })
    }

    /// Build the reply packet for `payload` arriving on `flow`.
    ///
    /// The packet's source is the real destination and its destination is the
    /// client, so the client's source port is restored. A reply also refreshes
    /// the flow's activity and counts its payload as received. An unknown or
    /// evicted flow still gets a reply — the table is bookkeeping, not a gate
    /// on traffic.
    pub fn handle_reply(&mut self, flow: FlowId, payload: &[u8]) -> Vec<u8> {
        let reply = self.build_reply(flow, payload);
        if !reply.is_empty() {
            self.counters.add_rx(payload.len());
        }
        reply
    }

    /// Build the reply for `payload` only if it fits within `mtu`.
    ///
    /// This is the encapsulating caller's variant of [`Self::handle_reply`]:
    /// an inner packet larger than the tunnel MTU cannot be carried without IP
    /// fragmentation, which this data path does not do, so it is dropped
    /// instead of being handed to the tunnel only to be discarded there.
    /// Returns `None` for an over-MTU reply and for payloads that
    /// [`build_udp_reply`] cannot represent (IPv6 destinations or a length
    /// beyond the 16-bit IPv4 total length).
    ///
    /// Only a reply that is actually returned is counted as received: an
    /// over-MTU datagram never reaches the client.
    pub fn handle_reply_within_mtu(
        &mut self,
        flow: FlowId,
        payload: &[u8],
        mtu: usize,
    ) -> Option<Vec<u8>> {
        let reply = self.build_reply(flow, payload);
        if reply.is_empty() || reply.len() > mtu {
            return None;
        }
        self.counters.add_rx(payload.len());
        Some(reply)
    }

    /// Refresh `flow`'s activity and build its reply packet.
    ///
    /// An empty result means the payload cannot be represented as an IPv4
    /// datagram at all; the public wrappers drop it, and neither counts it.
    fn build_reply(&mut self, flow: FlowId, payload: &[u8]) -> Vec<u8> {
        self.touch(&flow, Instant::now());
        build_udp_reply(flow.dst, flow.client, payload)
    }

    /// Forget every tracked flow at once, for example when the tunnel is lost.
    ///
    /// The forwarder owns no sockets, so the returned [`FlowId`]s are what the
    /// caller — which does own the outbound sockets — needs in order to close
    /// them. A flow that keeps sending afterwards is simply tracked again.
    pub fn tear_down_all(&mut self) -> Vec<FlowId> {
        self.by_activity.clear();
        std::mem::take(&mut self.flows).into_keys().collect()
    }

    /// Forget one flow, for example because the caller closed its socket.
    ///
    /// Unlike idle expiry this is immediate, so a flow whose socket was dropped
    /// or evicted stops counting toward `active_flows` at once instead of
    /// lingering until the next [`Self::expire`]. Removing an untracked flow is
    /// harmless and returns `false`.
    pub fn remove(&mut self, flow: &FlowId) -> bool {
        let Some(tracked) = self.flows.remove(flow) else {
            return false;
        };
        self.by_activity.remove(&(tracked.last_seen, tracked.seq));
        true
    }

    /// Remove flows that have been idle for at least the configured timeout.
    ///
    /// `now` is expected to come from the same clock as the `Instant::now()`
    /// calls that timestamp traffic.
    pub fn expire(&mut self, now: Instant) {
        let by_activity = &mut self.by_activity;
        let idle_timeout = self.idle_timeout;
        self.flows.retain(|_, flow| {
            let idle = now.saturating_duration_since(flow.last_seen);
            if idle >= idle_timeout {
                by_activity.remove(&(flow.last_seen, flow.seq));
                false
            } else {
                true
            }
        });
    }

    /// How many flows the table currently holds.
    pub fn len(&self) -> usize {
        self.flows.len()
    }

    /// Whether the table currently holds no flows.
    pub fn is_empty(&self) -> bool {
        self.flows.is_empty()
    }

    /// Record activity on `flow`, inserting it and evicting if necessary.
    ///
    /// Returns `None` only when the table has zero capacity.
    fn track(&mut self, flow: FlowId, now: Instant) -> Option<()> {
        if self.flows.contains_key(&flow) {
            self.touch(&flow, now);
            return Some(());
        }
        if self.max_flows == 0 {
            return None;
        }
        while self.flows.len() >= self.max_flows {
            if !self.evict_least_recent() {
                break;
            }
        }

        let seq = self.next_seq;
        self.next_seq += 1;
        self.flows.insert(
            flow,
            Flow {
                last_seen: now,
                seq,
            },
        );
        self.by_activity.insert((now, seq), flow);
        Some(())
    }

    /// Record that `flow` was just active, keeping the eviction order in step.
    fn touch(&mut self, flow: &FlowId, now: Instant) {
        if let Some(tracked) = self.flows.get_mut(flow) {
            let previous = (tracked.last_seen, tracked.seq);
            tracked.last_seen = now;
            self.by_activity.remove(&previous);
            self.by_activity.insert((now, tracked.seq), *flow);
        }
    }

    /// Backdate `flow`'s last activity, keeping the eviction order in step.
    ///
    /// Test-only: activity only ever moves forwards on the data path.
    #[cfg(test)]
    fn backdate(&mut self, flow: &FlowId, at: Instant) {
        self.touch(flow, at);
    }

    /// Drop the flow with the oldest activity, returning whether one went.
    fn evict_least_recent(&mut self) -> bool {
        let Some((activity, flow)) = self.by_activity.pop_first() else {
            return false;
        };
        let evicted = self.flows.remove(&flow);
        debug_assert!(
            evicted
                .as_ref()
                .is_some_and(|tracked| (tracked.last_seen, tracked.seq) == activity),
            "the activity index must name a live flow at its recorded activity"
        );
        evicted.is_some()
    }
}

/// Decode an inner IPv4/UDP packet.
///
/// Returns `None` if the packet is not a well-formed, unfragmented IPv4 UDP
/// datagram. IPv4 options are skipped; trailing bytes beyond the IP total
/// length, and bytes beyond the UDP length, are ignored.
pub fn parse_udp(pkt: &[u8]) -> Option<ParsedUdp> {
    if pkt.len() < IPV4_HEADER_LEN || pkt[0] >> 4 != 4 {
        return None;
    }

    let ip_header_len = usize::from(pkt[0] & 0x0f) * 4;
    if ip_header_len < IPV4_HEADER_LEN || ip_header_len > pkt.len() {
        return None;
    }

    let total_len = usize::from(u16::from_be_bytes([pkt[2], pkt[3]]));
    if total_len < ip_header_len + UDP_HEADER_LEN || total_len > pkt.len() {
        return None;
    }

    let fragment = u16::from_be_bytes([pkt[6], pkt[7]]);
    if fragment & (IPV4_FLAG_MF | IPV4_FRAGMENT_OFFSET) != 0 {
        return None;
    }

    if pkt[9] != UDP_PROTOCOL {
        return None;
    }

    let src_ip = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
    let dst_ip = Ipv4Addr::new(pkt[16], pkt[17], pkt[18], pkt[19]);

    let datagram = &pkt[ip_header_len..total_len];
    let udp_len = usize::from(u16::from_be_bytes([datagram[4], datagram[5]]));
    if udp_len < UDP_HEADER_LEN || udp_len > datagram.len() {
        return None;
    }

    let src_port = u16::from_be_bytes([datagram[0], datagram[1]]);
    let dst_port = u16::from_be_bytes([datagram[2], datagram[3]]);

    Some(ParsedUdp {
        src: SocketAddr::new(IpAddr::V4(src_ip), src_port),
        dst: SocketAddr::new(IpAddr::V4(dst_ip), dst_port),
        payload: datagram[UDP_HEADER_LEN..udp_len].to_vec(),
    })
}

/// Build an IPv4/UDP packet from `src` to `dst`.
///
/// The IPv4 header checksum and the UDP checksum (which covers the IPv4
/// pseudo-header) are filled in. Both addresses must be IPv4 and the payload
/// must fit an unfragmented IPv4 packet; otherwise an empty vector is returned
/// and the caller must drop the reply.
pub fn build_udp_reply(src: SocketAddr, dst: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let (IpAddr::V4(src_ip), IpAddr::V4(dst_ip)) = (src.ip(), dst.ip()) else {
        return Vec::new();
    };
    if payload.len() > MAX_UDP_PAYLOAD {
        return Vec::new();
    }

    let udp_len = UDP_HEADER_LEN + payload.len();
    let total_len = IPV4_HEADER_LEN + udp_len;
    let mut pkt = vec![0u8; total_len];

    // IPv4 header: version 4, IHL 5 (no options), no fragmentation.
    pkt[0] = 0x45;
    pkt[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
    pkt[8] = REPLY_TTL;
    pkt[9] = UDP_PROTOCOL;
    pkt[12..16].copy_from_slice(&src_ip.octets());
    pkt[16..20].copy_from_slice(&dst_ip.octets());
    let ip_checksum = checksum(&pkt[..IPV4_HEADER_LEN]);
    pkt[10..12].copy_from_slice(&ip_checksum.to_be_bytes());

    let datagram = &mut pkt[IPV4_HEADER_LEN..];
    datagram[0..2].copy_from_slice(&src.port().to_be_bytes());
    datagram[2..4].copy_from_slice(&dst.port().to_be_bytes());
    datagram[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    datagram[UDP_HEADER_LEN..].copy_from_slice(payload);

    // The checksum field is still zero here, so this covers pseudo-header plus
    // the datagram exactly as RFC 768 specifies.
    let mut pseudo = Vec::with_capacity(12 + udp_len);
    pseudo.extend_from_slice(&src_ip.octets());
    pseudo.extend_from_slice(&dst_ip.octets());
    pseudo.extend_from_slice(&[0, UDP_PROTOCOL]);
    pseudo.extend_from_slice(&(udp_len as u16).to_be_bytes());
    pseudo.extend_from_slice(datagram);

    // In IPv4 a zero UDP checksum means "omitted", so encode a computed zero
    // as all ones, as RFC 768 requires.
    let mut udp_checksum = checksum(&pseudo);
    if udp_checksum == 0 {
        udp_checksum = 0xffff;
    }
    datagram[6..8].copy_from_slice(&udp_checksum.to_be_bytes());

    pkt
}

/// 16-bit one's-complement checksum, as used by IPv4 and UDP.
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::{IpAddr, Ipv4Addr};
    use std::time::{Duration, Instant};

    const CLIENT_PORT: u16 = 53000;

    fn client() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 10, 10, 1)), CLIENT_PORT)
    }

    fn dst() -> SocketAddr {
        "1.1.1.1:443".parse().unwrap()
    }

    /// 16-bit one's-complement checksum, written independently of the
    /// implementation so a bug in one cannot hide in the other.
    fn checksum(bytes: &[u8]) -> u16 {
        let mut sum: u32 = 0;
        for chunk in bytes.chunks(2) {
            let word = match chunk {
                [hi, lo] => u16::from_be_bytes([*hi, *lo]),
                [hi] => u16::from(*hi) << 8,
                [] => unreachable!("chunks(2) yields no empty chunk"),
                _ => unreachable!("chunks(2) yields at most two bytes"),
            };
            sum += u32::from(word);
        }
        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        !(sum as u16)
    }

    /// A complete IPv4 packet carrying `payload` as a UDP datagram.
    ///
    /// `ip_header_len` and `frag` exist so malformed-input tests can build the
    /// exact shapes they need; the normal case is a 20-byte header, unfragmented.
    fn ipv4_packet(
        src: SocketAddr,
        dst: SocketAddr,
        protocol: u8,
        payload: &[u8],
        ip_header_len: usize,
        frag: u16,
    ) -> Vec<u8> {
        let (IpAddr::V4(src_ip), IpAddr::V4(dst_ip)) = (src.ip(), dst.ip()) else {
            panic!("tests build IPv4 packets only");
        };
        let src_ip = src_ip.octets();
        let dst_ip = dst_ip.octets();

        let udp_len = 8 + payload.len();
        let total_len = ip_header_len + udp_len;
        let mut pkt = vec![0u8; total_len];

        pkt[0] = 0x40 | (ip_header_len / 4) as u8;
        pkt[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        pkt[6..8].copy_from_slice(&frag.to_be_bytes());
        pkt[8] = 64; // TTL
        pkt[9] = protocol;
        pkt[12..16].copy_from_slice(&src_ip);
        pkt[16..20].copy_from_slice(&dst_ip);
        let ip_checksum = checksum(&pkt[..ip_header_len]);
        pkt[10..12].copy_from_slice(&ip_checksum.to_be_bytes());

        let udp = &mut pkt[ip_header_len..];
        udp[0..2].copy_from_slice(&src.port().to_be_bytes());
        udp[2..4].copy_from_slice(&dst.port().to_be_bytes());
        udp[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
        udp[8..].copy_from_slice(payload);

        let mut pseudo = Vec::with_capacity(12 + udp_len);
        pseudo.extend_from_slice(&src_ip);
        pseudo.extend_from_slice(&dst_ip);
        pseudo.push(0);
        pseudo.push(protocol);
        pseudo.extend_from_slice(&(udp_len as u16).to_be_bytes());
        pseudo.extend_from_slice(udp);
        let udp_checksum = checksum(&pseudo);
        udp[6..8].copy_from_slice(&udp_checksum.to_be_bytes());

        pkt
    }

    fn udp_packet(src: SocketAddr, dst: SocketAddr, payload: &[u8]) -> Vec<u8> {
        ipv4_packet(src, dst, 17, payload, 20, 0)
    }

    fn client_packet(port: u16, dst: SocketAddr, payload: &[u8]) -> Vec<u8> {
        let client = SocketAddr::new(client().ip(), port);
        udp_packet(client, dst, payload)
    }

    // ------------------------------------------------------------------
    // The two tests called out verbatim in the task brief.
    // ------------------------------------------------------------------

    #[test]
    fn reply_source_port_equals_client_destination_port() {
        let reply = build_udp_reply(
            "1.1.1.1:443".parse().unwrap(),
            "10.10.10.1:53000".parse().unwrap(),
            b"x",
        );
        assert_eq!(parse_udp(&reply).unwrap().src.port(), 443);
    }

    #[test]
    fn table_is_bounded_by_evicting_oldest() {
        let mut f = UdpForwarder::new(2, Duration::from_secs(30));
        f.handle_outbound(&client_packet(53001, dst(), b"a"));
        f.handle_outbound(&client_packet(53002, dst(), b"b"));
        f.handle_outbound(&client_packet(53003, dst(), b"c"));
        assert_eq!(f.len(), 2);
    }

    // ------------------------------------------------------------------
    // Flow identity: the client's source port must survive into the socket.
    // ------------------------------------------------------------------

    #[test]
    fn outbound_flow_carries_the_client_source_port() {
        let mut f = UdpForwarder::new(16, Duration::from_secs(30));
        let out = f
            .handle_outbound(&client_packet(53000, dst(), b"query"))
            .expect("a valid packet is forwarded");

        // The caller binds its socket to this exact port: pin it explicitly so
        // this cannot degrade into an incidental property of the type.
        assert_eq!(out.flow.client.port(), 53000);
        assert_eq!(out.flow.client, "10.10.10.1:53000".parse().unwrap());
        assert_eq!(out.flow.dst, dst(), "flow destination is the real target");
        assert_eq!(out.dst, dst(), "the socket is dialled to the real target");
        assert_eq!(out.payload, b"query");
    }

    #[test]
    fn one_client_host_can_hold_several_flows_on_different_ports() {
        let mut f = UdpForwarder::new(16, Duration::from_secs(30));
        let first = f
            .handle_outbound(&client_packet(53000, dst(), b"first"))
            .unwrap();
        let second = f
            .handle_outbound(&client_packet(53001, dst(), b"second"))
            .unwrap();

        assert_eq!(f.len(), 2, "distinct source ports are distinct flows");
        assert_ne!(first.flow, second.flow);
        assert_eq!(first.flow.client.port(), 53000);
        assert_eq!(second.flow.client.port(), 53001);
    }

    #[test]
    fn flows_are_keyed_by_destination_too() {
        let mut f = UdpForwarder::new(16, Duration::from_secs(30));
        f.handle_outbound(&client_packet(53000, "1.1.1.1:443".parse().unwrap(), b"a"));
        f.handle_outbound(&client_packet(53000, "8.8.8.8:53".parse().unwrap(), b"b"));
        assert_eq!(f.len(), 2, "the same client port may talk to two targets");
    }

    // ------------------------------------------------------------------
    // Reply construction.
    // ------------------------------------------------------------------

    #[test]
    fn reply_is_addressed_from_destination_to_client() {
        let mut f = UdpForwarder::new(8, Duration::from_secs(30));
        let out = f
            .handle_outbound(&client_packet(53000, dst(), b"query"))
            .unwrap();

        let reply = f.handle_reply(out.flow, b"answer");
        let parsed = parse_udp(&reply).expect("the reply is a well-formed IPv4/UDP packet");
        assert_eq!(parsed.src, dst(), "source is the real destination");
        assert_eq!(
            parsed.dst,
            client(),
            "destination is the masqueraded client"
        );
        assert_eq!(parsed.payload, b"answer");
    }

    #[test]
    fn an_empty_reply_payload_round_trips() {
        let reply = build_udp_reply(dst(), client(), b"");
        let parsed = parse_udp(&reply).unwrap();
        assert!(parsed.payload.is_empty());
    }

    #[test]
    fn reply_checksums_match_an_independent_computation() {
        let payloads: [&[u8]; 4] = [b"", b"x", b"odd", b"even"];
        for payload in payloads {
            let reply = build_udp_reply(dst(), client(), payload);
            let ihl = usize::from(reply[0] & 0x0f) * 4;
            assert_eq!(ihl, 20, "the reply has no IP options");
            assert_eq!(reply[0] >> 4, 4, "IPv4");
            assert_eq!(reply[9], 17, "UDP");
            assert_eq!(
                usize::from(u16::from_be_bytes([reply[2], reply[3]])),
                reply.len(),
                "total length covers the whole packet"
            );

            // IPv4 header: the field is a fresh computation (with the field
            // zeroed, as a sender computes it), and including it makes the
            // header sum to zero (the receiver's check).
            let header_sum = u16::from_be_bytes([reply[10], reply[11]]);
            let mut zeroed_header = reply[..ihl].to_vec();
            zeroed_header[10] = 0;
            zeroed_header[11] = 0;
            assert_eq!(
                header_sum,
                checksum(&zeroed_header),
                "IPv4 header checksum field is correct"
            );
            assert_eq!(
                checksum(&reply[..ihl]),
                0,
                "IPv4 header passes the receiver check"
            );

            // UDP: rebuild the pseudo-header and checksum from the wire bytes,
            // with the checksum field zeroed, exactly as a sender would.
            let udp = &reply[ihl..];
            let udp_len = usize::from(u16::from_be_bytes([udp[4], udp[5]]));
            assert_eq!(udp_len, 8 + payload.len(), "UDP length field");

            let mut pseudo = Vec::new();
            pseudo.extend_from_slice(&reply[12..20]); // source + destination IP
            pseudo.extend_from_slice(&[0, 17]); // zero, protocol
            pseudo.extend_from_slice(&(udp_len as u16).to_be_bytes());

            let mut zeroed = udp[..udp_len].to_vec();
            zeroed[6] = 0;
            zeroed[7] = 0;
            let mut with_zeroed = pseudo.clone();
            with_zeroed.extend_from_slice(&zeroed);
            let expected = checksum(&with_zeroed);

            let field = u16::from_be_bytes([udp[6], udp[7]]);
            assert_eq!(field, expected, "UDP checksum field is correct");
            assert_ne!(field, 0, "IPv4 UDP checksums may not be omitted");

            let mut with_field = pseudo;
            with_field.extend_from_slice(&udp[..udp_len]);
            assert_eq!(
                checksum(&with_field),
                0,
                "UDP pseudo-header + datagram passes the receiver check"
            );

            assert_eq!(parse_udp(&reply).unwrap().payload, payload);
        }
    }

    #[test]
    fn reply_building_is_ipv4_only() {
        let v6: SocketAddr = "[2001:db8::1]:443".parse().unwrap();
        assert!(
            build_udp_reply(v6, client(), b"x").is_empty(),
            "an IPv4-only reply builder cannot address IPv6"
        );
    }

    #[test]
    fn an_oversized_payload_is_refused() {
        // 65535 - 20 (IPv4) - 8 (UDP) is the largest representable datagram.
        let payload = vec![0u8; 65508];
        assert!(build_udp_reply(dst(), client(), &payload).is_empty());
    }

    // ------------------------------------------------------------------
    // Parsing.
    // ------------------------------------------------------------------

    #[test]
    fn parse_udp_reads_ports_and_payload() {
        let pkt = udp_packet(client(), dst(), b"hello");
        let parsed = parse_udp(&pkt).expect("a valid packet parses");
        assert_eq!(parsed.src, client());
        assert_eq!(parsed.dst, dst());
        assert_eq!(parsed.payload, b"hello");
    }

    #[test]
    fn ip_options_are_skipped() {
        let pkt = ipv4_packet(client(), dst(), 17, b"hello", 24, 0);
        let parsed = parse_udp(&pkt).expect("IHL 6 is valid");
        assert_eq!(parsed.src, client());
        assert_eq!(parsed.dst, dst());
        assert_eq!(parsed.payload, b"hello");
    }

    #[test]
    fn trailing_padding_beyond_the_udp_length_is_ignored() {
        let mut pkt = udp_packet(client(), dst(), b"hello");
        pkt.extend_from_slice(b"pad!");
        // The padding is inside the IP packet but outside the UDP datagram, as
        // with link-layer padding; the UDP length field is authoritative.
        let total = pkt.len();
        pkt[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        pkt[10..12].fill(0);
        let ip_checksum = checksum(&pkt[..20]);
        pkt[10..12].copy_from_slice(&ip_checksum.to_be_bytes());

        assert_eq!(parse_udp(&pkt).unwrap().payload, b"hello");
    }

    #[test]
    fn donotfragment_is_not_treated_as_fragmentation() {
        let pkt = ipv4_packet(client(), dst(), 17, b"x", 20, 0x4000);
        assert_eq!(parse_udp(&pkt).unwrap().payload, b"x");
    }

    fn malformed_packets() -> Vec<(&'static str, Vec<u8>)> {
        let base = udp_packet(client(), dst(), b"data");
        let mut cases: Vec<(&'static str, Vec<u8>)> = Vec::new();

        cases.push(("empty", Vec::new()));
        cases.push(("shorter than an IPv4 header", base[..10].to_vec()));
        cases.push(("truncated UDP header", base[..24].to_vec()));
        cases.push(("IPv6", {
            let mut p = base.clone();
            p[0] = 0x60;
            p
        }));
        cases.push(("IHL below the minimum", {
            let mut p = base.clone();
            p[0] = 0x44;
            p
        }));
        cases.push(("IHL past the end of the packet", {
            let mut p = base.clone();
            p[0] = 0x4f;
            p
        }));
        cases.push(("total length past the end of the packet", {
            let mut p = base.clone();
            p[2..4].copy_from_slice(&9999u16.to_be_bytes());
            p
        }));
        cases.push(("total length below header + UDP header", {
            let mut p = base.clone();
            p[2..4].copy_from_slice(&16u16.to_be_bytes());
            p
        }));
        cases.push((
            "protocol is TCP, not UDP",
            ipv4_packet(client(), dst(), 6, b"data", 20, 0),
        ));
        cases.push(("fragment offset is nonzero", {
            let mut p = base.clone();
            p[6..8].copy_from_slice(&1u16.to_be_bytes());
            p
        }));
        cases.push(("more-fragments flag is set", {
            let mut p = base.clone();
            p[6..8].copy_from_slice(&0x2000u16.to_be_bytes());
            p
        }));
        cases.push(("UDP length below its own header", {
            let mut p = base.clone();
            p[24..26].copy_from_slice(&7u16.to_be_bytes());
            p
        }));
        cases.push(("UDP length past the IP payload", {
            let mut p = base.clone();
            let too_long = p.len() as u16 - 20 + 1;
            p[24..26].copy_from_slice(&too_long.to_be_bytes());
            p
        }));
        cases.push(("UDP length zero", {
            let mut p = base.clone();
            p[24..26].copy_from_slice(&0u16.to_be_bytes());
            p
        }));

        cases
    }

    #[test]
    fn malformed_packets_are_dropped_without_panicking() {
        let mut f = UdpForwarder::new(8, Duration::from_secs(30));
        for (name, pkt) in malformed_packets() {
            assert!(
                parse_udp(&pkt).is_none(),
                "{name}: parse_udp must reject the packet"
            );
            assert!(
                f.handle_outbound(&pkt).is_none(),
                "{name}: handle_outbound must drop the packet"
            );
        }
        assert_eq!(f.len(), 0, "a malformed packet never creates a flow");
    }

    #[test]
    fn a_packet_that_is_all_zeroes_is_dropped() {
        assert!(parse_udp(&[0u8; 60]).is_none());
    }

    // ------------------------------------------------------------------
    // Bounded table: capacity eviction and idle expiry are independent.
    // ------------------------------------------------------------------

    #[test]
    fn eviction_drops_the_least_recently_active_flow() {
        let mut f = UdpForwarder::new(2, Duration::from_secs(30));
        let a = f
            .handle_outbound(&client_packet(53001, dst(), b"a"))
            .unwrap();
        let b = f
            .handle_outbound(&client_packet(53002, dst(), b"b"))
            .unwrap();

        // A is refreshed by a reply and by a further outbound packet, so B is
        // now the least recently active flow in the table.
        f.handle_reply(a.flow, b"reply");
        f.handle_outbound(&client_packet(53001, dst(), b"a again"));

        f.handle_outbound(&client_packet(53003, dst(), b"c"));

        assert_eq!(f.len(), 2);
        assert!(
            f.flows.contains_key(&a.flow),
            "a flow that is still moving traffic must not be evicted"
        );
        assert!(
            !f.flows.contains_key(&b.flow),
            "the least recently active flow is the one evicted"
        );
    }

    #[test]
    fn resending_an_existing_flow_does_not_grow_the_table() {
        let mut f = UdpForwarder::new(4, Duration::from_secs(30));
        for _ in 0..5 {
            f.handle_outbound(&client_packet(53000, dst(), b"again"));
        }
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn a_full_table_does_not_block_a_repeated_flow() {
        let mut f = UdpForwarder::new(1, Duration::from_secs(30));
        f.handle_outbound(&client_packet(53000, dst(), b"a"));
        let again = f.handle_outbound(&client_packet(53000, dst(), b"a"));
        assert!(
            again.is_some(),
            "the existing flow is not evicted for itself"
        );
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn zero_capacity_drops_flows() {
        let mut f = UdpForwarder::new(0, Duration::from_secs(30));
        assert!(
            f.handle_outbound(&client_packet(53000, dst(), b"a"))
                .is_none()
        );
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn expire_removes_idle_flows_and_keeps_fresh_ones() {
        let mut f = UdpForwarder::new(8, Duration::from_secs(30));
        let fresh = f
            .handle_outbound(&client_packet(53001, dst(), b"a"))
            .unwrap();
        let idle = f
            .handle_outbound(&client_packet(53002, dst(), b"b"))
            .unwrap();

        f.expire(Instant::now());
        assert_eq!(f.len(), 2, "a just-created flow is not idle");

        let now = Instant::now();
        f.backdate(&idle.flow, now - Duration::from_secs(31));
        f.expire(now);

        assert_eq!(f.len(), 1);
        assert!(f.flows.contains_key(&fresh.flow), "the fresh flow survives");
        assert!(
            !f.flows.contains_key(&idle.flow),
            "the idle flow is removed"
        );
    }

    #[test]
    fn a_reply_counts_as_activity_for_expiry() {
        let mut f = UdpForwarder::new(8, Duration::from_secs(30));
        let flow = f
            .handle_outbound(&client_packet(53000, dst(), b"a"))
            .unwrap()
            .flow;

        // Pretend the flow has already been idle for a while; the reply should
        // refresh it, just as an outbound packet would.
        f.backdate(&flow, Instant::now() - Duration::from_secs(20));
        let _ = f.handle_reply(flow, b"reply");

        f.expire(Instant::now() + Duration::from_secs(20));
        assert_eq!(f.len(), 1, "a flow that just replied is still live");
    }

    #[test]
    fn an_outbound_packet_counts_as_activity_for_expiry() {
        let mut f = UdpForwarder::new(8, Duration::from_secs(30));
        let flow = f
            .handle_outbound(&client_packet(53000, dst(), b"a"))
            .unwrap()
            .flow;

        f.backdate(&flow, Instant::now() - Duration::from_secs(20));
        f.handle_outbound(&client_packet(53000, dst(), b"a again"));

        f.expire(Instant::now() + Duration::from_secs(20));
        assert_eq!(f.len(), 1, "a flow that is still sending is still live");
    }

    #[test]
    fn expiry_frees_capacity_for_new_flows() {
        let mut f = UdpForwarder::new(1, Duration::from_secs(30));
        f.handle_outbound(&client_packet(53001, dst(), b"a"));
        f.expire(Instant::now() + Duration::from_secs(31));

        let next = f.handle_outbound(&client_packet(53002, dst(), b"b"));
        assert!(next.is_some());
        assert_eq!(f.len(), 1);
        assert!(f.flows.contains_key(&next.unwrap().flow));
    }

    #[test]
    fn an_evicted_flow_can_still_have_its_reply_built() {
        // Expiring the bookkeeping entry must not blackhole a flow whose
        // socket is still alive: the FlowId carries everything the reply needs.
        let mut f = UdpForwarder::new(1, Duration::from_secs(30));
        let first = f
            .handle_outbound(&client_packet(53001, dst(), b"a"))
            .unwrap();
        f.handle_outbound(&client_packet(53002, dst(), b"b"));

        let reply = f.handle_reply(first.flow, b"late");
        let parsed = parse_udp(&reply).expect("the reply is still built");
        assert_eq!(parsed.dst, first.flow.client);
        assert_eq!(parsed.payload, b"late");
    }

    // ------------------------------------------------------------------
    // Payload accounting and teardown.
    // ------------------------------------------------------------------

    #[test]
    fn payload_bytes_are_counted_in_both_directions() {
        let counters = Arc::new(StatsCounters::new());
        let mut f = UdpForwarder::with_counters(8, Duration::from_secs(30), Arc::clone(&counters));

        let out = f
            .handle_outbound(&client_packet(53000, dst(), b"ping"))
            .expect("a valid packet is forwarded");
        let _ = f.handle_reply(out.flow, b"pong");

        let stats = counters.snapshot(f.len());
        assert_eq!(stats.tx_bytes, 4, "the outbound payload is counted once");
        assert_eq!(stats.rx_bytes, 4, "the reply payload is counted once");
        assert_eq!(stats.active_flows, 1, "the flow is live");
    }

    #[test]
    fn a_reply_that_never_crosses_the_tunnel_is_not_counted_as_received() {
        let counters = Arc::new(StatsCounters::new());
        let mut f = UdpForwarder::with_counters(8, Duration::from_secs(30), Arc::clone(&counters));
        let flow = f
            .handle_outbound(&client_packet(53000, dst(), b"q"))
            .unwrap()
            .flow;

        // Over the tunnel MTU: dropped before it reaches the client.
        assert!(f.handle_reply_within_mtu(flow, &[0xab; 64], 64).is_none());
        // And one that cannot be represented as an IPv4 datagram at all.
        assert!(
            f.handle_reply_within_mtu(flow, &vec![0u8; 65508], 65535)
                .is_none()
        );

        let stats = counters.snapshot(f.len());
        assert_eq!(stats.rx_bytes, 0, "an undeliverable reply moves no bytes");
        assert_eq!(stats.tx_bytes, 1, "the request that did go out is counted");
    }

    #[test]
    fn removing_a_flow_is_immediate_and_frees_its_capacity() {
        let mut f = UdpForwarder::new(1, Duration::from_secs(30));
        let flow = f
            .handle_outbound(&client_packet(53000, dst(), b"a"))
            .unwrap()
            .flow;
        assert_eq!(f.len(), 1);

        assert!(f.remove(&flow), "the tracked flow is removed");
        assert_eq!(f.len(), 0, "the live count drops immediately");
        assert!(!f.remove(&flow), "removing again is harmless");

        // The eviction index was cleaned up too: a new flow fits without
        // evicting anything, and expiry cannot resurrect a stale entry.
        let next = f
            .handle_outbound(&client_packet(53001, dst(), b"b"))
            .expect("the freed capacity accepts a new flow");
        assert_eq!(f.len(), 1);
        f.expire(Instant::now() + Duration::from_secs(31));
        assert!(f.is_empty(), "only the new flow expires");
        assert_ne!(next.flow, flow);
    }

    #[test]
    fn tearing_down_all_flows_empties_the_table() {
        let mut f = UdpForwarder::new(8, Duration::from_secs(30));
        let a = f
            .handle_outbound(&client_packet(53001, dst(), b"a"))
            .unwrap()
            .flow;
        let b = f
            .handle_outbound(&client_packet(53002, dst(), b"b"))
            .unwrap()
            .flow;

        // The caller owns the outbound sockets, so it is handed every flow it
        // has to close.
        let torn = f.tear_down_all();
        assert_eq!(torn.len(), 2);
        assert!(torn.contains(&a) && torn.contains(&b));
        assert!(f.is_empty(), "no flow survives the teardown");
        assert!(
            f.tear_down_all().is_empty(),
            "tearing down an empty table is harmless"
        );

        // A flow that starts afterwards is tracked cleanly, with no stale
        // bookkeeping left behind.
        let again = f
            .handle_outbound(&client_packet(53001, dst(), b"a"))
            .unwrap();
        assert_eq!(again.flow, a);
        assert_eq!(f.len(), 1);
    }
}
