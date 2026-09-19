//! SASL SCRAM client implementation (RFC 5802 / RFC 7677).
//!
//! Implements the client side of SCRAM (Salted Challenge Response
//! Authentication Mechanism) — a challenge-response mechanism that never puts
//! the password on the wire — over any of the three hashes IRC servers
//! advertise: SHA-1, SHA-256 and SHA-512.
//!
//! The three differ only in the width of the derived keys; the message format
//! is identical, so the protocol code below is written once and the hash is
//! chosen by [`ScramHash`].
//!
//! The `-PLUS` variants (channel binding) are deliberately **not** supported —
//! see [`ScramHash::from_mechanism`].

use base64::Engine as _;
use color_eyre::eyre::{Result, eyre};
use hmac::{Mac, SimpleHmac};
use rand::RngExt;
use sha1::Sha1;
use sha2::digest::core_api::BlockSizeUser;
use sha2::{Digest, Sha256, Sha512};

/// Maximum allowed PBKDF2 iteration count to prevent denial-of-service via absurdly
/// high server-requested iterations.
const MAX_ITERATIONS: u32 = 100_000;

/// The hash a SCRAM exchange is built on.
///
/// This is the only thing that varies between `SCRAM-SHA-1`, `SCRAM-SHA-256`
/// and `SCRAM-SHA-512`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScramHash {
    /// SHA-1 — RFC 5802. Still offered by older services; preferred over
    /// `PLAIN` because it does not reveal the password.
    Sha1,
    /// SHA-256 — RFC 7677.
    Sha256,
    /// SHA-512 — `draft-melnikov-scram-sha-512`, registered with IANA.
    Sha512,
}

impl ScramHash {
    /// The SASL mechanism name, exactly as it appears in the `sasl` capability.
    ///
    /// Parsing goes the other way through `SaslMechanism::from_name`, which
    /// owns the whole mechanism namespace — including the rule that
    /// `SCRAM-SHA-256-PLUS` resolves to nothing, because we do not implement
    /// channel binding and must not answer a `-PLUS` offer with a plain
    /// exchange.
    #[must_use]
    pub const fn mechanism(self) -> &'static str {
        match self {
            Self::Sha1 => "SCRAM-SHA-1",
            Self::Sha256 => "SCRAM-SHA-256",
            Self::Sha512 => "SCRAM-SHA-512",
        }
    }
}

impl std::fmt::Display for ScramHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.mechanism())
    }
}

/// Apply `SASLprep` (RFC 4013) to a credential.
///
/// Required by RFC 5802 §5.1 and RFC 4616 so that both ends derive the same
/// bytes from the same password. It is the identity for printable ASCII, which
/// is what nearly every credential is.
///
/// On prohibited input the **input is returned unchanged**, with a warning that
/// names neither the value nor its length. Failing the exchange instead would
/// break a credential that authenticates today against the many servers that
/// do not prep either — a regression traded for a purity we cannot enforce
/// from this side of the connection.
#[must_use]
pub fn saslprep(value: &str) -> String {
    stringprep::saslprep(value).map_or_else(
        |_| {
            tracing::warn!("SASL: credential is not valid SASLprep input — sending it unprepared");
            value.to_string()
        },
        std::borrow::Cow::into_owned,
    )
}

