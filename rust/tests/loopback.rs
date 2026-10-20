//! Full loopback integration: WireGuard on both ends, the gateway in the
//! middle, and real TCP/UDP sockets on the far side.
//!
//! The harness lives in [`support`]; this file drives it through the happy path
//! and pins the properties that only a real round trip can show: the client's
//! source port survives into the UDP reply, the TCP half-close crosses the
//! splice, and replies larger than the tunnel MTU are dropped before they are
//! encapsulated.

mod support;

use std::net::SocketAddr;
use std::time::Duration;
use std::time::Instant as StdInstant;

use smoltcp::time::Instant;

use cellbridge::{Gateway, Routed, UdpForwarder, build_udp_reply};

use support::*;

#[tokio::test]
async fn tcp_and_udp_roundtrip_through_tunnel() {
    let (tcp_dst, tcp_server) = tcp_echo_server().await;
    let (udp_dst, udp_server) = udp_echo_server().await;

    let mut bridge = Bridge::new().await;
    bridge.wire.connect().await;

    let mut tcp_client = TcpClient::new(TCP_CLIENT_ADDR, tcp_dst);
    bridge
        .wire
        .send_from_client(&tcp_client.syn())
        .expect("inject the SYN");
    let udp_request_packet = build_udp_reply(UDP_CLIENT_ADDR, udp_dst, b"ping");
    bridge
        .wire
        .send_from_client(&udp_request_packet)
        .expect("inject the UDP datagram");

    let mut seen = ClientSeen::default();
    let mut last_tick = StdInstant::now();

    let deadline = StdInstant::now() + DRIVE_TIMEOUT;
    while !(tcp_client.finished() && seen.udp_reply.is_some()) {
        assert!(
            StdInstant::now() < deadline,
            "loopback did not finish: tcp_finished={}, udp_reply={}",
            tcp_client.finished(),
            seen.udp_reply.is_some()
        );

        let (to_client, _link) = bridge.step().await.expect("bridge step");
        client_react(
            &mut bridge.wire,
            &mut tcp_client,
            TCP_CLIENT_ADDR,
            UDP_CLIENT_ADDR,
            &to_client,
            &mut seen,
        );

        if last_tick.elapsed() >= TICK_INTERVAL {
            bridge.tick().expect("tunnel tick");
            last_tick = StdInstant::now();
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    // --- TCP assertions -----------------------------------------------------
    assert!(
        tcp_client.echoed(),
        "the TCP payload round-tripped through the tunnel"
    );
    assert!(
        tcp_client.finished(),
        "the spliced connection closed cleanly"
    );
    assert_eq!(
        bridge.accepted,
        vec![(TCP_CLIENT_ADDR, tcp_dst)],
        "the gateway accepted exactly the client's connection, for the destination it asked for"
    );

    // --- UDP assertions -----------------------------------------------------
    assert!(
        bridge.routed_udp,
        "the gateway demultiplexed UDP before smoltcp"
    );
    assert_eq!(seen.udp_reply.as_deref(), Some(b"pong".as_slice()));
    let source = seen.udp_reply_source.expect("the UDP reply has a source");
    let destination = seen.udp_reply_dst.expect("the UDP reply has a destination");
    assert_eq!(
        source.port(),
        udp_dst.port(),
        "the reply's source port is the destination port that was used"
    );
    assert_eq!(source, udp_dst, "the reply comes from the real destination");
    assert_eq!(
        destination, UDP_CLIENT_ADDR,
        "the reply is addressed to the client"
    );

    let flow = bridge.udp_flow.expect("the forwarder recorded a flow");
    assert_eq!(
        flow.client.port(),
        UDP_CLIENT_PORT,
        "the flow preserves the client's original source port"
    );
    assert_eq!(flow.client, UDP_CLIENT_ADDR);
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
