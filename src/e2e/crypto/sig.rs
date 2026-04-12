//! Ed25519 signature helpers for handshake messages.

use ed25519_dalek::{SIGNATURE_LENGTH, Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::e2e::error::{E2eError, Result};

pub const SIG_LEN: usize = SIGNATURE_LENGTH;

#[must_use]
pub fn sign(signing: &SigningKey, message: &[u8]) -> [u8; SIG_LEN] {
    signing.sign(message).to_bytes()
}

pub fn verify(pubkey: &[u8; 32], message: &[u8], sig: &[u8; SIG_LEN]) -> Result<()> {
    let vk = VerifyingKey::from_bytes(pubkey)
        .map_err(|e| E2eError::Crypto(format!("invalid verifying key: {e}")))?;
    let sig = Signature::from_bytes(sig);
    vk.verify(message, &sig)
        .map_err(|e| E2eError::Crypto(format!("signature verify: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2e::crypto::identity::Identity;

    #[test]
    fn sign_and_verify_roundtrip() {
        let id = Identity::generate().unwrap();
        let msg = b"handshake payload";
        let sig = sign(id.signing_key(), msg);
        verify(&id.public_bytes(), msg, &sig).expect("verify ok");
    }

    #[test]
    fn verify_fails_on_tamper() {
        let id = Identity::generate().unwrap();
        let sig = sign(id.signing_key(), b"orig");
        assert!(verify(&id.public_bytes(), b"tampered", &sig).is_err());
    }

    #[test]
    fn verify_fails_with_wrong_key() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let sig = sign(a.signing_key(), b"msg");
        assert!(verify(&b.public_bytes(), b"msg", &sig).is_err());
    }
}
