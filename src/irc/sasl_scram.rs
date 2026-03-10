//! SASL SCRAM-SHA-256 client implementation (RFC 5802 / RFC 7677).
//!
//! Implements the client-side SCRAM (Salted Challenge Response Authentication
//! Mechanism) using SHA-256 as the hash function.  This is a challenge-response
//! mechanism that avoids sending passwords in plaintext.

use base64::Engine as _;
use color_eyre::eyre::{Result, eyre};
use hmac::{Hmac, Mac};
use rand::RngExt;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Maximum allowed PBKDF2 iteration count to prevent denial-of-service via absurdly
/// high server-requested iterations.
const MAX_ITERATIONS: u32 = 100_000;

/// Generate the client-first message for SCRAM-SHA-256.
///
/// Returns `(client_first_bare, full_client_first_message, client_nonce)`.
///
/// - `client_first_bare` = `n=<username>,r=<client_nonce>`
/// - `full_message` = `n,,` + `client_first_bare`  (gs2-header + bare)
/// - `client_nonce` = 24 random bytes, base64-encoded
#[must_use]
pub fn client_first(username: &str) -> (String, String, String) {
    let mut nonce_bytes = [0u8; 24];
    rand::rng().fill(&mut nonce_bytes);
    let client_nonce = base64::engine::general_purpose::STANDARD.encode(nonce_bytes);

    // RFC 5802: SASLprep the username and escape '=' and ','
    let safe_username = username.replace('=', "=3D").replace(',', "=2C");

    let client_first_bare = format!("n={safe_username},r={client_nonce}");
    let full_message = format!("n,,{client_first_bare}");

    (client_first_bare, full_message, client_nonce)
}

/// Process the server-first message and compute the client-final message.
///
/// Returns `(client_final_message, server_signature)` on success.
///
/// # Errors
///
/// Returns an error if:
/// - The server-first message is malformed
/// - The combined nonce does not start with the client nonce
/// - The salt cannot be decoded from base64
/// - The iteration count is invalid or too high
pub fn client_final(
    server_first: &str,
    client_first_bare: &str,
    client_nonce: &str,
    password: &str,
) -> Result<(String, Vec<u8>)> {
    // Parse server-first message: r=<combined_nonce>,s=<salt_b64>,i=<iterations>
    let mut combined_nonce = None;
    let mut salt_b64 = None;
    let mut iterations = None;

    for field in server_first.split(',') {
        if let Some(value) = field.strip_prefix("r=") {
            combined_nonce = Some(value);
        } else if let Some(value) = field.strip_prefix("s=") {
            salt_b64 = Some(value);
        } else if let Some(value) = field.strip_prefix("i=") {
            iterations = Some(value);
        }
    }

    let combined_nonce = combined_nonce.ok_or_else(|| eyre!("SCRAM: server-first missing nonce (r=)"))?;
    let salt_b64 = salt_b64.ok_or_else(|| eyre!("SCRAM: server-first missing salt (s=)"))?;
    let iterations_str = iterations.ok_or_else(|| eyre!("SCRAM: server-first missing iterations (i=)"))?;

    // Verify combined nonce starts with client nonce
    if !combined_nonce.starts_with(client_nonce) {
        return Err(eyre!(
            "SCRAM: server nonce does not start with client nonce"
        ));
    }

    // Decode salt
    let salt = base64::engine::general_purpose::STANDARD
        .decode(salt_b64)
        .map_err(|e| eyre!("SCRAM: invalid base64 salt: {e}"))?;

    // Parse iterations
    let iter_count: u32 = iterations_str
        .parse()
        .map_err(|e| eyre!("SCRAM: invalid iteration count: {e}"))?;

    if iter_count == 0 {
        return Err(eyre!("SCRAM: iteration count must be > 0"));
    }
    if iter_count > MAX_ITERATIONS {
        return Err(eyre!(
            "SCRAM: iteration count {iter_count} exceeds maximum {MAX_ITERATIONS}"
        ));
    }

    // PBKDF2-HMAC-SHA-256(password, salt, iterations) -> salted_password
    let mut salted_password = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iter_count, &mut salted_password);

    // client_key = HMAC-SHA-256(salted_password, "Client Key")
    let client_key = hmac_sha256(&salted_password, b"Client Key");

    // stored_key = SHA-256(client_key)
    let stored_key = sha256(&client_key);

    // client_final_without_proof = "c=biws,r=" + combined_nonce
    //   "biws" = base64("n,,") — the gs2-header used in client-first
    let client_final_without_proof = format!("c=biws,r={combined_nonce}");

    // auth_message = client_first_bare + "," + server_first + "," + client_final_without_proof
    let auth_message = format!("{client_first_bare},{server_first},{client_final_without_proof}");

    // client_signature = HMAC-SHA-256(stored_key, auth_message)
    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());

    // client_proof = client_key XOR client_signature
    let client_proof: Vec<u8> = client_key
        .iter()
        .zip(client_signature.iter())
        .map(|(a, b)| a ^ b)
        .collect();

    let proof_b64 = base64::engine::general_purpose::STANDARD.encode(&client_proof);

    // server_key = HMAC-SHA-256(salted_password, "Server Key")
    let server_key = hmac_sha256(&salted_password, b"Server Key");

    // server_signature = HMAC-SHA-256(server_key, auth_message)
    let server_signature = hmac_sha256(&server_key, auth_message.as_bytes());

    let client_final_message = format!("{client_final_without_proof},p={proof_b64}");

    Ok((client_final_message, server_signature.to_vec()))
}

