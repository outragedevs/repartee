use std::time::Duration;

use reqwest::{Client, StatusCode, Url, header};

pub const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;

pub enum Credentials {
    Basic { username: String, password: String },
    Bearer(String),
}

impl Credentials {
    pub fn for_bouncer(config: &crate::config::ServerConfig, provider: Option<crate::irc::bouncer::Provider>) -> Result<Self, Error> {
        if provider == Some(crate::irc::bouncer::Provider::Lurker)
            && [&config.sasl_user, &config.sasl_pass, &config.sasl_mechanism,
                &config.sasl_key_path, &config.client_cert_path].iter().all(|value| value.is_none())
            && let Some((login, secret)) = config.password.as_deref().and_then(|pass| pass.split_once(':'))
        {
            let mut normalized = config.clone();
            if !login.is_empty() {
                normalized.username = Some(login.split(['/', '@']).next().unwrap_or_default().to_string());
            }
            normalized.password = Some(secret.to_string());
            return Self::from_config(&normalized);
        }
        Self::from_config(config)
    }

    pub fn from_config(config: &crate::config::ServerConfig) -> Result<Self, Error> {
        let mechanism = config.sasl_mechanism.as_deref().map(str::to_ascii_uppercase);
        if mechanism.as_deref() == Some("OAUTHBEARER") {
            return config.sasl_pass.as_ref().filter(|value| !value.is_empty())
                .map(|value| Self::Bearer(value.clone())).ok_or(Error::Credentials);
        }
        if mechanism.as_deref().is_some_and(|value|
            !matches!(value, "PLAIN" | "SCRAM-SHA-1" | "SCRAM-SHA-256" | "SCRAM-SHA-512"))
            || (mechanism.is_none() && (config.client_cert_path.is_some() || config.sasl_key_path.is_some()))
        {
            return Err(Error::Credentials);
        }
        let (username, password) = match (&config.sasl_user, &config.sasl_pass) {
            (Some(username), Some(password)) => (username, password),
            _ if mechanism.is_none() => (config.username.as_ref().ok_or(Error::Credentials)?,
                config.password.as_ref().ok_or(Error::Credentials)?),
            _ => return Err(Error::Credentials),
        };
        if username.is_empty() || username.contains(':') || username.chars().any(char::is_control)
            || password.is_empty()
        {
            return Err(Error::Credentials);
        }
        Ok(Self::Basic { username: username.clone(), password: password.clone() })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("No supported bouncer upload credentials are available")]
    Credentials,
    #[error("Invalid or unsupported upload URL")]
    Url,
    #[error("Encrypted IRC connections require HTTPS uploads")]
    Downgrade,
    #[error("File must contain between 1 byte and 64 MiB")]
    Size,
    #[error("Invalid filename or content type")]
    Metadata,
    #[error("The upload service does not accept this content type")]
    UnsupportedType,
    #[error("Upload connection failed or timed out")]
    Transport,
    #[error("Upload service returned HTTP {status}")]
    Http { status: u16 },
    #[error("Upload service did not return a valid file location")]
    Location,
}

pub struct Filehost {
    endpoint: Url,
    encrypted: bool,
    client: Client,
}

impl Filehost {
    pub fn new(endpoint: &str, encrypted: bool) -> Result<Self, Error> {
        let endpoint = validate_url(endpoint, encrypted)?;
        let builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_mins(2));
        #[cfg(test)]
        let builder = fixture_trust(builder)?;
        let client = builder.build()
            .map_err(|_| Error::Transport)?;
        Ok(Self { endpoint, encrypted, client })
    }

    pub async fn upload(
        &self,
        credentials: &Credentials,
        filename: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<String, Error> {
        if body.is_empty() || body.len() > MAX_UPLOAD_BYTES {
            return Err(Error::Size);
        }
        let disposition = disposition(filename)?;
        if !valid_mime(content_type) {
            return Err(Error::Metadata);
        }
        let options = self.client.request(reqwest::Method::OPTIONS, self.endpoint.clone())
            .send().await.map_err(|_| Error::Transport)?;
        if !options.status().is_success() {
            return Err(Error::Http { status: options.status().as_u16() });
        }
        let accepted = options.headers().get_all("Accept-Post").iter()
            .map(|value| value.to_str().map_err(|_| Error::UnsupportedType))
            .collect::<Result<Vec<_>, _>>()?;
        if !accepted.is_empty() && !accepted.iter().any(|value| accepts(value, content_type)) {
            return Err(Error::UnsupportedType);
        }
        let request = self.client.post(self.endpoint.clone())
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_DISPOSITION, disposition)
            .header(header::CONTENT_LENGTH, body.len())
            .body(body);
        let request = match credentials {
            Credentials::Basic { username, password } => request.basic_auth(username, Some(password)),
            Credentials::Bearer(token) => request.bearer_auth(token),
        };
        let response = request.send().await.map_err(|_| Error::Transport)?;
        if response.status() != StatusCode::CREATED {
            return Err(Error::Http { status: response.status().as_u16() });
        }
        let location = response.headers().get(header::LOCATION)
            .and_then(|value| value.to_str().ok()).filter(|value| !value.trim().is_empty())
            .ok_or(Error::Location)?;
        let resolved = self.endpoint.join(location).map_err(|_| Error::Location)?;
        validate_url(resolved.as_str(), self.encrypted).map_err(|_| Error::Location)
            .map(|url| url.to_string())
    }
}

