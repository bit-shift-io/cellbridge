use std::net::UdpSocket;
use std::os::fd::IntoRawFd;
use std::sync::Mutex;

use cellbridge::{BridgeConfig, BridgeState, generate_keypair, native};

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn invalid_endpoint_keeps_an_actionable_secret_free_diagnostic() {
    let _serial = SERIAL.lock().unwrap();
    let mut cfg = config();
    cfg.endpoint = "secret-invalid-endpoint".into();
    assert_eq!(
        native::start(cfg, None).unwrap_err(),
        "Invalid peer endpoint"
    );
    let snapshot = native::snapshot_json();
    assert!(snapshot.contains("Invalid peer endpoint"));
    assert!(!snapshot.contains("secret-invalid-endpoint"));
}

fn config() -> BridgeConfig {
    BridgeConfig {
        private_key: generate_keypair().private_key,
        peer_public_key: generate_keypair().public_key,
        address: "10.10.10.2/24".into(),
        endpoint: "127.0.0.1:51820".into(),
        preshared_key: None,
        mtu: 1420,
    }
}

#[test]
fn native_runtime_survives_callers_and_can_restart() {
    let _serial = SERIAL.lock().unwrap();
    native::stop().unwrap();
    for _ in 0..2 {
        native::start(config(), None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(cellbridge::status().state, BridgeState::Connecting);
        assert!(native::start(config(), None).is_err());
        native::stop().unwrap();
        assert_eq!(cellbridge::status().state, BridgeState::Idle);
    }
    native::stop().unwrap();
    let events = cellbridge::diagnostics::snapshot();
    assert!(events.iter().any(|event| event.message == "Connecting"));
    assert_eq!(events.last().unwrap().message, "Idle");
}

#[test]
fn failed_validation_releases_transferred_socket() {
    let _serial = SERIAL.lock().unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    let mut cfg = config();
    cfg.private_key = "secret-invalid-key".into();
    let error = native::start(cfg, Some(socket.into_raw_fd())).unwrap_err();
    assert_eq!(error, "Invalid phone private key");
    assert!(!error.contains("secret-invalid-key"));
    assert!(
        UdpSocket::bind(address).is_ok(),
        "failed start leaked its fd"
    );
}

#[test]
fn native_json_boundary_rejects_malformed_settings_and_keeps_snapshot_readable() {
    let _serial = SERIAL.lock().unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    let error = native::start_json(
        "{\"privateKey\":\"secret\",\"mtu\":\"wrong\"}",
        socket.into_raw_fd(),
    )
    .unwrap_err();
    assert!(!error.contains("secret"));
    assert!(UdpSocket::bind(address).is_ok());
    let snapshot: serde_json::Value = serde_json::from_str(&native::snapshot_json()).unwrap();
    assert_eq!(snapshot["state"], "idle");
    assert_eq!(snapshot["txBytes"], "0");
    assert!(snapshot["diagnostics"].is_array());
}

#[test]
fn duplicate_start_releases_only_new_socket_and_stop_releases_original() {
    let _serial = SERIAL.lock().unwrap();
    native::stop().unwrap();
    let first = UdpSocket::bind("127.0.0.1:0").unwrap();
    let first_address = first.local_addr().unwrap();
    native::start(config(), Some(first.into_raw_fd())).unwrap();
    let second = UdpSocket::bind("127.0.0.1:0").unwrap();
    let second_address = second.local_addr().unwrap();
    assert!(native::start(config(), Some(second.into_raw_fd())).is_err());
    let replacement = UdpSocket::bind(second_address).expect("duplicate start leaked fd");
    assert!(UdpSocket::bind(first_address).is_err());
    native::stop().unwrap();
    assert!(UdpSocket::bind(first_address).is_ok());
    assert!(
        UdpSocket::bind(second_address).is_err(),
        "stop closed an unrelated fd"
    );
    drop(replacement);
}
