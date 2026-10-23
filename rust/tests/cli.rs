use std::process::Command;

#[test]
fn two_real_cli_processes_handshake_and_fetch_through_production_bridge() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        process::{Child, Stdio},
        sync::mpsc,
        time::Duration,
    };
    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = std::env::temp_dir().join(format!("cellbridge-process-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let router = cellbridge::generate_keypair();
    let phone = cellbridge::generate_keypair();
    let key = dir.join("router.key");
    std::fs::write(&key, &router.private_key).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut peer = Process(
        Command::new(env!("CARGO_BIN_EXE_standin_router"))
            .args(["serve", "--key-file"])
            .arg(&key)
            .args(["--peer", &phone.public_key, "--listen", "127.0.0.1:0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = peer.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let endpoint = loop {
        let line = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if let Some(addr) = line.strip_prefix("Listening: ") {
            break addr.to_string();
        }
    };
    let config = dir.join("phone.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&cellbridge::BridgeConfig {
            private_key: phone.private_key,
            peer_public_key: router.public_key,
            address: "10.10.10.2/24".into(),
            endpoint,
            mtu: 1420,
            preshared_key: None,
        })
        .unwrap(),
    )
    .unwrap();
    let mut bridge = Process(
        Command::new(env!("CARGO_BIN_EXE_bridge_launcher"))
            .arg("--config")
            .arg(&config)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut check = |command: &str| {
        writeln!(peer.0.stdin.as_mut().unwrap(), "{command}").unwrap();
        loop {
            let line = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            if let Ok(result) = serde_json::from_str::<serde_json::Value>(&line) {
                assert_eq!(result["passed"], true, "{line}");
                break;
            }
        }
    };
    check("handshake");
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap();
    let fixture = std::thread::spawn(move || {
        let (mut socket, _) = server.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = [0; 4];
        socket.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"ping");
        socket.write_all(b"pong").unwrap();
    });
    check(&format!("tcp {address} pong ping"));
    fixture.join().unwrap();
    writeln!(bridge.0.stdin.as_mut().unwrap(), "quit").unwrap();
    assert!(bridge.0.wait().unwrap().success());
    writeln!(peer.0.stdin.as_mut().unwrap(), "quit").unwrap();
    assert!(peer.0.wait().unwrap().success());
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn peer_ctrl_c_exits_even_when_stdin_is_still_open() {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let peer = env!("CARGO_BIN_EXE_standin_router");
    let dir = std::env::temp_dir().join(format!("cellbridge-signal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("peer.key");
    assert!(
        Command::new(peer)
            .args(["init", "--key-file"])
            .arg(&key)
            .output()
            .unwrap()
            .status
            .success()
    );
    let mut child = Command::new(peer)
        .args(["serve", "--key-file"])
        .arg(&key)
        .args([
            "--peer",
            &cellbridge::generate_keypair().public_key,
            "--listen",
            "127.0.0.1:0",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let exited = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !exited {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    std::fs::remove_dir_all(dir).unwrap();
    assert!(exited, "Ctrl-C waited indefinitely for stdin");
}

#[test]
fn tools_show_help_protect_keys_and_reject_malformed_config_without_echoing_secrets() {
    let peer = env!("CARGO_BIN_EXE_standin_router");
    let bridge = env!("CARGO_BIN_EXE_bridge_launcher");
    assert!(
        Command::new(peer)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        Command::new(bridge)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
    let dir = std::env::temp_dir().join(format!("cellbridge-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("peer.key");
    let result = Command::new(peer)
        .args(["init", "--key-file"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(result.status.success());
    let original = std::fs::read_to_string(&key).unwrap();
    assert!(!String::from_utf8_lossy(&result.stdout).contains(original.trim()));
    assert!(
        !Command::new(peer)
            .args(["init", "--key-file"])
            .arg(&key)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(std::fs::read_to_string(&key).unwrap(), original);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let config = dir.join("bad.json");
    std::fs::write(&config, "{secret-invalid-json").unwrap();
    let result = Command::new(bridge)
        .arg("--config")
        .arg(&config)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("secret-invalid-json"));
    std::fs::remove_dir_all(dir).unwrap();
}
