//! SASL `ECDSA-NIST256P-CHALLENGE` client implementation.
//!
//! The mechanism authenticates by signing a random challenge with a NIST P-256
//! private key: no password, no shared secret, and nothing derived from the key
//! ever crosses the wire. Services register the *public* key
//! (`/msg NickServ SET PUBKEY …`); the private half stays in a PEM file.
//!
//! The exchange, as implemented by atheme's `ecdsa-nist256p-challenge` module
//! and by `WeeChat`'s client:
//!
//! 1. `AUTHENTICATE ECDSA-NIST256P-CHALLENGE`
//! 2. server → `AUTHENTICATE +`
//! 3. client → base64(`<authzid>\0<authcid>`), both the account name
//! 4. server → `AUTHENTICATE <base64 challenge>` — 32 random bytes
//! 5. client → base64(DER ECDSA signature over those bytes)
//! 6. server → `903` / `904`
//!
//! Two details are load-bearing and easy to get wrong:
//!
//! - The challenge is signed **as a prehash** — it is fed to ECDSA where a
//!   message digest would go, with no further hashing. Both reference
//!   implementations call `ECDSA_sign(0, challenge, len, …)` /
//!   `ECDSA_verify(0, challenge, len, …)`, which treat the input as an
//!   already-computed digest.
//! - The signature must be **DER**, because OpenSSL's `ECDSA_sign` emits DER
//!   and the server parses it as such. The fixed 64-byte `(r, s)` form — what
//!   most Rust signature APIs hand you by default — is rejected.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr as _, eyre};
use p256::SecretKey;
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::pkcs8::DecodePrivateKey as _;

/// Shortest challenge we will sign.
///
/// P-256 takes the leftmost 256 bits of the digest, so a short challenge is
/// silently left-padded rather than rejected by the maths — but a server that
/// sends one is not implementing this mechanism, and signing whatever it sent
/// would be a signing oracle. Atheme's challenge is 32 bytes.
const MIN_CHALLENGE_BYTES: usize = 16;

/// Build the first client message: `<authzid>\0<authcid>`, both the account
/// name.
///
/// The same shape SASL PLAIN sends, minus the password — the server needs to
/// know which account's public key to check the signature against.
#[must_use]
pub fn authcid_payload(user: &str) -> Vec<u8> {
    let user = crate::irc::sasl_scram::saslprep(user);
    let mut payload = Vec::with_capacity(user.len() * 2 + 1);
    payload.extend_from_slice(user.as_bytes());
    payload.push(0);
    payload.extend_from_slice(user.as_bytes());
    payload
}

/// Resolve a configured `sasl_key_path` to a real path.
///
/// `~/…` expands to the home directory and absolute paths are taken as they
/// are; anything else resolves against `~/.repartee/certs`, so
/// `sasl_key_path = "libera.pem"` works without spelling out a full path.
/// Resolving relative to the working directory instead would make a key
/// resolve differently depending on where the client was launched from.
#[must_use]
pub fn resolve_key_path(configured: &str) -> PathBuf {
    if let Some(rest) = configured.strip_prefix("~/") {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(rest);
    }
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::constants::certs_dir().join(path)
    }
}

/// Parse a PEM P-256 private key.
///
/// Both encodings are accepted: `ecdsatool` writes SEC1 (`EC PRIVATE KEY`) and
/// OpenSSL 3 writes PKCS#8 (`PRIVATE KEY`), and a user has no reason to care
/// which one their generator produced.
///
/// # Errors
///
/// Returns an error naming the two accepted encodings if the input is neither.
/// The error never echoes the input — it is a private key.
pub fn load_key(pem: &str) -> Result<SigningKey> {
    if let Ok(secret) = SecretKey::from_sec1_pem(pem) {
        return Ok(SigningKey::from(secret));
    }
    SecretKey::from_pkcs8_pem(pem)
        .map(SigningKey::from)
        .map_err(|e| {
            eyre!(
                "expected a PEM NIST P-256 private key — either SEC1 \
                 (\"BEGIN EC PRIVATE KEY\") or PKCS#8 (\"BEGIN PRIVATE KEY\"): {e}"
            )
        })
}

/// Read and parse the P-256 private key at `path`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or does not hold a P-256 key.
/// Both messages name the path and neither includes the file's contents.
pub fn load_key_file(path: &Path) -> Result<SigningKey> {
    let pem = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("cannot read SASL key {}", path.display()))?;
    load_key(&pem).wrap_err_with(|| format!("cannot use SASL key {}", path.display()))
}

