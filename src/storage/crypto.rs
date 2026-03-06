use std::io::{BufRead, Write};
use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::{Engine, engine::general_purpose::STANDARD};

use crate::constants::APP_NAME;

/// Key name stored in the .env file (`APP_NAME` uppercased + `_LOG_KEY`).
fn env_key_name() -> String {
    format!("{}_LOG_KEY", APP_NAME.to_uppercase())
}

/// Encrypted payload: base64-encoded ciphertext and the 12-byte IV used.
pub struct EncryptedData {
    pub ciphertext: String,
    pub iv: Vec<u8>,
}

/// Generate a random 256-bit key and return it as a 64-character hex string.
pub fn generate_key_hex() -> String {
    let mut key_bytes = [0u8; 32];
    rand::fill(&mut key_bytes);
    hex::encode(key_bytes)
}

/// Decode a 64-character hex string into an AES-256-GCM key.
pub fn import_key(hex_key: &str) -> Result<Key<Aes256Gcm>, String> {
    let bytes = hex::decode(hex_key).map_err(|e| format!("invalid hex key: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("key must be 32 bytes, got {}", bytes.len()));
    }
    Ok(*Key::<Aes256Gcm>::from_slice(&bytes))
}

/// Encrypt `plaintext` with AES-256-GCM using a random 12-byte IV.
pub fn encrypt(plaintext: &str, key: &Key<Aes256Gcm>) -> Result<EncryptedData, String> {
    let cipher = Aes256Gcm::new(key);

    let mut iv_bytes = [0u8; 12];
    rand::fill(&mut iv_bytes);
    let nonce = Nonce::from_slice(&iv_bytes);

    let ciphertext_bytes = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| format!("encryption failed: {e}"))?;

    Ok(EncryptedData {
        ciphertext: STANDARD.encode(&ciphertext_bytes),
        iv: iv_bytes.to_vec(),
    })
}

/// Decrypt a base64-encoded ciphertext using the given 12-byte IV and key.
pub fn decrypt(ciphertext_b64: &str, iv: &[u8], key: &Key<Aes256Gcm>) -> Result<String, String> {
    let cipher = Aes256Gcm::new(key);

    let ciphertext_bytes = STANDARD
        .decode(ciphertext_b64)
        .map_err(|e| format!("invalid base64: {e}"))?;

    let nonce = Nonce::from_slice(iv);

    let plaintext_bytes = cipher
        .decrypt(nonce, ciphertext_bytes.as_ref())
        .map_err(|e| format!("decryption failed: {e}"))?;

    String::from_utf8(plaintext_bytes).map_err(|e| format!("invalid UTF-8: {e}"))
}

/// Load or create the encryption key from the default .env path.
pub fn load_or_create_key() -> Result<String, String> {
    let path = crate::constants::env_path();
    load_or_create_key_at(&path)
}

/// Load the encryption key from `path`, or generate one and append it.
///
/// The .env file is expected to contain lines like `KEY=value`.
/// If the key line is missing, a new key is generated and appended.
/// On Unix, the file is chmod 0o600.
pub fn load_or_create_key_at(path: &Path) -> Result<String, String> {
    let key_name = env_key_name();

    // Try to read existing key from file
    if path.exists() {
        let file =
            std::fs::File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
        let reader = std::io::BufReader::new(file);
        for line in reader.lines() {
            let line = line.map_err(|e| format!("failed to read line: {e}"))?;
            let trimmed = line.trim();
            if let Some(value) = trimmed.strip_prefix(&format!("{key_name}=")) {
                let value = value.trim();
                if !value.is_empty() {
                    return Ok(value.to_string());
                }
            }
        }
    }

    // Key not found — generate and append
    let new_key = generate_key_hex();

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory {}: {e}", parent.display()))?;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("failed to open {}: {e}", path.display()))?;

    writeln!(file, "{key_name}={new_key}")
        .map_err(|e| format!("failed to write key: {e}"))?;

    // Set file permissions to 0600 on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)
            .map_err(|e| format!("failed to set permissions: {e}"))?;
    }

    Ok(new_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_key_is_32_bytes_hex() {
        let key = generate_key_hex();
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key_hex = generate_key_hex();
        let key = import_key(&key_hex).unwrap();
        let plaintext = "Hello, repartee!";

        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted.ciphertext, &encrypted.iv, &key).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn different_ivs_produce_different_ciphertext() {
        let key_hex = generate_key_hex();
        let key = import_key(&key_hex).unwrap();
        let plaintext = "same input twice";

        let enc1 = encrypt(plaintext, &key).unwrap();
        let enc2 = encrypt(plaintext, &key).unwrap();

        // IVs should differ (random)
        assert_ne!(enc1.iv, enc2.iv);
        // Ciphertext should differ due to different IVs
        assert_ne!(enc1.ciphertext, enc2.ciphertext);
    }

    #[test]
    fn wrong_key_fails_decrypt() {
        let key1 = import_key(&generate_key_hex()).unwrap();
        let key2 = import_key(&generate_key_hex()).unwrap();
        let plaintext = "secret message";

        let encrypted = encrypt(plaintext, &key1).unwrap();
        let result = decrypt(&encrypted.ciphertext, &encrypted.iv, &key2);

        assert!(result.is_err());
    }

    #[test]
    fn load_or_create_key_roundtrip() {
        let dir = std::env::temp_dir().join(format!("repartee_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let env_file = dir.join(".env");

        // First call creates the key
        let key1 = load_or_create_key_at(&env_file).unwrap();
        assert_eq!(key1.len(), 64);

        // Second call returns the same key
        let key2 = load_or_create_key_at(&env_file).unwrap();
        assert_eq!(key1, key2);

        // File contains the key
        let contents = std::fs::read_to_string(&env_file).unwrap();
        assert!(contents.contains(&key1));

        // Cleanup
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
