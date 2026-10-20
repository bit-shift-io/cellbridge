//! WireGuard tunnel wrapper around [`boringtun::noise::Tunn`].
//!
//! [`Tunnel`] owns a boringtun [`Tunn`] and adapts its in-place, byte-slice API
//! into one that returns owned packet buffers, so the rest of the core (and the
//! `flutter_rust_bridge` boundary) never has to juggle scratch buffers.

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use boringtun::noise::errors::WireGuardError;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};

use crate::config::BridgeConfig;

/// Largest UDP datagram we expect to receive or produce.
const MAX_DATAGRAM: usize = 65_535;
/// boringtun requires a destination buffer of at least 148 bytes regardless of
/// the source packet size.
const MIN_PACKET_BUFFER: usize = 148;

/// Errors produced by the [`Tunnel`] wrapper.
#[derive(Debug)]
pub enum TunnelError {
    /// A key in the [`BridgeConfig`] was not valid base64 or not 32 bytes long.
    InvalidKey {
        /// Which field was malformed.
        field: &'static str,
        /// Human-readable reason.
        reason: String,
    },
    /// The underlying boringtun tunnel returned an error.
    WireGuard(WireGuardError),
}

impl fmt::Display for TunnelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TunnelError::InvalidKey { field, reason } => {
                write!(f, "invalid WireGuard key for `{field}`: {reason}")
            }
            TunnelError::WireGuard(err) => write!(f, "WireGuard error: {err:?}"),
        }
    }
}

impl std::error::Error for TunnelError {}

impl From<WireGuardError> for TunnelError {
    fn from(err: WireGuardError) -> Self {
        TunnelError::WireGuard(err)
    }
}

/// Process-wide source of unique handshake indices.
///
/// WireGuard identifies sessions by a locally chosen 32-bit index; two tunnels
/// created in the same process must not collide.
fn next_index() -> u32 {
    static NEXT_INDEX: AtomicU32 = AtomicU32::new(1);
    NEXT_INDEX.fetch_add(1, Ordering::Relaxed)
}

/// Decode a base64 key, accepting both padded and unpadded standard base64.
fn parse_key(value: &str, field: &'static str) -> Result<[u8; 32], TunnelError> {
    let trimmed = value.trim();
    let decoded = STANDARD
        .decode(trimmed)
        .or_else(|_| STANDARD_NO_PAD.decode(trimmed))
        .map_err(|err| TunnelError::InvalidKey {
            field,
            reason: err.to_string(),
        })?;
    decoded
        .try_into()
        .map_err(|bytes: Vec<u8>| TunnelError::InvalidKey {
            field,
            reason: format!("expected 32 bytes, got {}", bytes.len()),
        })
}

/// What the tunnel wants done after an operation.
///
/// boringtun can produce datagrams to send to the peer and decrypted inner IP
/// packets from a single call; keeping the two streams separate means the
/// caller never has to guess which is which.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TunnelOutput {
    /// Encrypted WireGuard datagrams to send to the peer (handshake, keepalive,
    /// data).
    pub to_network: Vec<Vec<u8>>,
    /// Decrypted inner IP packets to hand to the gateway.
    pub to_tunnel: Vec<Vec<u8>>,
    /// How long until `tick` should be called again, if the tunnel needs it.
    ///
    /// Always `None` for now: boringtun 0.7.1 exposes no "next timer deadline"
    /// signal (its timer constants are private and `update_timers` only returns
    /// a `TunnResult`), so the caller must drive `tick` on its own schedule.
    pub retry_after: Option<Duration>,
}

/// Fold a boringtun result into `out`.
///
/// Returns `true` when the caller should keep flushing with an empty datagram
/// (a `WriteToNetwork` was produced), `false` when the operation is finished.
fn absorb(out: &mut TunnelOutput, result: TunnResult<'_>) -> Result<bool, TunnelError> {
    match result {
        TunnResult::WriteToNetwork(pkt) => {
            out.to_network.push(pkt.to_vec());
            Ok(true)
        }
        TunnResult::WriteToTunnelV4(pkt, _) | TunnResult::WriteToTunnelV6(pkt, _) => {
            out.to_tunnel.push(pkt.to_vec());
            Ok(false)
        }
        TunnResult::Done => Ok(false),
        TunnResult::Err(err) => Err(err.into()),
    }
}

/// A point-to-point WireGuard tunnel.
pub struct Tunnel {
    tunn: Tunn,
}

impl Tunnel {
    /// Build a tunnel from configuration.
    pub fn new(cfg: &BridgeConfig) -> Result<Tunnel, TunnelError> {
        let private = parse_key(&cfg.private_key, "private_key")?;
        let peer_public = parse_key(&cfg.peer_public_key, "peer_public_key")?;
        let preshared = match cfg.preshared_key.as_deref() {
            Some(key) if !key.trim().is_empty() => Some(parse_key(key, "preshared_key")?),
            _ => None,
        };

        let tunn = Tunn::new(
            StaticSecret::from(private),
            PublicKey::from(peer_public),
            preshared,
            None,
            next_index(),
            None,
        );

        Ok(Tunnel { tunn })
    }

    /// Whether a handshake has completed and a session is live.
    pub fn is_connected(&self) -> bool {
        self.tunn.time_since_last_handshake().is_some()
    }

