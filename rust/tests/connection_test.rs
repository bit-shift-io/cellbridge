use cellbridge::{
    BridgeConfig, connection_test, generate_keypair,
    test_peer::{PeerConfig, TestPeer},
};
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::test]
async fn one_shot_authenticates_without_forwarding_or_changing_service_state() {
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
    let cfg = BridgeConfig {
        private_key: phone.private_key,
        peer_public_key: router.public_key,
        endpoint: peer.local_addr().unwrap().to_string(),
        address: "10.10.10.2/24".into(),
        mtu: 1420,
        preshared_key: None,
    };
    let (tx, rx) = mpsc::channel(1);
    let (results, _rx) = mpsc::channel(1);
    let task = tokio::spawn(peer.run(rx, results));
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    connection_test::handshake(cfg, socket, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(cellbridge::native::snapshot_json().contains("\"state\":\"idle\""));
    assert!(
        std::net::UdpSocket::bind(address).is_ok(),
        "test socket was not closed"
    );
    drop(tx);
    task.await.unwrap();
}

#[tokio::test]
async fn one_shot_is_bounded_and_closes_socket_when_peer_is_absent() {
    let phone = generate_keypair();
    let router = generate_keypair();
    let absent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let cfg = BridgeConfig {
        private_key: phone.private_key,
        peer_public_key: router.public_key,
        endpoint: absent.local_addr().unwrap().to_string(),
        address: "10.10.10.2/24".into(),
        mtu: 1420,
        preshared_key: None,
    };
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    let error = connection_test::handshake(cfg, socket, Duration::from_millis(80))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        "No authenticated response from peer (connection test timed out)"
    );
    assert!(std::net::UdpSocket::bind(address).is_ok());
}
