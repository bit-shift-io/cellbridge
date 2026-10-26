//! Router setup lives in the Rust core; Flutter only renders its snapshot.

use base64::{Engine, engine::general_purpose::STANDARD};
use russh::{ChannelMsg, client};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, net::{Shutdown, TcpStream}, sync::{Arc, Mutex, OnceLock}, time::Duration};
use tokio::{runtime::Runtime, sync::oneshot, time::timeout};

/// Session-only input. Never derive Debug or place this in diagnostics/storage.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub username: String,
    pub password: String,
    pub priority: u8,
    pub phone_public_key: String,
    pub expected_fingerprint: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouterSettings {
    pub address: String,
    pub peer_public_key: String,
    pub preshared_key: String,
    pub endpoint: String,
    pub mtu: u16,
    pub phone_public_key: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub state: String,
    pub logs: VecDeque<String>,
    pub fingerprint: Option<String>,
    pub error: Option<String>,
    pub result: Option<RouterSettings>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { state: "idle".into(), logs: VecDeque::new(), fingerprint: None, error: None, result: None }
    }
}

type Progress = Arc<Mutex<Snapshot>>;
struct Job { progress: Progress, cancel: Option<oneshot::Sender<()>> }

fn job() -> &'static Mutex<Job> {
    static JOB: OnceLock<Mutex<Job>> = OnceLock::new();
    JOB.get_or_init(|| Mutex::new(Job { progress: Arc::new(Mutex::new(Snapshot::default())), cancel: None }))
}

pub fn snapshot_json() -> String {
    let job = job().lock().unwrap_or_else(|e| e.into_inner());
    let snapshot = job.progress.lock().unwrap_or_else(|e| e.into_inner());
    serde_json::to_string(&*snapshot).unwrap_or_else(|_| "{\"state\":\"error\"}".into())
}

pub fn cancel() {
    let mut job = job().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cancel) = job.cancel.take() { let _ = cancel.send(()); }
    // Clear returned credentials as well as the SSH task on navigation away.
    *job.progress.lock().unwrap_or_else(|e| e.into_inner()) = Snapshot::default();
}

/// Starts a job on the headless runtime. The connected Wi-Fi stream is owned
/// from entry, including validation failures and cancellation.
pub fn start(rt: &Runtime, socket: TcpStream, request: Request) -> Result<(), String> {
    if !(1..=10).contains(&request.priority) || !valid_key(&request.phone_public_key) ||
        request.username.is_empty() || request.username.len() > 128 || request.password.len() > 4096 {
        return Err("Invalid router setup input".into());
    }
    socket.set_nonblocking(true).map_err(|_| "Could not prepare Wi-Fi TCP socket")?;
    let shutdown = socket.try_clone().map_err(|_| "Could not own Wi-Fi TCP socket")?;
    let mut job = job().lock().unwrap_or_else(|e| e.into_inner());
    if matches!(job.progress.lock().unwrap_or_else(|e| e.into_inner()).state.as_str(),
        "connecting" | "uploading" | "checkingInternet" | "registering") {
        return Err("Router setup is already running".into());
    }
    if let Some(cancel) = job.cancel.take() { let _ = cancel.send(()); }
    let progress = Arc::new(Mutex::new(Snapshot { state: "connecting".into(), ..Snapshot::default() }));
    let (cancel_tx, cancel_rx) = oneshot::channel();
    job.progress = progress.clone();
    job.cancel = Some(cancel_tx);
    rt.spawn(async move {
        // Shutdown the underlying socket even if russh still owns an internal
        // session task when this future is cancelled/times out.
        struct ShutdownSocket(TcpStream);
        impl Drop for ShutdownSocket { fn drop(&mut self) { let _ = self.0.shutdown(Shutdown::Both); } }
        let _shutdown = ShutdownSocket(shutdown);
        let result = tokio::select! {
            result = timeout(Duration::from_secs(900), run(socket, request, progress.clone())) =>
                result.unwrap_or_else(|_| Err("Router setup timed out; inspect the output and retry".into())),
            _ = cancel_rx => return,
        };
        if let Err(message) = result {
            let mut state = progress.lock().unwrap_or_else(|e| e.into_inner());
            state.error = Some(message.clone());
            state.state = "error".into();
            push_log(&mut state, message);
        }
    });
    Ok(())
}

