use cellbridge::{
    BridgeConfig, generate_keypair,
    test_peer::{PeerConfig, Scenario, TestPeer},
};
use std::net::Ipv4Addr;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};

static BRIDGE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn android_style_dual_stack_socket_accepts_ipv4_peer_replies() {
    use std::os::fd::IntoRawFd;
    let _serial = BRIDGE_LOCK.lock().await;
    let phone = generate_keypair();
    let router = generate_keypair();
    let peer = TestPeer::bind(PeerConfig {
        private_key: router.private_key,
        peer_public_key: phone.public_key,
        listen: "127.0.0.1:0".parse().unwrap(),
        mtu: 1420,
    })
    .await
    .unwrap()
    .with_timeout(Duration::from_secs(2));
    let endpoint = peer.local_addr().unwrap();
    let socket = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, None).unwrap();
    socket.set_only_v6(false).unwrap();
    socket
        .bind(&"[::]:0".parse::<std::net::SocketAddr>().unwrap().into())
        .unwrap();
    let (tx, rx) = mpsc::channel(1);
    let (results, mut result_rx) = mpsc::channel(1);
    let task = tokio::spawn(peer.run(rx, results));
    cellbridge::start_bridge(
        BridgeConfig {
            private_key: phone.private_key,
            peer_public_key: router.public_key,
            address: "10.10.10.2/24".into(),
            endpoint: endpoint.to_string(),
            mtu: 1420,
            preshared_key: None,
        },
        Some(socket.into_raw_fd()),
    )
    .await
    .unwrap();
    tx.send(Scenario::Handshake).await.unwrap();
    let result = result_rx.recv().await.unwrap();
    cellbridge::stop_bridge().await.unwrap();
    drop(tx);
    task.await.unwrap();
    assert!(
        result.passed,
        "dual-stack fd handshake failed: {}",
        result.message
    );
}

#[tokio::test]
async fn persistent_peer_reauthenticates_after_bridge_changes_source_port() {
    use std::os::fd::IntoRawFd;
    let _serial = BRIDGE_LOCK.lock().await;
    let phone = generate_keypair();
    let router = generate_keypair();
    let peer = TestPeer::bind(PeerConfig {
        private_key: router.private_key,
        peer_public_key: phone.public_key,
        listen: "127.0.0.1:0".parse().unwrap(),
        mtu: 1420,
    })
    .await
    .unwrap()
    .with_timeout(Duration::from_millis(800));
    let endpoint = peer.local_addr().unwrap();
    let cfg = BridgeConfig {
        private_key: phone.private_key,
        peer_public_key: router.public_key,
        address: "10.10.10.2/24".into(),
        endpoint: endpoint.to_string(),
        mtu: 1420,
        preshared_key: None,
    };
    let (tx, rx) = mpsc::channel(1);
    let (results, mut result_rx) = mpsc::channel(1);
    let task = tokio::spawn(peer.run(rx, results));
    let first = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let second = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    assert_ne!(first.local_addr().unwrap(), second.local_addr().unwrap());
    let udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let destination = udp.local_addr().unwrap();
    let fixture = tokio::spawn(async move {
        let mut buffer = [0; 64];
        for _ in 0..2 {
            let (n, from) = udp.recv_from(&mut buffer).await.unwrap();
            udp.send_to(&buffer[..n], from).await.unwrap();
        }
    });
    for socket in [first, second] {
        cellbridge::start_bridge(cfg.clone(), Some(socket.into_raw_fd()))
            .await
            .unwrap();
        tx.send(Scenario::Handshake).await.unwrap();
        let result = result_rx.recv().await.unwrap();
        assert!(result.passed);
        tokio::time::sleep(Duration::from_millis(100)).await;
        tx.send(Scenario::Udp {
            destination,
            payload: b"reconnect".to_vec(),
            expected: b"reconnect".to_vec(),
        })
        .await
        .unwrap();
        let result = result_rx.recv().await.unwrap();
        cellbridge::stop_bridge().await.unwrap();
        assert!(
            result.passed,
            "persistent peer could not follow authenticated reconnect: {}",
            result.message
        );
        let forged = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        forged.send_to(&[0; 148], endpoint).await.unwrap();
    }
    fixture.await.unwrap();
    drop(tx);
    task.await.unwrap();
}

