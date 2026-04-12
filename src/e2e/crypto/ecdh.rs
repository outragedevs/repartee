//! X25519 ECDH + HKDF-SHA256 wrap key derivation.
//!
//! Also exposes the RFC 7748 Appendix A birational map between Ed25519 and
//! X25519 (Montgomery form) keys, used by the REKEY distribution path to
//! encrypt a freshly generated session key to a peer whose only stable key
//! is their Ed25519 identity. These helpers match libsodium's
//! `crypto_sign_ed25519_pk_to_curve25519` and
//! `crypto_sign_ed25519_sk_to_curve25519` so Perl/Python test scripts based
//! on libsodium interoperate with Rust consumers on the same wire form.

use ed25519_dalek::{SigningKey, VerifyingKey};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::e2e::error::{E2eError, Result};

/// Ephemeral X25519 keypair used for key wrap during handshake.
pub struct EphemeralKeypair {
    secret: StaticSecret,
    public: PublicKey,
}

impl std::fmt::Debug for EphemeralKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EphemeralKeypair")
            .field("public", &hex::encode(self.public.to_bytes()))
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl EphemeralKeypair {
    pub fn generate() -> Result<Self> {
        let mut seed = Zeroizing::new([0u8; 32]);
        rand::fill(seed.as_mut_slice());
        let secret = StaticSecret::from(*seed);
        let public = PublicKey::from(&secret);
        Ok(Self { secret, public })
    }

    #[must_use]
    pub fn public_bytes(&self) -> [u8; 32] {
        self.public.to_bytes()
    }

    /// Perform X25519 ECDH and derive a 32-byte wrap key via HKDF-SHA256.
    ///
    /// The HKDF `info` string binds the wrap key to the RPE2E protocol and
    /// to the handshake context (sender/recipient handles, channel).
    #[must_use]
    pub fn derive_wrap_key(&self, peer_pub: &[u8; 32], info: &[u8]) -> [u8; 32] {
        let peer = PublicKey::from(*peer_pub);
        let shared = self.secret.diffie_hellman(&peer);
        let hk = Hkdf::<Sha256>::new(Some(b"RPE2E01-WRAP"), shared.as_bytes());
        let mut okm = [0u8; 32];
        hk.expand(info, &mut okm).expect("hkdf expand 32 bytes");
        okm
    }
}

/// Static ECDH from a persistent X25519 secret (not currently used in v1,
/// reserved for future long-term X25519 identity key).
#[allow(dead_code)]
#[must_use]
pub fn static_derive_wrap_key(
    my_secret: &[u8; 32],
    peer_public: &[u8; 32],
    info: &[u8],
) -> [u8; 32] {
    let secret = StaticSecret::from(*my_secret);
    let shared = secret.diffie_hellman(&PublicKey::from(*peer_public));
    let hk = Hkdf::<Sha256>::new(Some(b"RPE2E01-WRAP"), shared.as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm).expect("hkdf expand 32 bytes");
    okm
}

/// Convert an Ed25519 verifying key to its X25519 public key counterpart
/// via the RFC 7748 Appendix A birational map. Used for deriving long-lived
/// ECDH endpoints from Ed25519 identity keys, which lets the initiator of a
/// REKEY distribution encrypt to a peer whose only stable key is their
/// Ed25519 identity.
///
/// Matches libsodium's `crypto_sign_ed25519_pk_to_curve25519`.
pub fn ed25519_pub_to_x25519(ed_pub: &[u8; 32]) -> Result<[u8; 32]> {
    let vk = VerifyingKey::from_bytes(ed_pub)
        .map_err(|e| E2eError::Crypto(format!("invalid ed25519 pub: {e}")))?;
    Ok(vk.to_montgomery().to_bytes())
}