#[cfg(test)]
pub fn fixture_trust(builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder, Error> {
    if let Ok(path) = std::env::var("REPARTEE_FILEHOST_TEST_CA") {
        let pem = std::fs::read(path).map_err(|_| Error::Transport)?;
        let cert = reqwest::Certificate::from_pem(&pem).map_err(|_| Error::Transport)?;
        Ok(builder.tls_certs_merge([cert]))
    } else { Ok(builder) }
}

fn validate_url(value: &str, encrypted: bool) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|_| Error::Url)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some()
    {
        return Err(Error::Url);
    }
    if encrypted && url.scheme() != "https" {
        return Err(Error::Downgrade);
    }
    Ok(url)
}

fn valid_mime(value: &str) -> bool {
    value.split_once('/').is_some_and(|(kind, subtype)| {
        <[&str; 2]>::from((kind, subtype)).iter().all(|part| !part.is_empty() && part.bytes().all(|byte|
            byte.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&byte)))
    })
}

fn accepts(ranges: &str, mime: &str) -> bool {
    ranges.split(',').any(|range| {
        let range = range.split(';').next().unwrap_or_default().trim();
        range == "*/*" || range.eq_ignore_ascii_case(mime)
            || range.strip_suffix("/*").is_some_and(|kind|
                mime.split_once('/').is_some_and(|(actual, _)| actual.eq_ignore_ascii_case(kind)))
    })
}