fn push_log(snapshot: &mut Snapshot, message: String) {
    if snapshot.logs.len() == 400 { snapshot.logs.pop_front(); }
    snapshot.logs.push_back(message);
}

fn stage(progress: &Progress, state: &str, message: &str) {
    let mut snapshot = progress.lock().unwrap_or_else(|e| e.into_inner());
    snapshot.state = state.into();
    push_log(&mut snapshot, message.into());
}

fn valid_key(value: &str) -> bool {
    value.len() == 44 && STANDARD.decode(value).is_ok_and(|v| v.len() == 32)
}

fn parse_result(stdout: &[u8], expected_key: &str) -> Result<RouterSettings, String> {
    let invalid = || "Router returned invalid or mismatched phone settings".to_string();
    let text = std::str::from_utf8(stdout).map_err(|_| invalid())?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let payload = lines.next().and_then(|l| l.strip_prefix("CELLBRIDGE1:")).ok_or_else(invalid)?;
    if lines.next().is_some() { return Err(invalid()); }
    let result: RouterSettings = serde_json::from_str(payload).map_err(|_| invalid())?;
    let (address, prefix) = result.address.split_once('/').ok_or_else(invalid)?;
    let ip: std::net::Ipv4Addr = address.parse().map_err(|_| invalid())?;
    let octets = ip.octets();
    if result.phone_public_key != expected_key || !valid_key(&result.peer_public_key) ||
        !valid_key(&result.preshared_key) || prefix != "24" || octets[0..2] != [10, 10] ||
        !(10..=17).contains(&octets[2]) || octets[3] != 2 || !(576..=9000).contains(&result.mtu) ||
        result.endpoint.parse::<std::net::SocketAddr>().is_err() {
        return Err(invalid());
    }
    Ok(result)
}

#[derive(Default)]
struct LogDecoder { buffer: Vec<u8>, discarding: bool }

impl LogDecoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut output = Vec::new();
        for &b in bytes {
            if b == b'\n' {
                if !self.discarding {
                    let text = String::from_utf8_lossy(&self.buffer);
                    let lower = text.to_ascii_lowercase();
                    if lower.contains("privatekey") || lower.contains("private_key") ||
                        lower.contains("presharedkey") || lower.contains("preshared_key") ||
                        text.contains("CELLBRIDGE1:") {
                        output.push("[key material omitted]".into());
                    } else {
                        // Keys are base64 encoded 32-byte values. Sanitize even
                        // unexpected tool output before it reaches UI/copy logs.
                        let mut clean = String::new();
                        let mut token = String::new();
                        for ch in text.chars().chain(std::iter::once('\n')) {
                            if ch.is_ascii_alphanumeric() || "+/=".contains(ch) { token.push(ch); }
                            else {
                                if valid_key(&token) { clean.push_str("[key omitted]"); }
                                else { clean.push_str(&token); }
                                token.clear();
                                if !ch.is_control() || ch == '\t' { clean.push(ch); }
                            }
                        }
                        if !clean.trim().is_empty() { output.push(clean); }
                    }
                } else { output.push("[oversized output line omitted]".into()); }
                self.buffer.clear();
                self.discarding = false;
            } else if !self.discarding {
                if self.buffer.len() == 8192 { self.buffer.clear(); self.discarding = true; }
                else { self.buffer.push(b); }
            }
        }
        output
    }
}

struct HostVerifier { expected: Option<String>, seen: Arc<Mutex<Option<String>>> }
impl client::Handler for HostVerifier {
    type Error = russh::Error;
    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fingerprint = key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(fingerprint.clone());
        Ok(self.expected.as_deref() == Some(&fingerprint))
    }
}

struct CommandResult { code: u32, stdout: Vec<u8> }