/// Generate the client-first message.
///
/// Returns `(client_first_bare, full_client_first_message, client_nonce)`.
///
/// - `client_first_bare` = `n=<username>,r=<client_nonce>`
/// - `full_message` = `n,,` + `client_first_bare`  (gs2-header + bare)
/// - `client_nonce` = 24 random bytes, base64-encoded
///
/// Hash-independent: the client-first message is identical for every
/// [`ScramHash`].
#[must_use]
pub fn client_first(username: &str) -> (String, String, String) {
    let mut nonce_bytes = [0u8; 24];
    rand::rng().fill(&mut nonce_bytes);
    let client_nonce = base64::engine::general_purpose::STANDARD.encode(nonce_bytes);

    // RFC 5802: SASLprep the username, then escape '=' and ',' — in that order,
    // so prepping cannot reintroduce an unescaped separator.
    let safe_username = saslprep(username)
        .replace('=', "=3D")
        .replace(',', "=2C");

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
    hash: ScramHash,
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

    let combined_nonce =
        combined_nonce.ok_or_else(|| eyre!("SCRAM: server-first missing nonce (r=)"))?;
    let salt_b64 = salt_b64.ok_or_else(|| eyre!("SCRAM: server-first missing salt (s=)"))?;
    let iterations_str =
        iterations.ok_or_else(|| eyre!("SCRAM: server-first missing iterations (i=)"))?;

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

    // RFC 5802 §5.1: the password is SASLprep'd before it is salted.
    let password = saslprep(password);

    Ok(match hash {
        ScramHash::Sha1 => compute_proof::<Sha1>(
            &password,
            &salt,
            iter_count,
            client_first_bare,
            server_first,
            combined_nonce,
        ),
        ScramHash::Sha256 => compute_proof::<Sha256>(
            &password,
            &salt,
            iter_count,
            client_first_bare,
            server_first,
            combined_nonce,
        ),
        ScramHash::Sha512 => compute_proof::<Sha512>(
            &password,
            &salt,
            iter_count,
            client_first_bare,
            server_first,
            combined_nonce,
        ),
    })
}

