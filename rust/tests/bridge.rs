//! What an unattended bridge needs beyond the data path: byte counters that
//! reflect real traffic, a link state machine, and the teardown a lost tunnel
//! must trigger.
//!
//! Spec review item 4 lives here: the tunnel dies mid-traffic and every flow has
//! to go, so a client sees a reset instead of hanging on a half-open connection
//! — and, just as important, a *transient* session expiry must not be mistaken
//! for a loss, or a healthy link would drop its flows every rekey.

mod support;

use std::net::SocketAddr;
use std::time::Duration;
use std::time::Instant as StdInstant;

use boringtun::noise::errors::WireGuardError;

use cellbridge::{BridgeState, LinkEvent, TunnelError, build_udp_reply};

use support::*;

/// Drive the bridge loop — step, react, tick — until `done`, failing the test
/// rather than hanging.
async fn drive(
    bridge: &mut Bridge,
    tcp_client: &mut TcpClient,
    seen: &mut ClientSeen,
    what: &str,
    mut done: impl FnMut(&Bridge, &TcpClient, &ClientSeen) -> bool,
) {
    let deadline = StdInstant::now() + DRIVE_TIMEOUT;
    let mut last_tick = StdInstant::now();
    while !done(bridge, tcp_client, seen) {
        assert!(StdInstant::now() < deadline, "the bridge never {what}");

        let (to_client, _link) = bridge.step().await.expect("bridge step");
        client_react(
            &mut bridge.wire,
            tcp_client,
            TCP_CLIENT_ADDR,
            UDP_CLIENT_ADDR,
            &to_client,
            seen,
        );

        if last_tick.elapsed() >= TICK_INTERVAL {
            tick(bridge);
            last_tick = StdInstant::now();
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Advance the tunnel timers the way the bridge loop does.
///
/// A `ConnectionExpired` is not a lost uplink: boringtun hands the session back
/// to the handshake, so the loop carries on and the link state machine decides
/// from `is_connected()`. Every other error would be a real surprise.
fn tick(bridge: &mut Bridge) {
    match bridge.tick() {
        Ok(()) | Err(TunnelError::WireGuard(WireGuardError::ConnectionExpired)) => {}
        Err(err) => panic!("tunnel tick failed: {err}"),
    }
}

/// Inject the opening TCP SYN and UDP request for a fresh pair of destinations.
fn open_flows(
    bridge: &mut Bridge,
    tcp_dst: SocketAddr,
    udp_dst: SocketAddr,
) -> (TcpClient, ClientSeen) {
    let tcp_client = TcpClient::new(TCP_CLIENT_ADDR, tcp_dst);
    bridge
        .wire
        .send_from_client(&tcp_client.syn())
        .expect("inject the SYN");
    bridge
        .wire
        .send_from_client(&build_udp_reply(UDP_CLIENT_ADDR, udp_dst, b"ping"))
        .expect("inject the UDP datagram");
    (tcp_client, ClientSeen::default())
}

#[tokio::test]
async fn counters_count_the_bytes_that_actually_cross() {
    let (tcp_dst, tcp_server) = tcp_echo_server().await;
    let (udp_dst, udp_server) = udp_echo_server().await;

    let mut bridge = Bridge::new().await;
    assert_eq!(
        bridge.status().state,
        BridgeState::Connecting,
        "a started bridge is Connecting until the handshake completes"
    );
    bridge.wire.connect().await;
    let (mut tcp_client, mut seen) = open_flows(&mut bridge, tcp_dst, udp_dst);

    // Run until the round trip is done *and* the finished splice has been
    // reaped, so the flow count is what the tables say it is.
    drive(
        &mut bridge,
        &mut tcp_client,
        &mut seen,
        "finished the round trip",
        |bridge, tcp_client, seen| {
            tcp_client.finished()
                && seen.udp_reply.is_some()
                && bridge.splice.flow_count() == 0
                && bridge.status().state == BridgeState::Connected
        },
    )
    .await;

    assert_eq!(
        bridge.status().state,
        BridgeState::Connected,
        "the handshake completed, so the link is Connected"
    );
    assert_eq!(bridge.status().error, None);

    let stats = bridge.stats();
    assert_eq!(
        stats.tx_bytes, 9,
        "5 bytes of TCP request plus 4 of UDP request"
    );
    assert_eq!(
        stats.rx_bytes, 9,
        "5 bytes of TCP echo plus 4 of UDP reply"
    );
    assert_eq!(
        stats.active_flows,
        bridge.splice.flow_count() + bridge.forwarder.len(),
        "active flows are read from the structures that own them"
    );
    assert_eq!(
        stats.active_flows, 1,
        "the finished splice is gone; the UDP flow is still tracked"
    );

    tokio::time::timeout(Duration::from_secs(5), tcp_server)
        .await
        .expect("the TCP echo server saw the whole connection")
        .expect("TCP echo server task");
    tokio::time::timeout(Duration::from_secs(5), udp_server)
        .await
        .expect("the UDP echo server saw the datagram")
        .expect("UDP echo server task");
}

/// Spec review item 4: the tunnel dies mid-traffic.
#[tokio::test]
async fn tunnel_loss_tears_down_all_flows() {
    let (tcp_dst, tcp_server) = tcp_echo_server().await;
    let (udp_dst, _udp_server) = udp_echo_server().await;

    let mut bridge = Bridge::new_with_grace(Duration::from_millis(200)).await;
    bridge.wire.connect().await;
    let (mut tcp_client, mut seen) = open_flows(&mut bridge, tcp_dst, udp_dst);
    // The connection has to still be open when the uplink dies.
    tcp_client.keep_open();

    drive(
        &mut bridge,
        &mut tcp_client,
        &mut seen,
        "brought both flows up",
        |bridge, tcp_client, seen| {
            tcp_client.echoed()
                && seen.udp_reply.is_some()
                && bridge.splice.flow_count() == 1
                && bridge.forwarder.len() == 1
        },
    )
    .await;

    let handles_before = bridge.handles.clone();
    assert_eq!(handles_before.len(), 1, "one spliced connection");
    assert_eq!(
        bridge.stats().active_flows,
        2,
        "one TCP splice and one UDP flow are live"
    );

    // The uplink dies. The tunnel is gone, and says so.
    bridge.wire = Wire::new();
    assert!(
        !bridge.wire.gateway_side.is_connected(),
        "the replacement wire has no session"
    );

    // Inside the grace period nothing is torn down: a session that is
    // re-handshaking must not cost a healthy link its flows. The tunnel is not
    // ticked any more either — the uplink is gone.
    let deadline = StdInstant::now() + DRIVE_TIMEOUT;
    loop {
        assert!(
            StdInstant::now() < deadline,
            "the lost link was never declared"
        );
        let (_, link) = bridge.step().await.expect("bridge step");
        if link == LinkEvent::Lost {
            break;
        }
        assert_eq!(link, LinkEvent::Down, "down, but inside the grace period");
        assert_eq!(
            bridge.status().state,
            BridgeState::Connected,
            "a link that is down inside the grace period is not an error"
        );
        assert_eq!(
            bridge.splice.flow_count(),
            1,
            "no flow is torn down before the grace period elapses"
        );
        assert_eq!(bridge.forwarder.len(), 1);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let status = bridge.status();
    assert_eq!(status.state, BridgeState::Error);
    assert!(
        status.error.is_some(),
        "a lost uplink carries a human-readable reason"
    );

    // The loop's response to `LinkEvent::Lost`.
    let torn = bridge.tear_down_all();

    // `TcpSpliceEngine::closed_handles` covered every handle the engine held.
    assert_eq!(
        torn, handles_before,
        "every spliced handle is reported closed by the teardown"
    );
    assert_eq!(bridge.torn_down_udp.len(), 1, "the UDP flow is dropped too");
    assert_eq!(bridge.torn_down_udp[0].client, UDP_CLIENT_ADDR);

    // The client is reset, not left hanging and not told the response ended.
    let mut saw_reset = false;
    let mut saw_fin = false;
    for packet in &bridge.torn_down_packets {
        if let Some(segment) = parse_tcp(packet)
            && segment.dst == TCP_CLIENT_ADDR
        {
            saw_reset |= segment.flags & RST != 0;
            saw_fin |= segment.flags & FIN != 0;
        }
    }
    assert!(saw_reset, "the client sees a reset");
    assert!(
        !saw_fin,
        "a dead uplink must not look like a clean end of response"
    );

    // Nothing is left behind: the tables are empty, the counter agrees, and the
    // gateway has freed the connection's buffers.
    assert_eq!(bridge.splice.flow_count(), 0);
    assert!(bridge.forwarder.is_empty());
    assert_eq!(
        bridge.stats().active_flows,
        0,
        "a torn-down bridge has no live flows"
    );
    assert_eq!(
        bridge.stats().tx_bytes,
        9,
        "a teardown does not discard the totals"
    );
    assert_eq!(
        bridge.gateway.socket_count(),
        1,
        "only the inert ICMP socket is left once the splice's socket is freed"
    );

    // The real socket was closed, which is what the echo server was waiting for.
    tokio::time::timeout(Duration::from_secs(5), tcp_server)
        .await
        .expect("the real socket was closed rather than leaked")
        .expect("TCP echo server task");

    // --- a new flow starts clean -------------------------------------------
    let (tcp_dst, tcp_server) = tcp_echo_server().await;
    let (udp_dst, udp_server) = udp_echo_server().await;

    bridge.wire = Wire::new();
    bridge.wire.connect().await;
    let (mut tcp_client, mut seen) = open_flows(&mut bridge, tcp_dst, udp_dst);

    drive(
        &mut bridge,
        &mut tcp_client,
        &mut seen,
        "recovered with fresh flows",
        |bridge, tcp_client, seen| {
            bridge.status().state == BridgeState::Connected
                && tcp_client.echoed()
                && seen.udp_reply.is_some()
        },
    )
    .await;

    assert_eq!(
        bridge.splice.flow_count(),
        1,
        "the new splice is the only one tracked"
    );
    assert_eq!(bridge.forwarder.len(), 1, "the new UDP flow is tracked");
    assert_eq!(
        bridge.stats().active_flows,
        2,
        "nothing of the torn-down flows is inherited by the new ones"
    );
    assert_eq!(
        bridge.stats().tx_bytes,
        18,
        "the totals keep accumulating across the loss"
    );
    assert_eq!(
        bridge.accepted.len(),
        2,
        "the recovered connection was accepted, like the first"
    );

    // Let the recovered connection finish normally, so the echo server sees its
    // EOF and the splice is reaped.
    drive(
        &mut bridge,
        &mut tcp_client,
        &mut seen,
        "finished the recovered connection",
        |bridge, _, _| bridge.splice.flow_count() == 0,
    )
    .await;

    tokio::time::timeout(Duration::from_secs(5), tcp_server)
        .await
        .expect("the TCP echo server saw the whole connection")
        .expect("TCP echo server task");
    tokio::time::timeout(Duration::from_secs(5), udp_server)
        .await
        .expect("the UDP echo server saw the datagram")
        .expect("UDP echo server task");
}

/// The other half of the same decision: a link that is down and comes back
/// inside the grace period is a re-handshake, not a loss, so its flows survive.
#[tokio::test]
async fn an_outage_inside_the_grace_keeps_the_flows() {
    let (tcp_dst, tcp_server) = tcp_echo_server().await;
    let (udp_dst, udp_server) = udp_echo_server().await;

    let mut bridge = Bridge::new_with_grace(Duration::from_secs(2)).await;
    bridge.wire.connect().await;
    let (mut tcp_client, mut seen) = open_flows(&mut bridge, tcp_dst, udp_dst);
    tcp_client.keep_open();

    drive(
        &mut bridge,
        &mut tcp_client,
        &mut seen,
        "brought both flows up",
        |bridge, tcp_client, seen| {
            tcp_client.echoed()
                && seen.udp_reply.is_some()
                && bridge.splice.flow_count() == 1
                && bridge.forwarder.len() == 1
        },
    )
    .await;

    // Keep the connected wire so the link can come back.
    let live = std::mem::replace(&mut bridge.wire, Wire::new());

    // The session is gone well inside the grace period. The flows are not.
    for _ in 0..5 {
        let (_, link) = bridge.step().await.expect("bridge step");
        assert_eq!(link, LinkEvent::Down, "the link is down");
        assert_eq!(
            bridge.status().state,
            BridgeState::Connected,
            "a session that is re-handshaking is not an error"
        );
        assert_eq!(bridge.splice.flow_count(), 1, "the splice survives the gap");
        assert_eq!(bridge.forwarder.len(), 1, "the UDP flow survives the gap");
        assert_eq!(bridge.stats().active_flows, 2);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The link comes back, and the loop reports it once.
    bridge.wire = live;
    let (to_client, link) = bridge.step().await.expect("bridge step");
    assert_eq!(link, LinkEvent::Connected, "the link is back");
    assert_eq!(bridge.status().state, BridgeState::Connected);
    client_react(
        &mut bridge.wire,
        &mut tcp_client,
        TCP_CLIENT_ADDR,
        UDP_CLIENT_ADDR,
        &to_client,
        &mut seen,
    );

    // Both flows are still there, and the splice still works: the client
    // finishes its half-close and the real server sees the whole connection.
    assert_eq!(bridge.splice.flow_count(), 1, "the splice was never torn down");
    assert_eq!(bridge.forwarder.len(), 1);

    bridge
        .wire
        .send_from_client(&tcp_client.half_close())
        .expect("inject the FIN");

    drive(
        &mut bridge,
        &mut tcp_client,
        &mut seen,
        "finished the connection after the gap",
        |bridge, tcp_client, _| tcp_client.finished() && bridge.splice.flow_count() == 0,
    )
    .await;

    tokio::time::timeout(Duration::from_secs(5), tcp_server)
        .await
        .expect("the TCP echo server saw the whole connection")
        .expect("TCP echo server task");
    tokio::time::timeout(Duration::from_secs(5), udp_server)
        .await
        .expect("the UDP echo server saw the datagram")
        .expect("UDP echo server task");
}