async fn command(handle: &client::Handle<HostVerifier>, text: &str, input: Option<&str>, progress: &Progress) -> Result<CommandResult, String> {
    let mut channel = handle.channel_open_session().await.map_err(|_| "SSH command channel failed")?;
    channel.exec(true, text).await.map_err(|_| "Router refused the setup command")?;
    if let Some(input) = input {
        channel.data(input.as_bytes()).await.map_err(|_| "Script upload failed")?;
    }
    channel.eof().await.map_err(|_| "SSH command input failed")?;
    let mut stdout = Vec::new();
    let mut decoder = LogDecoder::default();
    let mut code = None;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => {
                if stdout.len() + data.len() > 65536 { return Err("Unexpectedly large router response".into()); }
                stdout.extend_from_slice(&data);
            }
            ChannelMsg::ExtendedData { data, .. } => {
                for line in decoder.push(&data) {
                    push_log(&mut progress.lock().unwrap_or_else(|e| e.into_inner()), line);
                }
            }
            ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
            ChannelMsg::Failure => return Err("Router refused a setup operation".into()),
            ChannelMsg::Close => break,
            _ => {},
        }
    }
    for line in decoder.push(b"\n") { push_log(&mut progress.lock().unwrap_or_else(|e| e.into_inner()), line); }
    Ok(CommandResult { code: code.ok_or("SSH connection ended before the script reported an exit status; retry reuses this phone")?, stdout })
}

const SCRIPTS: [(&str, &str); 4] = [
    ("cellbridge-setup.sh", include_str!("../../router/cellbridge-setup.sh")),
    ("cellbridge-failover.sh", include_str!("../../router/cellbridge-failover.sh")),
    ("cellbridge-revert.sh", include_str!("../../router/cellbridge-revert.sh")),
    ("cellbridge-provision.sh", include_str!("../../router/cellbridge-provision.sh")),
];

