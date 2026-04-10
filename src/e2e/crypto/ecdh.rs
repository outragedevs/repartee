//! X25519 ECDH + HKDF-SHA256 wrap key derivation.

use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::e2e::error::Result;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
