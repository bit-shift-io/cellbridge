//! Headless entry points. The runtime outlives Activities and JNI calls.
use crate::{BridgeConfig, bridge};
use std::sync::OnceLock;
use tokio::runtime::Runtime;

fn runtime() -> Result<&'static Runtime, String> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("cellbridge")
                .build()
                .map_err(|_| "cannot create bridge runtime".into())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Call from a platform worker thread, never from inside a tokio runtime.
pub fn start(config: BridgeConfig, fd: Option<i32>) -> Result<(), String> {
    // Initialize before transferring the socket to the async lifecycle. If
    // runtime creation fails, still release the transferred descriptor.
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(error) => {
            #[cfg(unix)]
            if let Some(fd) = fd.filter(|fd| *fd >= 0) {
                use std::os::fd::FromRawFd;
                drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
            }
            return Err(error);
        }
    };
    rt.block_on(bridge::start(config, fd)).map_err(|error| {
        let message = if error.contains("`private_key`") {
            "Invalid phone private key"
        } else if error.contains("`peer_public_key`") {
            "Invalid peer public key"
        } else if error.contains("`preshared_key`") {
            "Invalid preshared key"
        } else if error.contains("invalid endpoint") {
            "Invalid peer endpoint"
        } else if error.contains("invalid address") || error.contains("invalid prefix") {
            "Invalid tunnel address"
        } else if error.contains("already running") {
            "Bridge is already running"
        } else {
            "Native bridge startup failed"
        };
        crate::diagnostics::record("native", message);
        message.to_string()
    })
}

pub fn stop() -> Result<(), String> {
    runtime()?.block_on(bridge::stop())
}

/// Owns the descriptor from entry, including malformed JSON failures.
#[cfg(unix)]
pub fn start_json(json: &str, fd: i32) -> Result<(), String> {
    use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
    if fd < 0 {
        return Err("invalid Wi-Fi socket descriptor".into());
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let config: BridgeConfig = serde_json::from_str(json).map_err(|_| {
        crate::diagnostics::record("native", "Invalid bridge configuration");
        "Invalid bridge configuration".to_string()
    })?;
    start(config, Some(owned.into_raw_fd()))
}

pub fn snapshot_json() -> String {
    let status = bridge::status();
    let stats = bridge::stats();
    serde_json::json!({
        "state": format!("{:?}", status.state).to_lowercase(),
        "error": status.error,
        "txBytes": stats.tx_bytes.to_string(), "rxBytes": stats.rx_bytes.to_string(),
        "activeFlows": stats.active_flows.to_string(), "diagnostics": crate::diagnostics::snapshot()
    })
    .to_string()
}
