//! Fingerprint derivation and human-readable encoding.

use bip39::{Language, Mnemonic};
use sha2::{Digest, Sha256};

use crate::e2e::error::{E2eError, Result};

/// 16-byte truncated SHA-256 of an Ed25519 public key.
pub type Fingerprint = [u8; 16];

#[must_use]
pub fn fingerprint(pubkey: &[u8; 32]) -> Fingerprint {
    let mut hasher = Sha256::new();
    hasher.update(b"RPE2E01-FP:");
    hasher.update(pubkey);
    let full = hasher.finalize();
    let mut fp = [0u8; 16];
    fp.copy_from_slice(&full[..16]);
    fp
}

#[must_use]
pub fn fingerprint_hex(fp: &Fingerprint) -> String {
    hex::encode(fp)
}

/// Encode a 16-byte fingerprint as 6 BIP-39 words.
///
/// We use the low 11*6 = 66 bits of the fingerprint. (BIP-39 normally
/// derives word count from entropy length; we encode directly.)
pub fn fingerprint_bip39(fp: &Fingerprint) -> Result<String> {
    // Pad fingerprint to 128 bits (16 bytes = 128 bits) -> BIP-39 mnemonic
    // with 12 words. We take the first 6 words for a more compact SAS.
    let mnemonic = Mnemonic::from_entropy_in(Language::English, fp)
        .map_err(|e| E2eError::Crypto(format!("bip39: {e}")))?;
    let words: Vec<String> = mnemonic.words().take(6).map(String::from).collect();
    Ok(words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_16_bytes() {
        let pk = [0u8; 32];
        let fp = fingerprint(&pk);
        assert_eq!(fp.len(), 16);
    }

    #[test]
    fn fingerprint_deterministic() {
        let pk = [42u8; 32];
        assert_eq!(fingerprint(&pk), fingerprint(&pk));
    }

    #[test]
    fn fingerprint_domain_separation_prevents_raw_sha256_collision() {
        // Our fingerprint includes a prefix; it must NOT equal raw SHA-256[..16].
        let pk = [7u8; 32];
        let raw = {
            let mut h = Sha256::new();
            h.update(pk);
            let d = h.finalize();
            let mut out = [0u8; 16];
            out.copy_from_slice(&d[..16]);
            out
        };
        assert_ne!(fingerprint(&pk), raw);
    }

    #[test]
    fn bip39_encoding_produces_six_words() {
        let fp = [0xab; 16];
        let sas = fingerprint_bip39(&fp).expect("bip39");
        assert_eq!(sas.split_whitespace().count(), 6);
    }

    #[test]
    fn bip39_encoding_deterministic() {
        let fp = [0xcd; 16];
        assert_eq!(
            fingerprint_bip39(&fp).unwrap(),
            fingerprint_bip39(&fp).unwrap()
        );
    }
}
