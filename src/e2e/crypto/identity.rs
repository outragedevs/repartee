//! Ed25519 long-term identity keypair.

use ed25519_dalek::{SigningKey, VerifyingKey, SECRET_KEY_LENGTH};
use zeroize::Zeroizing;

use crate::e2e::error::Result;

/// Long-term identity keypair for a local or remote peer.
///
/// The secret key is stored in a `Zeroizing` wrapper so that dropping it
/// clears the memory. Serialization is raw 32-byte arrays for interop with
/// libsodium-based clients.
#[derive(Debug)]
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    /// Generate a fresh identity using the OS CSPRNG.
    pub fn generate() -> Result<Self> {
        let mut seed = Zeroizing::new([0u8; SECRET_KEY_LENGTH]);
        rand::fill(seed.as_mut_slice());
        Ok(Self {
            signing: SigningKey::from_bytes(&seed),
        })
    }

    /// Load identity from a raw 32-byte secret seed.
    #[must_use]
    pub fn from_secret_bytes(seed: &[u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(seed),
        }
    }

    #[must_use]
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    #[must_use]
    pub fn public_bytes(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    #[must_use]
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_generate_produces_32_byte_keys() {
        let id = Identity::generate().expect("generate");
        assert_eq!(id.secret_bytes().len(), 32);
        assert_eq!(id.public_bytes().len(), 32);
    }

    #[test]
    fn identity_from_secret_is_deterministic() {
        let seed = [7u8; 32];
        let a = Identity::from_secret_bytes(&seed);
        let b = Identity::from_secret_bytes(&seed);
        assert_eq!(a.public_bytes(), b.public_bytes());
    }

    #[test]
    fn identity_distinct_seeds_produce_distinct_publics() {
        let a = Identity::from_secret_bytes(&[1u8; 32]);
        let b = Identity::from_secret_bytes(&[2u8; 32]);
        assert_ne!(a.public_bytes(), b.public_bytes());
    }
}
