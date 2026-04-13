use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::constants;

/// Load TLS config from user-provided cert/key paths, or generate a self-signed
/// certificate if both paths are empty.
pub fn load_or_generate_tls_config(cert_path: &str, key_path: &str) -> Result<Arc<ServerConfig>> {
    let (cert_file, key_file) = if cert_path.is_empty() && key_path.is_empty() {
        generate_self_signed_to(&constants::certs_dir())?
    } else {
        (PathBuf::from(cert_path), PathBuf::from(key_path))
    };

    let certs = load_certs(&cert_file)?;
    let key = load_private_key(&key_file)?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| eyre!("TLS config error: {e}"))?;

    Ok(Arc::new(config))
}

/// Generate a self-signed certificate and key in the given directory.
/// Returns `(cert_path, key_path)`.
pub fn generate_self_signed_to(dir: &Path) -> Result<(PathBuf, PathBuf)> {
    crate::fs_secure::create_dir_all(dir, 0o700)?;

    let cert_path = dir.join("self-signed.pem");
    let key_path = dir.join("self-signed-key.pem");

    // Skip regeneration if both files already exist.
    if cert_path.exists() && key_path.exists() {
        crate::fs_secure::restrict_path(&cert_path, 0o600)?;
        crate::fs_secure::restrict_path(&key_path, 0o600)?;
        tracing::info!("using existing self-signed cert at {}", cert_path.display());
        return Ok((cert_path, key_path));
    }

    let mut params =
        rcgen::CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])?;
    params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST,
        )));

    let key_pair = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    crate::fs_secure::write_file(&cert_path, cert.pem(), 0o600)?;
    crate::fs_secure::write_file(&key_path, key_pair.serialize_pem(), 0o600)?;

    tracing::info!("generated self-signed TLS cert at {}", cert_path.display());
    Ok((cert_path, key_path))
}

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)
        .map_err(|e| eyre!("failed to open cert {}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| eyre!("failed to parse cert PEM: {e}"))?;

    if certs.is_empty() {
        return Err(eyre!("no certificates found in {}", path.display()));
    }
    Ok(certs)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)
        .map_err(|e| eyre!("failed to open key {}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);

    rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| eyre!("no private key found in {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_self_signed_creates_valid_pem_files() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = generate_self_signed_to(dir.path()).unwrap();

        assert!(cert.exists());
        assert!(key.exists());

        let cert_pem = std::fs::read_to_string(&cert).unwrap();
        let key_pem = std::fs::read_to_string(&key).unwrap();
        assert!(cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
    }

    #[test]
    fn generate_self_signed_reuses_existing() {
        let dir = tempfile::tempdir().unwrap();
        let (cert1, _) = generate_self_signed_to(dir.path()).unwrap();
        let content1 = std::fs::read_to_string(&cert1).unwrap();

        // Second call should reuse, not regenerate.
        let (cert2, _) = generate_self_signed_to(dir.path()).unwrap();
        let content2 = std::fs::read_to_string(&cert2).unwrap();

        assert_eq!(content1, content2);
    }

    #[test]
    fn load_tls_config_from_generated_cert() {
        let dir = tempfile::tempdir().unwrap();
        generate_self_signed_to(dir.path()).unwrap();

        let cert_path = dir.path().join("self-signed.pem");
        let key_path = dir.path().join("self-signed-key.pem");

        let config =
            load_or_generate_tls_config(cert_path.to_str().unwrap(), key_path.to_str().unwrap());
        assert!(config.is_ok());
    }
}