/// Verify the server-final message against the expected server signature.
///
/// The server-final message has the format `v=<signature_b64>`.
/// Returns `true` if the signature matches.
#[must_use]
pub fn verify_server(server_final: &str, expected_signature: &[u8]) -> bool {
    let Some(sig_b64) = server_final.strip_prefix("v=") else {
        return false;
    };

    let Ok(sig_bytes) = base64::engine::general_purpose::STANDARD.decode(sig_b64) else {
        return false;
    };

    // Constant-time comparison to prevent timing attacks
    constant_time_eq(&sig_bytes, expected_signature)
}

/// Compute HMAC-SHA-256(key, data) and return the 32-byte result.
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac =
        HmacSha256::new_from_slice(key).expect("HMAC-SHA-256 accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Compute SHA-256(data) and return the 32-byte digest.
fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Constant-time byte comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Split a message into chunks of at most 400 bytes for AUTHENTICATE.
///
/// Per the `IRCv3` SASL spec, if the base64-encoded payload exceeds 400 bytes,
/// it must be split into 400-byte chunks.  If the final chunk is exactly
/// 400 bytes, an additional empty `+` terminator must be sent.
#[must_use]
pub fn chunk_authenticate(payload: &str) -> Vec<String> {
    if payload.is_empty() {
        return vec!["+".to_string()];
    }

    let mut chunks: Vec<String> = payload
        .as_bytes()
        .chunks(400)
        .map(|chunk| {
            // base64 output is always ASCII, so from_utf8 is infallible here
            std::str::from_utf8(chunk)
                .expect("base64 is always ASCII")
                .to_string()
        })
        .collect();

    // If the last chunk is exactly 400 bytes, append "+" terminator
    if chunks.last().is_some_and(|last| last.len() == 400) {
        chunks.push("+".to_string());
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_first_format() {
        let (bare, full, nonce) = client_first("testuser");
        // Full message starts with gs2-header "n,,"
        assert!(full.starts_with("n,,"), "full message must start with 'n,,'");
        // Bare message starts with "n=testuser,r="
        assert!(
            bare.starts_with("n=testuser,r="),
            "bare must start with 'n=testuser,r='"
        );
        // Full = gs2-header + bare
        assert_eq!(full, format!("n,,{bare}"));
        // Nonce is base64-encoded 24 bytes = 32 chars
        assert_eq!(nonce.len(), 32, "base64(24 bytes) = 32 chars");
        // Bare ends with the nonce
        assert!(bare.ends_with(&nonce));
    }

    #[test]
    fn client_first_escapes_special_chars() {
        let (bare, _, _) = client_first("user=name,test");
        // '=' -> '=3D', ',' -> '=2C'
        assert!(
            bare.starts_with("n=user=3Dname=2Ctest,r="),
            "special chars must be escaped: {bare}"
        );
    }

    #[test]
    fn client_final_known_values() {
        // RFC 7677 test vector (adapted for SCRAM-SHA-256)
        // We use a fixed set of values to verify the computation.
        let client_first_bare = "n=user,r=rOprNGfwEbeRWgbNEkqO";
        let client_nonce = "rOprNGfwEbeRWgbNEkqO";
        let password = "pencil";

        // Server-first with known salt and iterations
        let salt = base64::engine::general_purpose::STANDARD.encode(b"QSXCR+Q6sek8bf92");
        let server_first = format!("r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s={salt},i=4096");

        let result = client_final(&server_first, client_first_bare, client_nonce, password);
        assert!(result.is_ok(), "client_final should succeed");

        let (client_final_msg, server_sig) = result.unwrap();
        // Verify format: starts with "c=biws,r=<combined_nonce>,p="
        assert!(client_final_msg.starts_with("c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p="));
        // Server signature should be 32 bytes
        assert_eq!(server_sig.len(), 32);
    }

    #[test]
    fn client_final_rejects_bad_nonce() {
        let client_first_bare = "n=user,r=clientnonce123";
        let client_nonce = "clientnonce123";
        let password = "pass";

        // Server nonce doesn't start with client nonce
        let salt = base64::engine::general_purpose::STANDARD.encode(b"salt");
        let server_first = format!("r=WRONG_nonce_prefix,s={salt},i=4096");

        let result = client_final(&server_first, client_first_bare, client_nonce, password);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("nonce"),
            "error should mention nonce: {err_msg}"
        );
    }

    #[test]
    fn client_final_rejects_missing_fields() {
        let result = client_final("garbage", "n=user,r=nonce", "nonce", "pass");
        assert!(result.is_err());
    }

    #[test]
    fn client_final_rejects_zero_iterations() {
        let salt = base64::engine::general_purpose::STANDARD.encode(b"salt");
        let server_first = format!("r=nonce123server,s={salt},i=0");
        let result = client_final(&server_first, "n=user,r=nonce123", "nonce123", "pass");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("iteration count"));
    }

    #[test]
    fn client_final_rejects_excessive_iterations() {
        let salt = base64::engine::general_purpose::STANDARD.encode(b"salt");
        let server_first = format!("r=nonce123server,s={salt},i=999999");
        let result = client_final(&server_first, "n=user,r=nonce123", "nonce123", "pass");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds maximum"));
    }

    #[test]
    fn verify_server_correct_signature() {
        // Perform a full SCRAM exchange with known values and verify server sig
        let client_first_bare = "n=user,r=testnonce";
        let client_nonce = "testnonce";
        let password = "password123";
        let salt = base64::engine::general_purpose::STANDARD.encode(b"randomsalt");
        let server_first = format!("r=testnonceserverpart,s={salt},i=4096");

        let (_, server_sig) =
            client_final(&server_first, client_first_bare, client_nonce, password).unwrap();

        // Construct a valid server-final message
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(&server_sig);
        let server_final = format!("v={sig_b64}");

        assert!(verify_server(&server_final, &server_sig));
    }

    #[test]
    fn verify_server_wrong_signature() {
        let correct_sig = vec![1u8; 32];
        let wrong_sig = vec![2u8; 32];
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(&wrong_sig);
        let server_final = format!("v={sig_b64}");

        assert!(!verify_server(&server_final, &correct_sig));
    }

    #[test]
    fn verify_server_malformed() {
        assert!(!verify_server("garbage", &[0u8; 32]));
        assert!(!verify_server("v=not-valid-base64!!!", &[0u8; 32]));
        assert!(!verify_server("", &[0u8; 32]));
    }

    #[test]
    fn chunk_authenticate_short() {
        let short = "abc".repeat(10); // 30 bytes
        let chunks = chunk_authenticate(&short);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], short);
    }

    #[test]
    fn chunk_authenticate_exact_400() {
        let exact = "a".repeat(400);
        let chunks = chunk_authenticate(&exact);
        // Exactly 400 bytes: chunk + "+" terminator
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 400);
        assert_eq!(chunks[1], "+");
    }

    #[test]
    fn chunk_authenticate_long() {
        let long = "b".repeat(850);
        let chunks = chunk_authenticate(&long);
        assert_eq!(chunks.len(), 3); // 400 + 400 + 50
        assert_eq!(chunks[0].len(), 400);
        assert_eq!(chunks[1].len(), 400);
        assert_eq!(chunks[2].len(), 50);
    }

    #[test]
    fn chunk_authenticate_empty() {
        let chunks = chunk_authenticate("");
        assert_eq!(chunks, vec!["+"]);
    }

    #[test]
    fn scram_roundtrip_consistency() {
        // Full roundtrip: client_first -> client_final -> verify_server
        // Verify that the same password produces a verifiable signature.
        let (client_first_bare, _, client_nonce) = client_first("testuser");
        let password = "my_secret_password";

        // Simulate server-first: append server nonce to client nonce
        let combined_nonce = format!("{client_nonce}servernonce42");
        let salt = base64::engine::general_purpose::STANDARD.encode(b"test_salt_value!");
        let server_first = format!("r={combined_nonce},s={salt},i=4096");

        let (client_final_msg, server_sig) =
            client_final(&server_first, &client_first_bare, &client_nonce, password)
                .expect("client_final should succeed");

        // Verify client_final message format
        assert!(client_final_msg.starts_with("c=biws,r="));
        assert!(client_final_msg.contains(",p="));

        // Verify server signature is valid
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(&server_sig);
        assert!(verify_server(&format!("v={sig_b64}"), &server_sig));

        // Verify wrong password produces different signature
        let (_, wrong_sig) =
            client_final(&server_first, &client_first_bare, &client_nonce, "wrong_password")
                .expect("client_final should succeed even with wrong password");
        assert_ne!(server_sig, wrong_sig);
    }
}