fn disposition(filename: &str) -> Result<String, Error> {
    use std::fmt::Write;
    let filename = filename.rsplit(['/', '\\']).next().filter(|name|
        !name.is_empty() && name.len() <= 255 && !name.chars().any(char::is_control))
        .ok_or(Error::Metadata)?;
    let mut encoded = String::new();
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").map_err(|_| Error::Metadata)?;
        }
    }
    Ok(format!("attachment; filename*=UTF-8''{encoded}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_rejects_credentials_and_tls_downgrades() {
        assert!(matches!(Filehost::new("http://example.org/upload", true), Err(Error::Downgrade)));
        for url in ["ftp://example.org/upload", "https://user:pass@example.org/upload", "https://example.org/#secret"] {
            assert!(matches!(Filehost::new(url, false), Err(Error::Url)));
        }
    }

    #[test]
    fn filename_encoding_and_mime_ranges() {
        assert_eq!(disposition("/tmp/zażółć.png").unwrap(), "attachment; filename*=UTF-8''za%C5%BC%C3%B3%C5%82%C4%87.png");
        assert_eq!(disposition("file\r\nname"), Err(Error::Metadata));
        assert!(accepts("video/*, image/*", "image/png"));
        assert!(!accepts("image/jpeg", "image/png"));
        assert!(!valid_mime("image/png\r\nInjected: yes"));
    }
    async fn service(status: StatusCode, location: &'static str) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>, tokio::task::JoinHandle<()>) {
        use axum::{Router, routing::options, http::HeaderMap, body::Bytes};
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        let posts = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&posts);
        let app = Router::new().route("/upload", options(|headers: HeaderMap| async move {
            assert!(!headers.contains_key(header::AUTHORIZATION));
            (StatusCode::NO_CONTENT, [("Accept-Post", "image/*")])
        }).post(move |headers: HeaderMap, body: Bytes| {
            let count = Arc::clone(&count);
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                assert_eq!(headers[header::AUTHORIZATION], "Bearer disposable-test-credential");
                assert_eq!(headers[header::CONTENT_TYPE], "image/png");
                assert_eq!(body.as_ref(), b"image bytes");
                (status, [(header::LOCATION, location)])
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        (format!("http://{address}/upload"), posts, task)
    }

    #[tokio::test]
    async fn uploads_raw_bytes_and_resolves_relative_location() {
        let (endpoint, count, task) = service(StatusCode::CREATED, "/files/result.png").await;
        let host = Filehost::new(&endpoint, false).unwrap();
        let credentials = Credentials::Bearer("disposable-test-credential".into());
        let result = host.upload(&credentials, "photo.png", "image/png", b"image bytes".to_vec()).await.unwrap();
        assert_eq!(result, endpoint.replace("/upload", "/files/result.png"));
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
        task.abort();
    }

    #[tokio::test]
    async fn refuses_post_redirects_instead_of_resending_credentials() {
        let (endpoint, count, task) = service(StatusCode::TEMPORARY_REDIRECT, "/upload").await;
        let host = Filehost::new(&endpoint, false).unwrap();
        let result = host.upload(&Credentials::Bearer("disposable-test-credential".into()),
            "photo.png", "image/png", b"image bytes".to_vec()).await;
        assert_eq!(result, Err(Error::Http { status: 307 }));
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
        task.abort();
    }

    #[tokio::test]
    async fn mime_refusal_stops_before_authenticated_post() {
        let (endpoint, count, task) = service(StatusCode::CREATED, "/files/result.png").await;
        let host = Filehost::new(&endpoint, false).unwrap();
        let result = host.upload(&Credentials::Basic { username: "test".into(), password: "test".into() },
            "file.txt", "text/plain", b"text".to_vec()).await;
        assert_eq!(result, Err(Error::UnsupportedType));
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
        task.abort();
    }

    fn config() -> crate::config::ServerConfig {
        toml::from_str("label='test'\naddress='localhost'\nport=1\ntls=false\nchannels=[]\nusername='account@client/network'").unwrap()
    }

    #[test]
    fn pass_credentials_follow_the_confirmed_provider() {
        use crate::irc::bouncer::Provider;
        let mut config = config();
        config.password = Some("alice/network@client:secret:with:colons".into());
        assert!(matches!(Credentials::for_bouncer(&config, Some(Provider::Lurker)),
            Ok(Credentials::Basic { username, password }) if username == "alice" && password == "secret:with:colons"));
        for provider in [Some(Provider::Soju), None] {
            assert!(matches!(Credentials::for_bouncer(&config, provider),
                Ok(Credentials::Basic { username, password }) if username == "account@client/network" && password == "alice/network@client:secret:with:colons"));
        }
        config.sasl_user = Some("sasl-account".into());
        config.sasl_pass = Some("sasl-secret".into());
        assert!(matches!(Credentials::for_bouncer(&config, Some(Provider::Lurker)),
            Ok(Credentials::Basic { username, password }) if username == "sasl-account" && password == "sasl-secret"));
    }

    #[test]
    fn credentials_use_sasl_account_before_irc_username_and_pass() {
        let mut config = config();
        config.sasl_user = Some("sasl-account".into());
        config.sasl_pass = Some("sasl-secret".into());
        config.password = Some("different-pass".into());
        match Credentials::from_config(&config).unwrap() {
            Credentials::Basic { username, password } => {
                assert_eq!(username, "sasl-account");
                assert_eq!(password, "sasl-secret");
            }
            Credentials::Bearer(_) => panic!("expected Basic"),
        }
        config.sasl_user = None;
        config.sasl_pass = None;
        assert!(matches!(Credentials::from_config(&config).unwrap(), Credentials::Basic { username, password }
            if username == "account@client/network" && password == "different-pass"));
    }

    #[test]
    fn certificate_auth_never_falls_back_to_unrelated_stored_passwords() {
        let mut config = config();
        config.sasl_user = Some("account".into());
        config.sasl_pass = Some("unused-password".into());
        config.sasl_mechanism = Some("EXTERNAL".into());
        assert!(matches!(Credentials::from_config(&config), Err(Error::Credentials)));
        config.sasl_mechanism = None;
        config.client_cert_path = Some("client.pem".into());
        assert!(matches!(Credentials::from_config(&config), Err(Error::Credentials)));
        config.sasl_mechanism = Some("PLAIN".into());
        assert!(matches!(Credentials::from_config(&config), Ok(Credentials::Basic { .. })));
    }

    #[test]
    fn oauth_uploads_use_bearer_and_missing_secrets_are_rejected() {
        let mut config = config();
        config.sasl_mechanism = Some("oauthbearer".into());
        assert!(matches!(Credentials::from_config(&config), Err(Error::Credentials)));
        config.sasl_pass = Some("disposable-test-token".into());
        assert!(matches!(Credentials::from_config(&config), Ok(Credentials::Bearer(value))
            if value == "disposable-test-token"));
    }

}
