//! Bridge configuration.
//!
//! `BridgeConfig` is a plain owned struct with public fields. It is destined to
//! cross the `flutter_rust_bridge` boundary by value as an argument to
//! `start_bridge`, so it holds no lifetimes and borrows nothing.

/// Everything needed to bring up a WireGuard exit bridge.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeConfig {
    /// Base64-encoded WireGuard private key of this device.
    pub private_key: String,
    /// Interface address in CIDR form, e.g. `"10.7.0.2/24"`.
    pub address: String,
    /// Base64-encoded WireGuard public key of the LAN exit peer.
    pub peer_public_key: String,
    /// Optional base64-encoded preshared key. `None` or an empty string means
    /// no preshared key is used.
    pub preshared_key: Option<String>,
    /// Peer endpoint as `"host:port"`, e.g. `"203.0.113.1:51820"`.
    pub endpoint: String,
    /// MTU for the tunnel interface.
    pub mtu: u16,
}