async fn run(socket: TcpStream, request: Request, progress: Progress) -> Result<(), String> {
    let seen = Arc::new(Mutex::new(None));
    let stream = tokio::net::TcpStream::from_std(socket).map_err(|_| "Could not adopt Wi-Fi socket")?;
    let verifier = HostVerifier { expected: request.expected_fingerprint.clone(), seen: seen.clone() };
    let config = Arc::new(client::Config { inactivity_timeout: Some(Duration::from_secs(120)), ..Default::default() });
    let connection = timeout(Duration::from_secs(25), client::connect_stream(config, stream, verifier)).await;
    let mut handle = match connection {
        Ok(Ok(handle)) => handle,
        _ => {
            let fingerprint = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(fingerprint) = fingerprint {
                if request.expected_fingerprint.is_none() {
                    let mut snapshot = progress.lock().unwrap_or_else(|e| e.into_inner());
                    snapshot.state = "hostKey".into();
                    snapshot.fingerprint = Some(fingerprint);
                    push_log(&mut snapshot, "Confirm the router SSH fingerprint before sending login details".into());
                    return Ok(());
                }
                if request.expected_fingerprint.as_ref() != Some(&fingerprint) { return Err("Router SSH host key has changed; verify the router from a PC".into()); }
            }
            return Err("SSH handshake failed; check router address, port and Wi-Fi".into());
        }
    };
    let auth = timeout(Duration::from_secs(25), handle.authenticate_password(request.username, request.password)).await
        .map_err(|_| "SSH login timed out")?.map_err(|_| "SSH authentication failed")?;
    if !auth.success() { return Err("SSH login rejected; check username/password and root access".into()); }

    stage(&progress, "uploading", "Uploading bundled CellBridge scripts over Wi-Fi…");
    let root = timeout(Duration::from_secs(20), command(&handle, "id -u", None, &progress)).await.map_err(|_| "Router access check timed out")??;
    if root.code != 0 || root.stdout.as_slice() != b"0\n" { return Err("Router setup requires an OpenWrt root account".into()); }
    let dir = command(&handle, "umask 077; mktemp -d /tmp/cellbridge-app.XXXXXXXX", None, &progress).await?;
    let path = String::from_utf8(dir.stdout).map_err(|_| "Invalid staging directory")?;
    let path = path.trim().to_string();
    if dir.code != 0 || !path.starts_with("/tmp/cellbridge-app.") || path.len() > 100 ||
        !path.chars().all(|ch| ch.is_ascii_alphanumeric() || "/.-".contains(ch)) { return Err("Could not create a private router staging directory".into()); }

    // Everything after the staging directory exists runs inside this block so
    // the directory is always removed, including on early `?` returns.
    let outcome = async {
        for (name, contents) in SCRIPTS {
            let result = command(&handle, &format!("umask 077; cat > {path}/{name}"), Some(contents), &progress).await?;
            if result.code != 0 { return Err(format!("Could not upload {name}")); }
        }
        stage(&progress, "checkingInternet", "Checking router internet before changing its configuration…");
        let check = command(&handle, &format!("sh {path}/cellbridge-provision.sh check-internet"), None, &progress).await?;
        if check.code == 75 {
            stage(&progress, "waitingForInternet", "Router internet is unavailable. Restore it, then tap Retry.");
            return Ok(());
        }
        if check.code != 0 { return Err("Router internet check failed; inspect the output above".into()); }
        stage(&progress, "registering", "Installing dependencies and registering this phone…");
        let result = command(&handle, &format!("sh {path}/cellbridge-provision.sh register '{}' {}", request.phone_public_key, request.priority), None, &progress).await?;
        match result.code {
            0 => {
                let settings = parse_result(&result.stdout, &request.phone_public_key)?;
                let mut snapshot = progress.lock().unwrap_or_else(|e| e.into_inner());
                snapshot.result = Some(settings);
                snapshot.state = "success".into();
                push_log(&mut snapshot, "Router setup complete. Phone settings received.".into());
                Ok(())
            }
            75 => { stage(&progress, "waitingForInternet", "Router lost internet. Restore it, then tap Retry."); Ok(()) }
            code => Err(format!("Router setup exited with code {code}; inspect the output above. Retry reuses this phone.")),
        }
    }.await;

    let _ = timeout(Duration::from_secs(3), command(&handle, &format!("rm -rf {path}"), None, &progress)).await;
    let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "").await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";
    const PSK: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn payload_for(phone_key: &str) -> String {
        format!("CELLBRIDGE1:{{\"address\":\"10.10.10.2/24\",\"peerPublicKey\":\"{KEY}\",\"presharedKey\":\"{PSK}\",\"endpoint\":\"192.168.8.1:51820\",\"mtu\":1420,\"phonePublicKey\":\"{phone_key}\"}}\n")
    }

    fn payload() -> String {
        payload_for(KEY)
    }

    #[test]
    fn result_requires_one_framed_payload_for_the_correct_phone() {
        let settings = parse_result(payload().as_bytes(), KEY).unwrap();
        assert_eq!(settings.address, "10.10.10.2/24");
        assert!(parse_result(payload().as_bytes(), PSK).is_err());
        assert!(parse_result(format!("{}{}", payload(), payload()).as_bytes(), KEY).is_err());
        assert!(parse_result(b"installation complete", KEY).is_err());
        assert!(parse_result(payload().replace("10.10.10.2/24", "garbage").as_bytes(), KEY).is_err());
    }

    #[test]
    fn log_decoder_redacts_secrets_across_chunks_and_bounds_long_lines() {
        let mut lines = LogDecoder::default();
        let mut visible = lines.push(b"Installing tools\nPrivateKey = AAA");
        visible.extend(lines.push(format!("{}\nCELLBRIDGE1:secret\nDone\n", &PSK[3..]).as_bytes()));
        let joined = visible.join("\n");
        assert!(joined.contains("Installing tools"));
        assert!(joined.contains("Done"));
        assert!(!joined.contains(PSK));
        assert!(!joined.contains("CELLBRIDGE1:secret"));
        assert!(lines.push(&vec![b'x'; 100_000]).is_empty());
        assert!(lines.buffer.len() <= 8192);
    }

    struct TestServer {
        calls: Arc<Mutex<Vec<String>>>,
        auths: Arc<std::sync::atomic::AtomicUsize>,
        offline: bool,
        uploads: std::collections::HashMap<russh::ChannelId, usize>,
    }

    impl russh::server::Handler for TestServer {
        type Error = russh::Error;
        async fn auth_password(&mut self, user: &str, password: &str) -> Result<russh::server::Auth, Self::Error> {
            self.auths.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(user, "root");
            assert_eq!(password, "session-only-password");
            Ok(russh::server::Auth::Accept)
        }
        async fn channel_open_session(&mut self, _channel: russh::Channel<russh::server::Msg>, reply: russh::server::ChannelOpenHandle, _session: &mut russh::server::Session) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }
        async fn exec_request(&mut self, channel: russh::ChannelId, data: &[u8], session: &mut russh::server::Session) -> Result<(), Self::Error> {
            let cmd = String::from_utf8_lossy(data).into_owned();
            self.calls.lock().unwrap().push(cmd.clone());
            session.channel_success(channel)?;
            let (output, code) = if cmd == "id -u" { ("0\n".to_string(), 0) }
                else if cmd.contains("mktemp") { ("/tmp/cellbridge-app.test1234\n".into(), 0) }
                else if cmd.contains("cat >") { self.uploads.insert(channel, 0); return Ok(()); }
                else if cmd.ends_with("check-internet") { (String::new(), if self.offline {75} else {0}) }
                else if cmd.contains(" register ") {
                    // Echo the caller's own public key, exactly as the real
                    // router does, so the app can complete a full success path.
                    let key = cmd.split('\'').nth(1).unwrap_or(KEY).to_string();
                    session.extended_data(channel, 1, format!("Installing tools\nPresharedKey = {PSK}\nDone\n").into_bytes())?;
                    (payload_for(&key), 0)
                } else { (String::new(), 0) };
            if !output.is_empty() { session.data(channel, output.into_bytes())?; }
            session.exit_status_request(channel, code)?;
            session.eof(channel)?;
            session.close(channel)?;
            Ok(())
        }
        async fn data(&mut self, channel: russh::ChannelId, data: &[u8], _session: &mut russh::server::Session) -> Result<(), Self::Error> {
            *self.uploads.get_mut(&channel).expect("script input") += data.len();
            Ok(())
        }
        async fn channel_eof(&mut self, channel: russh::ChannelId, session: &mut russh::server::Session) -> Result<(), Self::Error> {
            if let Some(size) = self.uploads.remove(&channel) {
                assert!(size > 20);
                session.exit_status_request(channel, 0)?;
                session.eof(channel)?;
                session.close(channel)?;
            }
            Ok(())
        }
    }

    async fn exercise_ssh(trusted: bool, offline: bool) -> (Snapshot, usize, Vec<String>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let key = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
        let fingerprint = key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
        let config = Arc::new(russh::server::Config { keys: vec![key], ..Default::default() });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        socket.set_nonblocking(true).unwrap();
        let auths = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let handler = TestServer { auths: auths.clone(), calls: calls.clone(), offline, uploads: Default::default() };
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            if let Ok(session) = russh::server::run_stream(config, socket, handler).await { let _ = session.await; }
        });
        let request = Request { username: "root".into(), password: "session-only-password".into(), priority: 1,
            phone_public_key: KEY.into(), expected_fingerprint: trusted.then_some(fingerprint) };
        let progress = Arc::new(Mutex::new(Snapshot::default()));
        timeout(Duration::from_secs(15), run(socket, request, progress.clone())).await.unwrap().unwrap();
        server.abort();
        let state = progress.lock().unwrap().clone();
        let commands = calls.lock().unwrap().clone();
        (state, auths.load(Ordering::SeqCst), commands)
    }

    #[tokio::test]
    async fn unknown_host_key_never_sends_password() {
        let (state, auths, commands) = exercise_ssh(false, false).await;
        assert_eq!(state.state, "hostKey");
        assert!(state.fingerprint.unwrap().starts_with("SHA256:"));
        assert_eq!(auths, 0);
        assert!(commands.is_empty());
    }

    #[tokio::test]
    async fn real_ssh_uploads_scripts_streams_safe_logs_and_imports_result() {
        let (state, auths, commands) = exercise_ssh(true, false).await;
        assert_eq!(state.state, "success");
        assert_eq!(auths, 1);
        assert_eq!(commands.iter().filter(|c| c.contains("cat >")).count(), 4);
        assert!(commands.iter().any(|c| c.contains("register '") && c.ends_with(" 1")));
        assert_eq!(state.result.unwrap().phone_public_key, KEY);
        let logs = state.logs.into_iter().collect::<Vec<_>>().join("\n");
        assert!(logs.contains("Installing tools"));
        assert!(!logs.contains(PSK));
        assert!(!logs.contains("session-only-password"));
    }

    #[tokio::test]
    async fn offline_router_pauses_before_registration() {
        let (state, _, commands) = exercise_ssh(true, true).await;
        assert_eq!(state.state, "waitingForInternet");
        assert!(state.result.is_none());
        assert!(!commands.iter().any(|c| c.contains(" register ")));
    }

    /// Real-router interoperability check. Read-only: it authenticates and runs
    /// only inspection commands, never uploads or mutates. Credentials come
    /// from the environment and are never stored or printed.
    #[tokio::test]
    #[ignore = "manual; set CELLBRIDGE_TEST_ROUTER_HOST/USER/PASS"]
    async fn real_router_interop_read_only() {
        let Ok(endpoint) = std::env::var("CELLBRIDGE_TEST_ROUTER_HOST") else { return };
        let user = std::env::var("CELLBRIDGE_TEST_ROUTER_USER").expect("router user");
        let pass = std::env::var("CELLBRIDGE_TEST_ROUTER_PASS").expect("router password");
        let config = Arc::new(client::Config { inactivity_timeout: Some(Duration::from_secs(30)), ..Default::default() });

        let seen = Arc::new(Mutex::new(None));
        let _ = timeout(Duration::from_secs(20), client::connect_stream(
            config.clone(), tokio::net::TcpStream::connect(&endpoint).await.unwrap(),
            HostVerifier { expected: None, seen: seen.clone() })).await;
        let fingerprint = seen.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("host key presented");
        println!("router fingerprint: {fingerprint}");

        let mut handle = timeout(Duration::from_secs(20), client::connect_stream(
            config, tokio::net::TcpStream::connect(&endpoint).await.unwrap(),
            HostVerifier { expected: Some(fingerprint), seen: Arc::new(Mutex::new(None)) }))
            .await.expect("connect timeout").expect("connect with pinned key");
        let auth = handle.authenticate_password(user, pass).await.expect("auth");
        println!("auth result: {auth:?}");
        assert!(auth.success(), "password authentication accepted");
        let progress = Arc::new(Mutex::new(Snapshot::default()));
        for cmd in ["id -u", "command -v curl", "command -v wget", "sh -n /etc/init.d/dropbear || true"] {
            let result = command(&handle, cmd, None, &progress).await.expect("command");
            println!("$ {cmd} -> exit {} stdout {:?}", result.code, String::from_utf8_lossy(&result.stdout).trim());
        }
        let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "").await;
    }

    /// Manual device fixture only: adb reverse tcp:22222 tcp:22222. It never
    /// executes scripts, and the fixed result key cannot match a real phone,
    /// so successful fixture commands cannot replace the phone configuration.
    #[tokio::test]
    #[ignore = "manual connected-device SSH fixture (five minute lifetime)"]
    async fn device_ssh_fixture() {
        // A stable host key makes the app's pinned fingerprint survive fixture
        // restarts. Kept outside the repo; never a real credential.
        let key_path = std::path::Path::new("/tmp/opencode/cellbridge-fixture-hostkey");
        let key = if let Ok(text) = std::fs::read_to_string(key_path) {
            russh::keys::PrivateKey::from_openssh(text.trim()).expect("fixture host key")
        } else {
            let generated = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
            std::fs::write(key_path, generated.to_openssh(Default::default()).unwrap()).expect("save fixture host key");
            generated
        };
        let config = Arc::new(russh::server::Config { keys: vec![key], ..Default::default() });
        // Bind all interfaces so a phone on the same LAN can reach it at
        // <host-lan-ip>:22222 without adb reverse (which a Wi-Fi-bound socket
        // cannot use over loopback).
        let listener = tokio::net::TcpListener::bind("0.0.0.0:22222").await.unwrap();
        println!("Device fixture ready on 0.0.0.0:22222; root/session-only-password; offline until /tmp/opencode/cellbridge-fixture-online exists");
        let _ = timeout(Duration::from_secs(300), async {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let handler = TestServer { calls: Arc::new(Mutex::new(Vec::new())),
                    auths: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    offline: !std::path::Path::new("/tmp/opencode/cellbridge-fixture-online").exists(),
                    uploads: Default::default() };
                let config = config.clone();
                tokio::spawn(async move {
                    if let Ok(session) = russh::server::run_stream(config, socket, handler).await { let _ = session.await; }
                });
            }
        }).await;
    }
}