/// Sign a server challenge, returning the DER signature to base64 and send.
///
/// # Errors
///
/// Returns an error if the challenge is implausibly short or if signing fails.
pub fn sign_challenge(key: &SigningKey, challenge: &[u8]) -> Result<Vec<u8>> {
    if challenge.len() < MIN_CHALLENGE_BYTES {
        return Err(eyre!(
            "ECDSA-NIST256P-CHALLENGE: server challenge is {} bytes, expected at least \
             {MIN_CHALLENGE_BYTES}",
            challenge.len()
        ));
    }
    let signature: p256::ecdsa::Signature = key
        .sign_prehash(challenge)
        .map_err(|e| eyre!("ECDSA-NIST256P-CHALLENGE: cannot sign the challenge: {e}"))?;
    Ok(signature.to_der().as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::hazmat::PrehashVerifier;

    /// A throwaway P-256 key, in both PEM encodings. Generated for this test
    /// with `openssl ecparam -genkey -name prime256v1` and never used anywhere
    /// else — it authenticates to nothing.
    const SEC1_PEM: &str = "\
-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIJPs4/mrOjo5IUswCWuzw6AXrr8XkSqvO4Y0EeDNsOByoAoGCCqGSM49
AwEHoUQDQgAEBznG72VV2v/dEYvJevrBAnhW+uoQUtVWuFXXSGxZWMl6xR8VgzoI
p72n2RTnVCJ1aKv3iRxTWAC1sYSuxS448A==
-----END EC PRIVATE KEY-----
";

    const PKCS8_PEM: &str = "\
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgk+zj+as6OjkhSzAJ
a7PDoBeuvxeRKq87hjQR4M2w4HKhRANCAAQHOcbvZVXa/90Ri8l6+sECeFb66hBS
1Va4VddIbFlYyXrFHxWDOginvafZFOdUInVoq/eJHFNYALWxhK7FLjjw
-----END PRIVATE KEY-----
";

    #[test]
    fn both_pem_encodings_load_the_same_key() {
        let sec1 = load_key(SEC1_PEM).expect("SEC1 PEM must load");
        let pkcs8 = load_key(PKCS8_PEM).expect("PKCS#8 PEM must load");
        assert_eq!(
            sec1.verifying_key().to_encoded_point(true).as_bytes(),
            pkcs8.verifying_key().to_encoded_point(true).as_bytes(),
            "ecdsatool's SEC1 and OpenSSL 3's PKCS#8 are the same key"
        );
    }

    #[test]
    fn a_bad_pem_is_reported_without_echoing_it() {
        let secretish = "-----BEGIN EC PRIVATE KEY-----\nSUPERSECRETNOTBASE64\n";
        let err = load_key(secretish).unwrap_err().to_string();
        assert!(
            err.contains("SEC1") && err.contains("PKCS#8"),
            "the error must say what is accepted: {err}"
        );
        assert!(
            !err.contains("SUPERSECRETNOTBASE64"),
            "the error must never echo key material: {err}"
        );

        assert!(load_key("").is_err());
        assert!(load_key("not a pem at all").is_err());
    }

    /// The end-to-end property the server checks: a DER signature over the
    /// challenge *as a prehash*. Verifying with the same convention is what
    /// atheme does with `ECDSA_verify(0, challenge, len, …)`.
    #[test]
    fn a_signed_challenge_verifies_against_the_public_key() {
        let key = load_key(SEC1_PEM).unwrap();
        let challenge: Vec<u8> = (0u8..32).collect();

        let der = sign_challenge(&key, &challenge).expect("signing must succeed");
        let parsed = p256::ecdsa::Signature::from_der(&der)
            .expect("the wire format is DER, not the fixed 64-byte form");

        let verifying: &VerifyingKey = key.verifying_key();
        verifying
            .verify_prehash(&challenge, &parsed)
            .expect("the server must be able to verify what we send");

        // And it is a signature over *this* challenge, not a constant.
        let mut other = challenge;
        other[0] ^= 0xFF;
        assert!(verifying.verify_prehash(&other, &parsed).is_err());
    }

    #[test]
    fn the_signature_is_der_not_the_fixed_width_form() {
        let key = load_key(SEC1_PEM).unwrap();
        let der = sign_challenge(&key, &(0u8..32).collect::<Vec<_>>()).unwrap();
        // DER SEQUENCE of two INTEGERs: tag 0x30, and never exactly the 64
        // bytes of the fixed-width encoding that OpenSSL would reject.
        assert_eq!(der.first(), Some(&0x30));
        assert_ne!(der.len(), 64);
    }

    #[test]
    fn an_implausibly_short_challenge_is_refused() {
        let key = load_key(SEC1_PEM).unwrap();
        let err = sign_challenge(&key, b"tiny").unwrap_err().to_string();
        assert!(err.contains("4 bytes"), "unexpected error: {err}");
        assert!(sign_challenge(&key, &[]).is_err());
        // 16 bytes is the floor, and is signed.
        assert!(sign_challenge(&key, &[7u8; 16]).is_ok());
    }

    #[test]
    fn the_first_message_is_authzid_nul_authcid() {
        assert_eq!(authcid_payload("bob"), b"bob\0bob");
        // No trailing NUL, and nothing else appended — a password field here
        // would be a different mechanism.
        assert_eq!(authcid_payload("a"), b"a\0a");
        assert_eq!(authcid_payload("").len(), 1);
    }

    #[test]
    fn a_relative_key_path_resolves_under_the_certs_directory() {
        let relative = resolve_key_path("libera.pem");
        assert_eq!(relative, crate::constants::certs_dir().join("libera.pem"));

        let absolute = resolve_key_path("/etc/repartee/key.pem");
        assert_eq!(absolute, PathBuf::from("/etc/repartee/key.pem"));

        let tilde = resolve_key_path("~/keys/libera.pem");
        assert!(tilde.is_absolute(), "~ must expand: {}", tilde.display());
        assert!(tilde.ends_with("keys/libera.pem"));
        assert!(!tilde.starts_with("~"));
    }

    #[test]
    fn a_missing_key_file_names_the_path() {
        let path = Path::new("/nonexistent/repartee-test/key.pem");
        let err = load_key_file(path).unwrap_err().to_string();
        assert!(err.contains("/nonexistent/repartee-test/key.pem"), "{err}");
    }

    #[test]
    fn a_key_file_on_disk_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.pem");
        std::fs::write(&path, SEC1_PEM).unwrap();
        let from_file = load_key_file(&path).expect("a real PEM on disk must load");
        assert_eq!(
            from_file.verifying_key().to_encoded_point(true).as_bytes(),
            load_key(SEC1_PEM)
                .unwrap()
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
        );
    }
}