/// The SCRAM key derivation and proof, RFC 5802 §3, for one hash.
///
/// Returns `(client_final_message, expected_server_signature)`. Infallible:
/// every input has already been validated by [`client_final`], and HMAC accepts
/// keys of any length.
fn compute_proof<D>(
    password: &str,
    salt: &[u8],
    iterations: u32,
    client_first_bare: &str,
    server_first: &str,
    combined_nonce: &str,
) -> (String, Vec<u8>)
where
    D: Digest + BlockSizeUser + Clone + Sync,
{
    // salted_password = PBKDF2(HMAC, password, salt, iterations), one hash wide
    let mut salted_password = vec![0u8; <D as Digest>::output_size()];
    pbkdf2::pbkdf2::<SimpleHmac<D>>(password.as_bytes(), salt, iterations, &mut salted_password)
        .expect("HMAC accepts any key length");

    // client_key = HMAC(salted_password, "Client Key")
    let client_key = mac::<D>(&salted_password, b"Client Key");

    // stored_key = H(client_key)
    let stored_key = <D as Digest>::digest(&client_key);

    // client_final_without_proof = "c=biws,r=" + combined_nonce
    //   "biws" = base64("n,,") — the gs2-header used in client-first
    let client_final_without_proof = format!("c=biws,r={combined_nonce}");

    // auth_message = client_first_bare + "," + server_first + "," + client_final_without_proof
    let auth_message = format!("{client_first_bare},{server_first},{client_final_without_proof}");

    // client_signature = HMAC(stored_key, auth_message)
    let client_signature = mac::<D>(&stored_key, auth_message.as_bytes());

    // client_proof = client_key XOR client_signature
    let client_proof: Vec<u8> = client_key
        .iter()
        .zip(client_signature.iter())
        .map(|(a, b)| a ^ b)
        .collect();

    let proof_b64 = base64::engine::general_purpose::STANDARD.encode(&client_proof);

    // server_key = HMAC(salted_password, "Server Key")
    let server_key = mac::<D>(&salted_password, b"Server Key");

    // server_signature = HMAC(server_key, auth_message)
    let server_signature = mac::<D>(&server_key, auth_message.as_bytes());

    (
        format!("{client_final_without_proof},p={proof_b64}"),
        server_signature,
    )
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

/// Compute `HMAC-D(key, data)`.
///
/// [`SimpleHmac`] rather than [`hmac::Hmac`]: it is bounded by
/// `D: Digest + BlockSizeUser`, where `Hmac` needs the whole `CoreProxy` chain,
/// and the difference in cost is a few hundred bytes of stack per call on a
/// path that runs six times per connection.
fn mac<D>(key: &[u8], data: &[u8]) -> Vec<u8>
where
    D: Digest + BlockSizeUser + Clone + Sync,
{
    let mut mac = SimpleHmac::<D>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
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

    /// RFC 5802 §5 — the published SCRAM-SHA-1 exchange, verbatim.
    ///
    /// This is the test that distinguishes "computes SCRAM" from "computes
    /// something self-consistent": the proof and the server signature below are
    /// the RFC's own bytes, not ours.
    #[test]
    fn rfc5802_sha1_vector() {
        let client_nonce = "fyko+d2lbbFgONRv9qkxdawL";
        let client_first_bare = "n=user,r=fyko+d2lbbFgONRv9qkxdawL";
        let server_first =
            "r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j,s=QSXCR+Q6sek8bf92,i=4096";

        let (client_final_msg, server_sig) = client_final(
            ScramHash::Sha1,
            server_first,
            client_first_bare,
            client_nonce,
            "pencil",
        )
        .expect("RFC 5802 vector must compute");

        assert_eq!(
            client_final_msg,
            "c=biws,r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j,\
             p=v0X8v3Bz2T0CJGbJQyF0X+HI4Ts="
        );
        assert!(verify_server("v=rmF9pqV8S7suAoZWja4dJRkFsKQ=", &server_sig));
    }

    /// RFC 7677 §3 — the published SCRAM-SHA-256 exchange, verbatim.
    #[test]
    fn rfc7677_sha256_vector() {
        let client_nonce = "rOprNGfwEbeRWgbNEkqO";
        let client_first_bare = "n=user,r=rOprNGfwEbeRWgbNEkqO";
        let server_first = "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,\
                            s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";

        let (client_final_msg, server_sig) = client_final(
            ScramHash::Sha256,
            server_first,
            client_first_bare,
            client_nonce,
            "pencil",
        )
        .expect("RFC 7677 vector must compute");

        assert_eq!(
            client_final_msg,
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,\
             p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );
        assert!(verify_server(
            "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=",
            &server_sig
        ));
    }

    /// SHA-512 has no published IRC-facing vector, so pin the two properties
    /// that would break if the hash were wired up wrong: the derived key width
    /// and the dependence on the password.
    #[test]
    fn sha512_widens_the_derived_keys() {
        let server_first = "r=nonce123server,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        let (_, sig) = client_final(
            ScramHash::Sha512,
            server_first,
            "n=user,r=nonce123",
            "nonce123",
            "pencil",
        )
        .unwrap();
        assert_eq!(sig.len(), 64, "SHA-512 server signature is 64 bytes");

        let (_, other) = client_final(
            ScramHash::Sha512,
            server_first,
            "n=user,r=nonce123",
            "nonce123",
            "not pencil",
        )
        .unwrap();
        assert_ne!(sig, other, "the signature must depend on the password");
    }

    /// Every hash, with the derived-key width the RFCs give it. Parsing these
    /// names back is `SaslMechanism::from_name`'s job and is tested there.
    const HASHES: [(ScramHash, &str, usize); 3] = [
        (ScramHash::Sha1, "SCRAM-SHA-1", 20),
        (ScramHash::Sha256, "SCRAM-SHA-256", 32),
        (ScramHash::Sha512, "SCRAM-SHA-512", 64),
    ];

    #[test]
    fn each_hash_has_its_own_mechanism_name() {
        for (hash, name, _) in HASHES {
            assert_eq!(hash.mechanism(), name);
            assert_eq!(hash.to_string(), name);
        }
    }

    /// `SASLprep` is identity for printable ASCII — the case every shipped
    /// credential is in — and normalises the cases RFC 4013 names.
    #[test]
    fn saslprep_leaves_ascii_alone_and_never_fails() {
        assert_eq!(saslprep("hunter2"), "hunter2");
        assert_eq!(saslprep("p@ss w0rd!"), "p@ss w0rd!");
        // Non-ASCII space (U+00A0) maps to SPACE; soft hyphen is dropped.
        assert_eq!(saslprep("a\u{00A0}b"), "a b");
        assert_eq!(saslprep("a\u{00AD}b"), "ab");
        // Prohibited input falls back to the raw value rather than failing the
        // whole exchange: a server that does not prep either would accept it.
        assert_eq!(saslprep("bad\u{0007}"), "bad\u{0007}");
    }

    #[test]
    fn client_first_format() {
        let (bare, full, nonce) = client_first("testuser");
        // Full message starts with gs2-header "n,,"
        assert!(
            full.starts_with("n,,"),
            "full message must start with 'n,,'"
        );
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
    fn client_final_rejects_bad_nonce() {
        let client_first_bare = "n=user,r=clientnonce123";
        let client_nonce = "clientnonce123";
        let password = "pass";

        // Server nonce doesn't start with client nonce
        let salt = base64::engine::general_purpose::STANDARD.encode(b"salt");
        let server_first = format!("r=WRONG_nonce_prefix,s={salt},i=4096");

        let result = client_final(
            ScramHash::Sha256,
            &server_first,
            client_first_bare,
            client_nonce,
            password,
        );
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("nonce"),
            "error should mention nonce: {err_msg}"
        );
    }

    #[test]
    fn client_final_rejects_missing_fields() {
        let result = client_final(
            ScramHash::Sha256,
            "garbage",
            "n=user,r=nonce",
            "nonce",
            "pass",
        );
        assert!(result.is_err());
    }

    #[test]
    fn client_final_rejects_zero_iterations() {
        let salt = base64::engine::general_purpose::STANDARD.encode(b"salt");
        let server_first = format!("r=nonce123server,s={salt},i=0");
        let result = client_final(
            ScramHash::Sha256,
            &server_first,
            "n=user,r=nonce123",
            "nonce123",
            "pass",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("iteration count"));
    }

    #[test]
    fn client_final_rejects_excessive_iterations() {
        let salt = base64::engine::general_purpose::STANDARD.encode(b"salt");
        let server_first = format!("r=nonce123server,s={salt},i=999999");
        let result = client_final(
            ScramHash::Sha256,
            &server_first,
            "n=user,r=nonce123",
            "nonce123",
            "pass",
        );
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

        let (_, server_sig) = client_final(
            ScramHash::Sha256,
            &server_first,
            client_first_bare,
            client_nonce,
            password,
        )
        .unwrap();

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
        // Full roundtrip: client_first -> client_final -> verify_server, for
        // every hash — the same password must produce a verifiable signature
        // and a wrong one must not.
        for (hash, _, output_len) in HASHES {
            let (client_first_bare, _, client_nonce) = client_first("testuser");
            let password = "my_secret_password";

            // Simulate server-first: append server nonce to client nonce
            let combined_nonce = format!("{client_nonce}servernonce42");
            let salt = base64::engine::general_purpose::STANDARD.encode(b"test_salt_value!");
            let server_first = format!("r={combined_nonce},s={salt},i=4096");

            let (client_final_msg, server_sig) = client_final(
                hash,
                &server_first,
                &client_first_bare,
                &client_nonce,
                password,
            )
            .expect("client_final should succeed");

            // Verify client_final message format
            assert!(client_final_msg.starts_with("c=biws,r="), "{hash}");
            assert!(client_final_msg.contains(",p="), "{hash}");
            assert_eq!(server_sig.len(), output_len, "{hash}");

            // Verify server signature is valid
            let sig_b64 = base64::engine::general_purpose::STANDARD.encode(&server_sig);
            assert!(verify_server(&format!("v={sig_b64}"), &server_sig), "{hash}");

            // Verify wrong password produces different signature
            let (_, wrong_sig) = client_final(
                hash,
                &server_first,
                &client_first_bare,
                &client_nonce,
                "wrong_password",
            )
            .expect("client_final should succeed even with wrong password");
            assert_ne!(server_sig, wrong_sig, "{hash}");
        }
    }
}
