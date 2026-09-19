use color_eyre::eyre::{Result, WrapErr as _, eyre};
use irc::client::data::Config;
use tokio_rustls::rustls;
use zeroize::Zeroizing;

pub(super) fn configure(config: &mut Config, configured: Option<&str>) -> Result<()> {
    let Some(configured) = configured else {
        return Ok(());
    };
    if !config.use_tls() {
        return Err(eyre!(
            "client_cert_path requires TLS; enable tls for this server"
        ));
    }
    if configured.trim().is_empty() {
        return Err(eyre!(
            "client_cert_path must name a PEM certificate and private key file"
        ));
    }
    let path = super::sasl_ecdsa::resolve_key_path(configured);
    let pem = Zeroizing::new(std::fs::read_to_string(&path).wrap_err_with(|| {
        format!("cannot read client certificate {}: expected a UTF-8 PEM file containing the certificate chain and unencrypted private key", path.display())
    })?);
    let certs = rustls_pemfile::certs(&mut pem.as_bytes())
        .collect::<std::io::Result<Vec<_>>>()
        .wrap_err_with(|| format!("invalid PEM certificate chain in {}", path.display()))?;
    if certs.is_empty() {
        return Err(eyre!("no CERTIFICATE block in {}", path.display()));
    }
    let key = rustls_pemfile::private_key(&mut pem.as_bytes())
        .wrap_err_with(|| format!("invalid PEM private key in {}", path.display()))?
        .ok_or_else(|| eyre!("no unencrypted private key in {}; include a PKCS#8 PRIVATE KEY, RSA PRIVATE KEY or EC PRIVATE KEY block in the same PEM file", path.display()))?;
    rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(rustls::RootCertStore::empty())
    .with_client_auth_cert(certs, key)
    .wrap_err_with(|| {
        format!(
            "invalid or mismatched client certificate and private key in {}",
            path.display()
        )
    })?;
    config.client_cert_path = Some(
        path.to_str()
            .ok_or_else(|| eyre!("client certificate path must be valid UTF-8"))?
            .to_owned(),
    );
    config.client_cert_pass = Some(pem.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert.pem(), key.serialize_pem())
    }

    #[test]
    fn absolute_identity_supplies_both_tls_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.pem");
        let (cert, key) = identity();
        let pem = format!("{cert}{key}");
        std::fs::write(&path, &pem).unwrap();
        let mut config = Config::default();
        configure(&mut config, path.to_str()).unwrap();
        assert_eq!(config.client_cert_path.as_deref(), path.to_str());
        assert_eq!(config.client_cert_pass.as_deref(), Some(pem.as_str()));
    }

    #[tokio::test]
    async fn tls_connection_presents_the_configured_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.pem");
        let (cert, key) = identity();
        std::fs::write(&path, format!("{cert}{key}")).unwrap();
        let chain = rustls_pemfile::certs(&mut cert.as_bytes())
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        let private_key = rustls_pemfile::private_key(&mut key.as_bytes())
            .unwrap()
            .unwrap();
        let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        roots.add(chain[0].clone()).unwrap();
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            std::sync::Arc::new(roots),
            provider.clone(),
        )
        .build()
        .unwrap();
        let server = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain.clone(), private_key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let stream = acceptor.accept(stream).await.unwrap();
            assert_eq!(stream.get_ref().1.peer_certificates().unwrap(), chain);
        });
        let mut config = Config {
            server: Some("localhost".into()),
            port: Some(port),
            nickname: Some("test".into()),
            use_tls: Some(true),
            dangerously_accept_invalid_certs: Some(true),
            ..Config::default()
        };
        configure(&mut config, path.to_str()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let client = irc::client::Client::from_config(config).await.unwrap();
            accepted.await.unwrap();
            drop(client);
        })
        .await
        .unwrap();
    }

    #[test]
    fn relative_path_resolves_against_certificates_directory() {
        let configured = "missing-test-identity.pem";
        let error = configure(&mut Config::default(), Some(configured)).unwrap_err();
        assert!(
            error.to_string().contains(
                super::super::sasl_ecdsa::resolve_key_path(configured)
                    .to_str()
                    .unwrap()
            )
        );
        assert_eq!(
            super::super::sasl_ecdsa::resolve_key_path(configured),
            crate::constants::certs_dir().join(configured)
        );
    }

    #[test]
    fn invalid_material_is_rejected_without_echoing_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.pem");
        let (cert, key) = identity();
        let (_, other_key) = identity();
        for pem in [
            "not a PEM identity".to_owned(),
            cert.clone(),
            key,
            format!("{cert}{other_key}"),
            "-----BEGIN CERTIFICATE-----\ninvalid!\n-----END CERTIFICATE-----".to_owned(),
        ] {
            std::fs::write(&path, &pem).unwrap();
            let error = configure(&mut Config::default(), path.to_str()).unwrap_err();
            assert!(!error.to_string().contains(&pem));
        }
    }

    #[test]
    fn optional_identity_and_plaintext_guard() {
        let mut config = Config::default();
        configure(&mut config, None).unwrap();
        assert!(config.client_cert_pass.is_none());
        assert!(configure(&mut config, Some("")).is_err());
        config.use_tls = Some(false);
        assert!(
            configure(&mut config, Some("identity.pem"))
                .unwrap_err()
                .to_string()
                .contains("requires TLS")
        );
    }
}
