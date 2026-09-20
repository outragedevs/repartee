use base64::{Engine as _, engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD}};
use irc::proto::{Command, Message};
use serde::{Deserialize, Serialize};

pub const CAP: &str = "soju.im/webpush";

#[derive(Clone, Deserialize, Serialize)]
pub struct Subscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Subscription([redacted])")
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.endpoint.zeroize();
        self.auth.zeroize();
    }
}

fn decode(value: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value).or_else(|_| URL_SAFE.decode(value)).ok()
}

pub fn valid_public_key(value: &str) -> bool {
    decode(value).is_some_and(|bytes| bytes.len() == 65 && bytes[0] == 4
        && p256::PublicKey::from_sec1_bytes(&bytes).is_ok())
}

pub fn valid_endpoint(endpoint: &str) -> bool {
    !endpoint.is_empty() && endpoint.len() <= 2048
        && !endpoint.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        && reqwest::Url::parse(endpoint).is_ok_and(|url| url.scheme() == "https"
            && url.host_str().is_some() && url.username().is_empty()
            && url.password().is_none() && url.fragment().is_none())
}

fn request(args: Vec<String>) -> Result<Command, &'static str> {
    let command = Command::Raw("WEBPUSH".into(), args);
    let message: Message = command.clone().into();
    if message.to_string().len() > 512 { return Err("WebPush request exceeds the IRC line limit"); }
    Ok(command)
}

impl Subscription {
    pub fn register(&self) -> Result<Command, &'static str> {
        if !valid_endpoint(&self.endpoint) { return Err("Invalid WebPush endpoint"); }
        if !valid_public_key(&self.p256dh) || decode(&self.auth).is_none_or(|bytes| bytes.len() != 16) {
            return Err("Invalid WebPush subscription keys");
        }
        request(vec!["REGISTER".into(), self.endpoint.clone(), format!("p256dh={};auth={}", self.p256dh, self.auth)])
    }
}

pub fn unregister(endpoint: &str) -> Result<Command, &'static str> {
    if !valid_endpoint(endpoint) { return Err("Invalid WebPush endpoint"); }
    request(vec!["UNREGISTER".into(), endpoint.into()])
}

#[derive(PartialEq, Eq)]
pub enum Reply<'a> {
    Registered(&'a str),
    Unregistered(&'a str),
    Failed,
    Invalid,
}

pub fn parse(message: &Message) -> Option<Reply<'_>> {
    let Command::Raw(command, args) = &message.command else { return None; };
    if command.eq_ignore_ascii_case("FAIL") && args.first().is_some_and(|arg| arg.eq_ignore_ascii_case("WEBPUSH")) { return Some(Reply::Failed); }
    if !command.eq_ignore_ascii_case("WEBPUSH") { return None; }
    Some(match args.as_slice() {
        [action, endpoint] if action.eq_ignore_ascii_case("REGISTER") && valid_endpoint(endpoint) => Reply::Registered(endpoint),
        [action, endpoint] if action.eq_ignore_ascii_case("UNREGISTER") && valid_endpoint(endpoint) => Reply::Unregistered(endpoint),
        _ => Reply::Invalid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subscription() -> Subscription {
        use p256::elliptic_curve::sec1::ToEncodedPoint as _;
        let key = p256::SecretKey::from_slice(&[1; 32]).unwrap();
        Subscription { endpoint: "https://push.example.test/subscription".into(),
            p256dh: URL_SAFE_NO_PAD.encode(key.public_key().to_encoded_point(false).as_bytes()),
            auth: URL_SAFE_NO_PAD.encode([2; 16]) }
    }

    #[test]
    fn requests_validate_curve_auth_endpoint_and_wire_length() {
        let mut value = subscription();
        assert!(value.register().is_ok());
        assert!(!format!("{value:?}").contains(&value.auth));
        assert!(!format!("{value:?}").contains(&value.endpoint));
        value.auth = URL_SAFE_NO_PAD.encode([2; 15]);
        assert!(value.register().is_err());
        value = subscription();
        value.p256dh = URL_SAFE_NO_PAD.encode([4; 65]);
        assert!(value.register().is_err());
        value = subscription();
        value.endpoint = format!("https://push.example.test/{}", "x".repeat(500));
        assert!(value.register().is_err());
        for invalid in ["http://push.test/a", "https://u:p@push.test/a", "https://push.test/a#fragment", "https://push.test/\r\nQUIT"] {
            assert!(unregister(invalid).is_err());
        }
    }

    #[test]
    fn accepts_real_soju_failures_without_endpoint_and_strict_acknowledgments() {
        assert!(matches!(parse(&"FAIL WEBPUSH INVALID_PARAMS REGISTER :Invalid endpoint".parse().unwrap()), Some(Reply::Failed)));
        assert!(matches!(parse(&"WEBPUSH REGISTER https://push.test/a".parse().unwrap()), Some(Reply::Registered(_))));
        assert!(matches!(parse(&"WEBPUSH REGISTER https://push.test/a extra".parse().unwrap()), Some(Reply::Invalid)));
        assert!(parse(&"FAIL OTHER INVALID_PARAMS :wrong command".parse().unwrap()).is_none());
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "action")]
pub enum Action {
    Get,
    Lookup { scope: String },
    Register { scope: String, vapid: String, subscription: Subscription },
    Unregister { scope: String, endpoint: String },
}

#[derive(Clone, Deserialize, Serialize)]
pub struct WebRequest {
    pub connection_id: String,
    pub request_id: String,
    #[serde(flatten)]
    pub action: Action,
}

impl std::fmt::Debug for WebRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WebPushRequest([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum Status {
    Ready,
    Registered,
    Unregistered,
    Unavailable,
    Invalid,
    Busy,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BrowserContext {
    pub label: String,
    pub nick: String,
    pub chantypes: String,
    pub statusmsg: String,
    pub casemapping: String,
}