    /// Encrypt a plaintext IP packet.
    ///
    /// Returns the WireGuard datagram(s) to put on the wire in
    /// [`TunnelOutput::to_network`]. When no session is established yet,
    /// boringtun queues the packet and emits a handshake initiation instead;
    /// the queued packet is delivered once the session comes up.
    pub fn encapsulate(&mut self, ip_packet: &[u8]) -> Result<TunnelOutput, TunnelError> {
        let mut buf = vec![0u8; ip_packet.len().max(MIN_PACKET_BUFFER) + 32];
        let mut output = TunnelOutput::default();
        absorb(&mut output, self.tunn.encapsulate(ip_packet, &mut buf))?;
        Ok(output)
    }

    /// Process an incoming UDP datagram from the peer.
    ///
    /// WireGuard datagrams that must be sent back (handshake responses,
    /// keepalives) land in [`TunnelOutput::to_network`]; decrypted inner IP
    /// packets land in [`TunnelOutput::to_tunnel`]. Nothing is dropped.
    pub fn handle_udp(&mut self, datagram: &[u8]) -> Result<TunnelOutput, TunnelError> {
        let mut output = TunnelOutput::default();
        let mut current = datagram;
        loop {
            let mut buf = vec![0u8; MAX_DATAGRAM];
            // Per boringtun: after a WriteToNetwork, keep calling with an empty
            // datagram to flush any queued packets until Done.
            if !absorb(&mut output, self.tunn.decapsulate(None, current, &mut buf))? {
                break;
            }
            current = &[];
        }
        Ok(output)
    }

    /// Advance the tunnel timers (handshake retries, keepalives, session expiry).
    ///
    /// Any resulting datagrams land in [`TunnelOutput::to_network`].
    pub fn tick(&mut self) -> Result<TunnelOutput, TunnelError> {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let mut output = TunnelOutput::default();
        absorb(&mut output, self.tunn.update_timers(&mut buf))?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(private: [u8; 32], peer_public: [u8; 32]) -> BridgeConfig {
        BridgeConfig {
            private_key: STANDARD.encode(private),
            address: "10.7.0.2/24".to_string(),
            peer_public_key: STANDARD.encode(peer_public),
            preshared_key: None,
            endpoint: "203.0.113.1:51820".to_string(),
            mtu: 1420,
        }
    }

    fn public_of(private: [u8; 32]) -> [u8; 32] {
        PublicKey::from(&StaticSecret::from(private)).to_bytes()
    }

    /// A minimal (header-only) IPv4 packet: version 4, 20 byte header, UDP.
    fn ipv4_packet() -> Vec<u8> {
        let mut ip = vec![0u8; 20];
        ip[0] = 0x45; // version 4, IHL 5
        ip[2..4].copy_from_slice(&20u16.to_be_bytes()); // total length
        ip[8] = 64; // TTL
        ip[9] = 17; // protocol: UDP
        ip[12..16].copy_from_slice(&[10, 0, 0, 2]); // source
        ip[16..20].copy_from_slice(&[10, 0, 0, 1]); // destination
        ip
    }

    #[test]
    fn handshake_and_roundtrip() {
        let phone_secret = [0x11; 32];
        let router_secret = [0x22; 32];

        let mut phone = Tunnel::new(&config(phone_secret, public_of(router_secret))).unwrap();
        let mut router = Tunnel::new(&config(router_secret, public_of(phone_secret))).unwrap();

        let ip = ipv4_packet();

        // Drive the handshake: the phone initiates by encapsulating a packet
        // (boringtun queues it and emits a handshake initiation). Datagrams on
        // `to_network` are bounced between the two sides until both report a
        // live session.
        let mut to_router = phone.encapsulate(&ip).unwrap().to_network;
        for _ in 0..16 {
            if phone.is_connected() && router.is_connected() {
                break;
            }
            to_router.extend(phone.tick().unwrap().to_network);

            let mut to_phone = Vec::new();
            for dg in to_router.drain(..) {
                to_phone.extend(router.handle_udp(&dg).unwrap().to_network);
            }
            to_phone.extend(router.tick().unwrap().to_network);

            for dg in to_phone {
                to_router.extend(phone.handle_udp(&dg).unwrap().to_network);
            }
        }

        assert!(
            phone.is_connected() && router.is_connected(),
            "handshake did not complete"
        );

        // A data packet encapsulated on the phone: the encrypted datagram is in
        // `to_network` and nothing is destined for the local stack.
        let outbound = phone.encapsulate(&ip).unwrap();
        assert_eq!(outbound.to_network.len(), 1);
        assert!(outbound.to_tunnel.is_empty());

        // The peer decrypts it, surfacing the original plaintext in `to_tunnel`
        // and nothing to send back.
        let inbound = router.handle_udp(&outbound.to_network[0]).unwrap();
        assert_eq!(inbound.to_tunnel, vec![ip]);
        assert!(inbound.to_network.is_empty());

        // `tick` is serviced on the caller's schedule: no deadline hint today.
        assert!(phone.tick().unwrap().retry_after.is_none());
    }

    #[test]
    fn invalid_key_is_an_error_not_a_panic() {
        let mut cfg = config([0x11; 32], public_of([0x22; 32]));

        cfg.private_key = "not-valid-base64!!".to_string();
        assert!(matches!(
            Tunnel::new(&cfg),
            Err(TunnelError::InvalidKey {
                field: "private_key",
                ..
            })
        ));

        cfg.private_key = STANDARD.encode([0x11; 32]);
        cfg.peer_public_key = STANDARD.encode([0u8; 31]); // wrong length
        assert!(matches!(
            Tunnel::new(&cfg),
            Err(TunnelError::InvalidKey {
                field: "peer_public_key",
                ..
            })
        ));
    }
}
