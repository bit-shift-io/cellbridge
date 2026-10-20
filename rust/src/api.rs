//! The `flutter_rust_bridge` surface.
//!
//! Everything here is a thin delegate: the bridge loop lives in
//! [`crate::bridge`] and the data path in the core modules. Keeping this file to
//! signatures keeps the generated Dart bindings small and readable.

use flutter_rust_bridge::frb;

use crate::bridge;
use crate::config::BridgeConfig;
use crate::stats::{BridgeStatus, Stats};

pub use crate::keys::BridgeKeypair;

/// Round-trip smoke test: the Rust core replies `"pong"`.
pub fn ping() -> String {
    "pong".to_string()
}

/// Start the Wi-Fi bridge.
///
/// `wg_socket_fd` is the WireGuard UDP socket prepared by Android and bound to
/// the Wi-Fi network. **Ownership transfers to Rust**: the caller must detach
/// the descriptor (`ParcelFileDescriptor.detachFd()`) and must not close it.
/// Pass `None` to let Rust bind an ordinary UDP socket, which is how the bridge
/// runs on a Linux desktop for development.
///
/// Returns an error if the bridge is already running, the configuration is
/// invalid, or the socket cannot be bound/adopted.
#[frb]
pub async fn start_bridge(cfg: BridgeConfig, wg_socket_fd: Option<i32>) -> Result<(), String> {
    bridge::start(cfg, wg_socket_fd).await
}

/// Stop the bridge and return it to [`crate::BridgeState::Idle`].
///
/// Safe to call when the bridge is not running.
pub async fn stop_bridge() -> Result<(), String> {
    bridge::stop().await
}

/// The current lifecycle state and last error, if any.
#[frb(sync)]
pub fn status() -> BridgeStatus {
    bridge::status()
}

/// The current byte and flow counters.
#[frb(sync)]
pub fn stats() -> Stats {
    bridge::stats()
}

/// Generate a WireGuard key pair using the operating system's CSPRNG.
///
/// Both keys are standard WireGuard base64 (32 bytes, 44 characters). The
/// private key is clamped, matching `wg genkey`.
pub fn generate_keypair() -> BridgeKeypair {
    crate::keys::generate()
}

/// Derive the base64 public key for a base64 private key.
///
/// Input is clamped exactly as X25519 specifies, so a pasted `wg genkey`
/// private key yields the same public key `wg pubkey` reports. Malformed
/// base64 or a key that is not 32 bytes returns an error; the message never
/// includes the key material.
pub fn derive_public_key(private_key: String) -> Result<String, String> {
    crate::keys::derive_public(&private_key)
}
