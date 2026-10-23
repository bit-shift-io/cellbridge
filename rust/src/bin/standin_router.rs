use cellbridge::test_peer::{PeerConfig, Scenario, TestPeer};
use std::{fs::OpenOptions, io::Write, path::Path};
use tokio::sync::mpsc;
mod common;

const HELP: &str = "standin_router init --key-file PATH\nstandin_router serve --key-file PATH --peer PUBLIC_KEY --listen IP:PORT\nCommands: handshake | tcp IP:PORT EXPECT REQUEST | udp IP:PORT EXPECT PAYLOAD | dns RESOLVER:PORT NAME | quit\nREQUEST/PAYLOAD/EXPECT are single tokens. Use http IP:PORT HOST PATH for a public-IP fetch.\nNo routes, interfaces or root privileges required.";

fn option<'a>(args: &'a [String], name: &str) -> Result<&'a str, String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
        .ok_or_else(|| format!("missing {name}"))
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        println!("{HELP}");
        return Ok(());
    }
    let mode = args.first().map(String::as_str).ok_or(HELP)?;
    let path = Path::new(option(&args, "--key-file")?);
    if mode == "init" {
        let keys = cellbridge::generate_keypair();
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .map_err(|_| "cannot create key file (it may already exist)")?;
        writeln!(file, "{}", keys.private_key).map_err(|_| "cannot write key file")?;
        file.sync_all().map_err(|_| "cannot persist key file")?;
        println!("Public key: {}", keys.public_key);
        return Ok(());
    }
    if mode != "serve" {
        return Err(HELP.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path)
            .map_err(|_| "cannot read key metadata")?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("key file must be owner-only (chmod 600)".into());
        }
    }
    let private_key = std::fs::read_to_string(path)
        .map_err(|_| "cannot read key file")?
        .trim()
        .to_string();
    let public_key =
        cellbridge::derive_public_key(private_key.clone()).map_err(|_| "invalid key file")?;
    let peer = TestPeer::bind(PeerConfig {
        private_key,
        peer_public_key: option(&args, "--peer")?.into(),
        listen: option(&args, "--listen")?
            .parse()
            .map_err(|_| "invalid listen IP:port")?,
        mtu: 1420,
    })
    .await?;
    println!(
        "Public key: {public_key}\nListening: {}\n{HELP}",
        peer.local_addr().map_err(|e| e.to_string())?
    );
    let (tx, rx) = mpsc::channel(8);
    let (results, mut result_rx) = mpsc::channel(8);
    let task = tokio::spawn(peer.run(rx, results));
    let mut lines = common::input();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            result = result_rx.recv() => {
                let Some(result) = result else { break };
                println!("{}", serde_json::to_string(&result).map_err(|_| "result serialization failed")?);
            }
            line = lines.recv() => {
                let Some(line) = line else { break };
                if line.trim() == "quit" { break }
                match command(&line) {
                    Ok(scenario) => { tx.send(scenario).await.map_err(|_| "peer stopped")?; }
                    Err(error) => println!("{}", serde_json::json!({"name":"command", "passed":false,"message":error})),
                }
            }
        }
    }
    drop(tx);
    task.await.map_err(|_| "peer task failed")?;
    Ok(())
}

fn command(line: &str) -> Result<Scenario, String> {
    let parts: Vec<_> = line.split_whitespace().collect();
    let address = |s: &str| {
        s.parse::<std::net::SocketAddr>()
            .map_err(|_| "expected numeric IPv4:port".to_string())
    };
    match parts.as_slice() {
        ["handshake"] => Ok(Scenario::Handshake),
        ["tcp", dst, expect, send] => Ok(Scenario::Tcp {
            destination: address(dst)?,
            request: send.as_bytes().to_vec(),
            expected: expect.as_bytes().to_vec(),
        }),
        ["udp", dst, expect, send] => Ok(Scenario::Udp {
            destination: address(dst)?,
            payload: send.as_bytes().to_vec(),
            expected: expect.as_bytes().to_vec(),
        }),
        ["dns", dst, name] => Ok(Scenario::Dns {
            resolver: address(dst)?,
            name: (*name).into(),
        }),
        ["http", dst, host, path] => Ok(Scenario::Fetch {
            destination: address(dst)?,
            host: (*host).into(),
            path: (*path).into(),
        }),
        _ => Err("unknown command; see --help".into()),
    }
}
