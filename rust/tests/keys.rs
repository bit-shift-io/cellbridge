//! WireGuard key generation and public-key derivation.
//!
//! The vector is RFC 7748 §6.1 (Alice's key pair), *not* a key from this
//! project. X25519 clamps the scalar internally, so the raw scalar bytes are
//! fed to `derive_public_key` unchanged and must still produce the published
//! public key.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use cellbridge::{BridgeKeypair, derive_public_key, generate_keypair};

/// RFC 7748 §6.1 private key, base64 of the raw 32-byte scalar.
const RFC7748_PRIVATE: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=";
/// RFC 7748 §6.1 public key, base64 of the 32-byte u-coordinate.
const RFC7748_PUBLIC: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";

fn decode(key: &str) -> Vec<u8> {
    STANDARD.decode(key).expect("test vector is valid base64")
}

#[test]
fn derives_the_rfc7748_public_key() {
    let public = derive_public_key(RFC7748_PRIVATE.to_string()).expect("RFC vector derives");

    assert_eq!(public, RFC7748_PUBLIC);
    assert_eq!(decode(&public), decode(RFC7748_PUBLIC));
    assert_eq!(decode(&public).len(), 32);
}

#[test]
fn generated_keys_are_wireguard_shaped_and_consistent() {
    let BridgeKeypair {
        private_key,
        public_key,
    } = generate_keypair();

    // Base64 of 32 bytes is 44 characters with a single `=` pad.
    assert_eq!(private_key.len(), 44);
    assert_eq!(public_key.len(), 44);
    assert!(private_key.ends_with('='));
    assert!(public_key.ends_with('='));

    let private_bytes = decode(&private_key);
    assert_eq!(private_bytes.len(), 32);
    // `wg genkey` clamps the private key before encoding it.
    assert_eq!(private_bytes[0] & 0b0000_0111, 0);
    assert_eq!(private_bytes[31] & 0b1000_0000, 0);
    assert_eq!(private_bytes[31] & 0b0100_0000, 0b0100_0000);

    assert_eq!(
        derive_public_key(private_key).expect("generated key derives"),
        public_key
    );
}

#[test]
fn two_generated_keypairs_differ() {
    let first = generate_keypair();
    let second = generate_keypair();

    assert_ne!(first.private_key, second.private_key);
    assert_ne!(first.public_key, second.public_key);
}

#[test]
fn rejects_a_key_of_the_wrong_length() {
    let short = STANDARD.encode([0u8; 31]);
    let long = STANDARD.encode([0u8; 33]);

    for key in [short, long] {
        let error = derive_public_key(key.clone()).expect_err("wrong length must fail");
        assert!(!error.contains(&key), "error must not echo the key");
        assert!(error.contains("32"), "error should state the expected size");
    }
}

#[test]
fn rejects_non_base64_input_without_echoing_it() {
    let garbage = "this-is-not-base64!!";

    let error = derive_public_key(garbage.to_string()).expect_err("garbage must fail");
    assert!(!error.contains(garbage), "error must not echo the key");
}

#[test]
fn never_panics_on_arbitrary_input() {
    // Deterministic corpus: fixed edge cases plus pseudo-random ASCII strings.
    let mut corpus: Vec<String> = vec![
        String::new(),
        "=".to_string(),
        "====".to_string(),
        "A".repeat(43),
        "A".repeat(44),
        "A".repeat(45),
        format!("{}=", "A".repeat(43)),
        "\u{1f600}\u{1f600}".to_string(),
        "a\0b".to_string(),
        STANDARD.encode([0u8; 0]),
        STANDARD.encode([0u8; 34]),
        RFC7748_PRIVATE.to_string(),
    ];

    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    for _ in 0..256 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let len = (state % 96) as usize;
        let text: String = (0..len)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                char::from(b' ' + (state % 95) as u8)
            })
            .collect();
        corpus.push(text);
    }

    for input in corpus {
        // Any Result is acceptable; the property under test is that the call
        // returns rather than panicking.
        let _ = derive_public_key(input);
    }
}