/// Convert an Ed25519 secret seed (the 32-byte seed, NOT an expanded
/// secret key) to an X25519 scalar suitable for `x25519_dalek::StaticSecret`.
///
/// Matches libsodium's `crypto_sign_ed25519_sk_to_curve25519`, which
/// internally takes the first 32 bytes of `SHA-512(seed)` and applies the
/// standard clamping — `ed25519-dalek`'s `SigningKey::to_scalar_bytes` does
/// exactly this. See the `ed25519-dalek-2.2.0/tests/x25519.rs` vector test
/// (RFC 8032 §7.1 seeds) which is mirrored below in `ed25519_to_x25519_rfc8032_vectors`
/// to guarantee byte-for-byte interop with libsodium-based peers.
#[must_use]
pub fn ed25519_seed_to_x25519(ed_seed: &[u8; 32]) -> [u8; 32] {
    let signing = SigningKey::from_bytes(ed_seed);
    signing.to_scalar_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a 64-char hex string into a 32-byte array (test-only helper).
    fn h32(s: &str) -> [u8; 32] {
        let v = hex::decode(s).expect("bad hex");
        assert_eq!(v.len(), 32);
        let mut out = [0u8; 32];
        out.copy_from_slice(&v);
        out
    }

    #[test]
    fn ecdh_roundtrip_yields_same_shared() {
        let alice = EphemeralKeypair::generate().unwrap();
        let bob = EphemeralKeypair::generate().unwrap();
        let info = b"test-context";
        let k_ab = alice.derive_wrap_key(&bob.public_bytes(), info);
        let k_ba = bob.derive_wrap_key(&alice.public_bytes(), info);
        assert_eq!(k_ab, k_ba);
        assert_eq!(k_ab.len(), 32);
    }

    #[test]
    fn ecdh_different_info_yields_different_keys() {
        let alice = EphemeralKeypair::generate().unwrap();
        let bob = EphemeralKeypair::generate().unwrap();
        let k1 = alice.derive_wrap_key(&bob.public_bytes(), b"ctx-1");
        let k2 = alice.derive_wrap_key(&bob.public_bytes(), b"ctx-2");
        assert_ne!(k1, k2);
    }

    /// RFC 8032 §7.1 test vectors, also used by `ed25519-dalek-2.2.0`'s
    /// `tests/x25519.rs`. Asserts byte-for-byte agreement with libsodium's
    /// `crypto_sign_ed25519_{sk,pk}_to_curve25519` so Perl/Python peers
    /// using libsodium end up with the same X25519 keypair as we do.
    #[test]
    fn ed25519_to_x25519_rfc8032_vectors() {
        let seed_a = h32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let seed_b = h32("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb");

        let signing_a = SigningKey::from_bytes(&seed_a);
        let signing_b = SigningKey::from_bytes(&seed_b);

        let scalar_a = ed25519_seed_to_x25519(&seed_a);
        let scalar_b = ed25519_seed_to_x25519(&seed_b);

        let x_sk_a = StaticSecret::from(scalar_a);
        let x_sk_b = StaticSecret::from(scalar_b);

        // Known-good X25519 publics derived from the Ed25519 seeds via
        // libsodium's map — taken verbatim from ed25519-dalek's test vectors.
        assert_eq!(
            PublicKey::from(&x_sk_a).to_bytes(),
            h32("d85e07ec22b0ad881537c2f44d662d1a143cf830c57aca4305d85c7a90f6b62e")
        );
        assert_eq!(
            PublicKey::from(&x_sk_b).to_bytes(),
            h32("25c704c594b88afc00a76b69d1ed2b984d7e22550f3ed0802d04fbcd07d38d47")
        );

        // The birational map on the public side must agree with the scalar-mult
        // derivation from the secret side.
        let pub_from_ed_a = ed25519_pub_to_x25519(&signing_a.verifying_key().to_bytes()).unwrap();
        let pub_from_ed_b = ed25519_pub_to_x25519(&signing_b.verifying_key().to_bytes()).unwrap();
        assert_eq!(pub_from_ed_a, PublicKey::from(&x_sk_a).to_bytes());
        assert_eq!(pub_from_ed_b, PublicKey::from(&x_sk_b).to_bytes());

        // End-to-end DH shared secret from the RFC test vector.
        let expected_shared =
            h32("5166f24a6918368e2af831a4affadd97af0ac326bdf143596c045967cc00230e");
        assert_eq!(
            x_sk_a
                .diffie_hellman(&PublicKey::from(pub_from_ed_b))
                .to_bytes(),
            expected_shared,
        );
        assert_eq!(
            x_sk_b
                .diffie_hellman(&PublicKey::from(pub_from_ed_a))
                .to_bytes(),
            expected_shared,
        );
    }

    #[test]
    fn ed25519_to_x25519_roundtrip_via_identity() {
        use crate::e2e::crypto::identity::Identity;
        let id = Identity::generate().unwrap();
        let seed = id.secret_bytes();
        let pub_ed = id.public_bytes();
        let scalar = ed25519_seed_to_x25519(&seed);
        let pub_from_secret = PublicKey::from(&StaticSecret::from(scalar)).to_bytes();
        let pub_from_ed = ed25519_pub_to_x25519(&pub_ed).unwrap();
        assert_eq!(pub_from_secret, pub_from_ed);
    }
}