#[tokio::test]
async fn real_bridge_carries_multisegment_tcp_udp_and_rejects_bad_expectations() {
    let _serial = BRIDGE_LOCK.lock().await;
    let phone = generate_keypair();
    let router = generate_keypair();
    let peer = TestPeer::bind(PeerConfig {
        private_key: router.private_key,
        peer_public_key: phone.public_key,
        listen: "127.0.0.1:0".parse().unwrap(),
        mtu: 1420,
    })
    .await
    .unwrap();
    let endpoint = peer.local_addr().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (result_tx, mut result_rx) = mpsc::channel(8);
    let peer_task = tokio::spawn(peer.run(cmd_rx, result_tx));
    // Real transport fault injection: drop one encrypted data packet and
    // duplicate subsequent ones. TCP must retransmit; WireGuard must reject
    // replay without corrupting the inner stream.
    let relay = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let relay_task = tokio::spawn(async move {
        let mut phone_addr = None;
        let mut dropped = false;
        let mut packet = vec![0; 65535];
        loop {
            let (n, from) = relay.recv_from(&mut packet).await.unwrap();
            let target = if from == endpoint {
                phone_addr
            } else {
                phone_addr = Some(from);
                Some(endpoint)
            };
            if let Some(target) = target {
                let data = n > 32 && packet[..4] == [4, 0, 0, 0];
                if from == endpoint && data && !dropped {
                    dropped = true;
                    continue;
                }
                relay.send_to(&packet[..n], target).await.unwrap();
                if data {
                    relay.send_to(&packet[..n], target).await.unwrap();
                }
            }
        }
    });
    let cfg = BridgeConfig {
        private_key: phone.private_key,
        peer_public_key: router.public_key,
        address: "10.10.10.2/24".into(),
        endpoint: relay_addr.to_string(),
        mtu: 1420,
        preshared_key: None,
    };
    cellbridge::start_bridge(cfg, None).await.unwrap();
    cmd_tx.send(Scenario::Handshake).await.unwrap();
    assert!(result_rx.recv().await.unwrap().passed);
    let attacker = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    attacker.send_to(&[0; 148], endpoint).await.unwrap();

    let tcp = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let tcp_addr = tcp.local_addr().unwrap();
    let payload = vec![b'Q'; 12_000];
    let response = payload.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = tcp.accept().await.unwrap();
        let mut request = [0; 4];
        stream.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"test");
        stream.write_all(&response).await.unwrap();
        stream.shutdown().await.unwrap();
    });
    cmd_tx
        .send(Scenario::Tcp {
            destination: tcp_addr,
            request: b"test".to_vec(),
            expected: payload.clone(),
        })
        .await
        .unwrap();
    let result = result_rx.recv().await.unwrap();
    assert!(result.passed, "{}", result.message);
    assert_eq!(result.received_bytes, payload.len());
    server.await.unwrap();

    let udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_addr = udp.local_addr().unwrap();
    let udp_server = tokio::spawn(async move {
        let mut buffer = [0; 64];
        for _ in 0..2 {
            let (n, addr) = udp.recv_from(&mut buffer).await.unwrap();
            udp.send_to(&buffer[..n], addr).await.unwrap();
        }
    });
    for (expected, pass) in [(b"ping".to_vec(), true), (b"wrong".to_vec(), false)] {
        cmd_tx
            .send(Scenario::Udp {
                destination: udp_addr,
                payload: b"ping".to_vec(),
                expected,
            })
            .await
            .unwrap();
        assert_eq!(result_rx.recv().await.unwrap().passed, pass);
    }
    udp_server.await.unwrap();
    cellbridge::stop_bridge().await.unwrap();
    relay_task.abort();
    drop(cmd_tx);
    tokio::time::timeout(Duration::from_secs(2), peer_task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn unconnected_peer_times_out_and_ignores_forged_packets() {
    let peer = TestPeer::bind(PeerConfig {
        private_key: generate_keypair().private_key,
        peer_public_key: generate_keypair().public_key,
        listen: "127.0.0.1:0".parse().unwrap(),
        mtu: 1420,
    })
    .await
    .unwrap()
    .with_timeout(Duration::from_millis(100));
    let addr = peer.local_addr().unwrap();
    let (tx, rx) = mpsc::channel(1);
    let (results, mut result_rx) = mpsc::channel(1);
    let task = tokio::spawn(peer.run(rx, results));
    let attacker = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    attacker.send_to(&[0; 148], addr).await.unwrap();
    tx.send(Scenario::Handshake).await.unwrap();
    let result = result_rx.recv().await.unwrap();
    assert!(!result.passed);
    assert!(result.message.contains("timed out"));
    drop(tx);
    task.await.unwrap();
}
