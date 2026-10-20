//! WireGuard key material: generation and public-key derivation.
//!
//! Keys cross the `flutter_rust_bridge` boundary as standard WireGuard base64:
//! 32 bytes encoded with the usual alphabet and `=` padding (44 characters).
//! Derivation uses [`boringtun::x25519`], which applies X25519's standard
//! scalar clamping (`k[0] &= 248; k[31] &= 127; k[31] |= 64`) internally.
//!
//! Error messages never include the supplied key: a malformed private key may
//! still be secret material.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use boringtun::x25519::{PublicKey, StaticSecret};

/// Length of a Curve25519 key in bytes.
pub const KEY_LEN: usize = 32;

/// A freshly generated WireGuard private/public key pair, both base64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeKeypair {
    /// Base64-encoded private key, already clamped like `wg genkey` output.
    pub private_key: String,
    /// Base64-encoded public key that belongs to [`Self::private_key`].
    pub public_key: String,
}

/// Generates a key pair from the operating system's CSPRNG.
///
/// The private key is clamped before encoding, matching `wg genkey`, so the
/// displayed key is canonical WireGuard material rather than raw randomness.
pub fn generate() -> BridgeKeypair {
    let mut bytes = [0u8; KEY_LEN];
    getrandom::getrandom(&mut bytes).expect("operating system CSPRNG is unavailable");
    let private = clamp(bytes);
    let public = public_bytes(&private);

    BridgeKeypair {
        private_key: STANDARD.encode(private),
        public_key: STANDARD.encode(public),
    }
}

/// Derives the base64 public key for a base64 private key.
///
/// The private key has the same shape as `wg genkey` output but may be any
/// 32-byte value; clamping is applied during derivation, exactly as X25519
/// specifies. Malformed input yields `Err` and never panics.
pub fn derive_public(private_key: &str) -> Result<String, String> {
    let bytes = decode_private(private_key)?;
    Ok(STANDARD.encode(public_bytes(&bytes)))
}

/// Clamps a Curve25519 scalar the way X25519 (and `wg genkey`) does.
fn clamp(mut key: [u8; KEY_LEN]) -> [u8; KEY_LEN] {
    key[0] &= 248;
    key[KEY_LEN - 1] &= 127;
    key[KEY_LEN - 1] |= 64;
    key
}

/// Computes the public key. `PublicKey::from(&StaticSecret)` clamps internally;
/// [`clamp`] above just keeps generated private keys canonical.
fn public_bytes(private: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    let secret = StaticSecret::from(*private);
    PublicKey::from(&secret).to_bytes()
}

/// Decodes a base64 private key without ever echoing it in an error.
fn decode_private(private_key: &str) -> Result<[u8; KEY_LEN], String> {
    let bytes = STANDARD
        .decode(private_key.as_bytes())
        .map_err(|_| "Private key must be a base64-encoded 32-byte key.".to_string())?;
    bytes
        .try_into()
        .map_err(|_| "Private key must be a base64-encoded 32-byte key.".to_string())
}
