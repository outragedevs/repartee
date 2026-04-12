//! XChaCha20-Poly1305 AEAD wrapper used for both session-key wrapping and
//! message encryption.

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};

use crate::e2e::error::{E2eError, Result};

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;

/// Raw 32-byte symmetric session key.
pub type SessionKey = [u8; KEY_LEN];

/// 24-byte XChaCha20 nonce.
pub type Nonce = [u8; NONCE_LEN];

/// Encrypt `plaintext` with `aad` as additional authenticated data.
///
/// A fresh random nonce is generated for each call. Returns `(nonce, ciphertext)`
/// where `ciphertext` includes the Poly1305 tag.
pub fn encrypt(key: &SessionKey, aad: &[u8], plaintext: &[u8]) -> Result<(Nonce, Vec<u8>)> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| E2eError::Crypto(format!("aead encrypt: {e}")))?;
    let mut n = [0u8; NONCE_LEN];
    n.copy_from_slice(nonce.as_slice());
    Ok((n, ct))
}

/// Decrypt `ciphertext` using `nonce` and `aad`.
pub fn decrypt(key: &SessionKey, nonce: &Nonce, aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let xnonce = XNonce::from_slice(nonce);
    cipher
        .decrypt(
            xnonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|e| E2eError::Crypto(format!("aead decrypt: {e}")))
}

/// Generate a fresh 32-byte session key.
pub fn generate_session_key() -> Result<SessionKey> {
    let mut key = [0u8; KEY_LEN];
    rand::fill(&mut key);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aead_roundtrip() {
        let key = generate_session_key().unwrap();
        let aad = b"RPE2E01:sender@host:#chan:msgid:ts:1:1";
        let pt = b"hello world";
        let (nonce, ct) = encrypt(&key, aad, pt).unwrap();
        let pt2 = decrypt(&key, &nonce, aad, &ct).unwrap();
        assert_eq!(pt2, pt);
    }

    #[test]
    fn aead_aad_mismatch_fails() {
        let key = generate_session_key().unwrap();
        let aad1 = b"ctx-1";
        let aad2 = b"ctx-2";
        let pt = b"secret";
        let (nonce, ct) = encrypt(&key, aad1, pt).unwrap();
        assert!(decrypt(&key, &nonce, aad2, &ct).is_err());
    }

    #[test]
    fn aead_key_mismatch_fails() {
        let key1 = generate_session_key().unwrap();
        let key2 = generate_session_key().unwrap();
        let aad = b"ctx";
        let pt = b"secret";
        let (nonce, ct) = encrypt(&key1, aad, pt).unwrap();
        assert!(decrypt(&key2, &nonce, aad, &ct).is_err());
    }

    #[test]
    fn aead_ciphertext_tamper_fails() {
        let key = generate_session_key().unwrap();
        let aad = b"ctx";
        let pt = b"secret message";
        let (nonce, mut ct) = encrypt(&key, aad, pt).unwrap();
        ct[0] ^= 0x01;
        assert!(decrypt(&key, &nonce, aad, &ct).is_err());
    }
}
