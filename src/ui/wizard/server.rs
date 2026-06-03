//! Server wizard: the field schema for adding/editing an IRC server and the
//! `ServerConfig` <-> field-values serializers. The reusable engine in [`super`]
//! knows none of this.

use std::collections::HashMap;

use super::{Field, FieldKind, FieldValue, WizardMode, WizardState};
use crate::commands::handlers_admin::CredUpdate;
use crate::config::ServerConfig;

const SASL_MECHS: &[&str] = &["Auto", "PLAIN", "EXTERNAL"];

/// Slugify a network name into a server id: lowercase, non-`[a-z0-9_]` runs
/// collapse to a single `_`, leading/trailing `_` trimmed.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_us = false;
    for ch in name.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// Make `base` unique against existing ids by appending `_2`, `_3`, …
#[must_use]
pub fn unique_id(base: &str, servers: &HashMap<String, ServerConfig>) -> String {
    let base = if base.is_empty() {
        "server".to_string()
    } else {
        base.to_string()
    };
    if !servers.contains_key(&base) {
        return base;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}_{n}");
        if !servers.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn mech_index(m: Option<&str>) -> usize {
    match m {
        Some("PLAIN") => 1,
        Some("EXTERNAL") => 2,
        _ => 0,
    }
}

fn mech_from_choice(choice: &str) -> Option<String> {
    match choice {
        "PLAIN" => Some("PLAIN".to_string()),
        "EXTERNAL" => Some("EXTERNAL".to_string()),
        _ => None,
    }
}

/// Trim and convert empty to `None`.
fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// The field schema. Page 0 = Basics, page 1 = Advanced. `edit` makes the
/// server-id field read-only (it is the map key and cannot change).
fn schema(edit: bool) -> Vec<Field> {
    let text = |key, label, page| Field {
        key,
        label,
        kind: FieldKind::Text,
        page,
        required: false,
        readonly: false,
    };
    vec![
        Field { key: "network", label: "Network Name", kind: FieldKind::Text, page: 0, required: true, readonly: false },
        Field { key: "address", label: "Server address / IP", kind: FieldKind::Text, page: 0, required: true, readonly: false },
        Field { key: "port", label: "Port", kind: FieldKind::Number, page: 0, required: false, readonly: false },
        Field { key: "tls", label: "Use TLS/SSL", kind: FieldKind::Toggle, page: 0, required: false, readonly: false },
        Field { key: "tls_verify", label: "Verify TLS certificate", kind: FieldKind::Toggle, page: 0, required: false, readonly: false },
        text("bind_ip", "Bind IP", 0),
        // Advanced
        Field { key: "id", label: "Server id", kind: FieldKind::Text, page: 1, required: false, readonly: edit },
        text("nick", "Nick", 1),
        text("username", "Username", 1),
        text("realname", "Realname", 1),
        text("channels", "Channels (comma-separated)", 1),
        Field { key: "password", label: "Server password", kind: FieldKind::Masked, page: 1, required: false, readonly: false },
        text("sasl_user", "SASL user", 1),
        Field { key: "sasl_pass", label: "SASL pass", kind: FieldKind::Masked, page: 1, required: false, readonly: false },
        Field { key: "sasl_mechanism", label: "SASL mechanism", kind: FieldKind::Select(SASL_MECHS.to_vec()), page: 1, required: false, readonly: false },
        text("encoding", "Encoding", 1),
        Field { key: "autoconnect", label: "Autoconnect", kind: FieldKind::Toggle, page: 1, required: false, readonly: false },
        Field { key: "auto_reconnect", label: "Auto-reconnect", kind: FieldKind::Toggle, page: 1, required: false, readonly: false },
        Field { key: "reconnect_delay", label: "Reconnect delay (s)", kind: FieldKind::Number, page: 1, required: false, readonly: false },
        Field { key: "reconnect_max_retries", label: "Reconnect max retries", kind: FieldKind::Number, page: 1, required: false, readonly: false },
        text("autosendcmd", "Autosendcmd", 1),
        text("client_cert_path", "Client cert path", 1),
    ]
}

/// Default values for an "add" wizard.
fn add_values(fields: &[Field]) -> Vec<FieldValue> {
    fields
        .iter()
        .map(|f| match (f.key, &f.kind) {
            ("port", _) => FieldValue::Text("6667".into()),
            ("tls_verify" | "autoconnect" | "auto_reconnect", _) => FieldValue::Bool(true),
            (_, FieldKind::Toggle) => FieldValue::Bool(false),
            (_, FieldKind::Select(_)) => FieldValue::Choice(0),
            _ => FieldValue::Text(String::new()),
        })
        .collect()
}

/// Values pre-filled from an existing server (edit mode). Masked credential
/// fields are left EMPTY + untouched so they mean "unchanged".
fn edit_values(fields: &[Field], id: &str, s: &ServerConfig) -> Vec<FieldValue> {
    fields
        .iter()
        .map(|f| match (f.key, &f.kind) {
            ("network", _) => FieldValue::Text(s.label.clone()),
            ("address", _) => FieldValue::Text(s.address.clone()),
            ("port", _) => FieldValue::Text(s.port.to_string()),
            ("tls", _) => FieldValue::Bool(s.tls),
            ("tls_verify", _) => FieldValue::Bool(s.tls_verify),
            ("bind_ip", _) => FieldValue::Text(s.bind_ip.clone().unwrap_or_default()),
            ("id", _) => FieldValue::Text(id.to_string()),
            ("nick", _) => FieldValue::Text(s.nick.clone().unwrap_or_default()),
            ("username", _) => FieldValue::Text(s.username.clone().unwrap_or_default()),
            ("realname", _) => FieldValue::Text(s.realname.clone().unwrap_or_default()),
            ("channels", _) => FieldValue::Text(s.channels.join(", ")),
            ("sasl_user", _) => FieldValue::Text(s.sasl_user.clone().unwrap_or_default()),
            ("sasl_mechanism", _) => FieldValue::Choice(mech_index(s.sasl_mechanism.as_deref())),
            ("encoding", _) => FieldValue::Text(s.encoding.clone().unwrap_or_default()),
            ("autoconnect", _) => FieldValue::Bool(s.autoconnect),
            ("auto_reconnect", _) => FieldValue::Bool(s.auto_reconnect.unwrap_or(true)),
            ("reconnect_delay", _) => {
                FieldValue::Text(s.reconnect_delay.map(|v| v.to_string()).unwrap_or_default())
            }
            ("reconnect_max_retries", _) => FieldValue::Text(
                s.reconnect_max_retries
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            ("autosendcmd", _) => FieldValue::Text(s.autosendcmd.clone().unwrap_or_default()),
            ("client_cert_path", _) => {
                FieldValue::Text(s.client_cert_path.clone().unwrap_or_default())
            }
            (_, FieldKind::Toggle) => FieldValue::Bool(false),
            (_, FieldKind::Select(_)) => FieldValue::Choice(0),
            // Masked credential fields (password / sasl_pass) and any other
            // text default to empty; empty + untouched means "unchanged".
            _ => FieldValue::Text(String::new()),
        })
        .collect()
}

/// Construct the server wizard for add or edit.
#[must_use]
pub fn build_wizard(mode: WizardMode, existing: Option<&ServerConfig>) -> WizardState {
    let edit = matches!(mode, WizardMode::Edit { .. });
    let fields = schema(edit);
    let (title, values) = match (&mode, existing) {
        (WizardMode::Edit { id }, Some(s)) => {
            (format!("Edit Server — {id}"), edit_values(&fields, id, s))
        }
        _ => ("Add Server".to_string(), add_values(&fields)),
    };
    WizardState::new(mode, title, vec!["Basics", "Advanced"], fields, values)
}

/// Result of a successful build: ready to hand to `apply_server_config`.
#[derive(Debug)]
pub struct BuiltServer {
    pub id: String,
    pub config: ServerConfig,
    pub password: CredUpdate,
    pub sasl_pass: CredUpdate,
}

/// Resolve a masked credential field into a [`CredUpdate`] and the in-memory
/// value: untouched = keep the existing stored value, touched-empty = remove,
/// touched-nonempty = set.
fn resolve_cred(w: &WizardState, key: &str, existing: Option<&str>) -> (CredUpdate, Option<String>) {
    if !w.was_touched(key) {
        return (CredUpdate::Keep, existing.map(ToOwned::to_owned));
    }
    let v = w.text(key);
    if v.is_empty() {
        (CredUpdate::Remove, None)
    } else {
        (CredUpdate::Set(v.to_string()), Some(v.to_string()))
    }
}

/// Validate + serialize the wizard into a `ServerConfig` and credential updates.
/// `servers` is used for id uniqueness (add) and for the existing credentials
/// (edit, when a masked field was left unchanged).
///
/// # Errors
/// Returns a human-readable message when a required field is empty, a number is
/// out of range, or (add mode) the typed id collides — though collisions are
/// auto-suffixed, so this is reserved for malformed numeric input.
pub fn build(w: &WizardState, servers: &HashMap<String, ServerConfig>) -> Result<BuiltServer, String> {
    if let Some(label) = w.first_missing_required() {
        return Err(format!("{label} is required"));
    }
    let network = w.text("network").trim().to_string();
    let address = w.text("address").trim().to_string();

    let tls = w.boolean("tls");
    let mut port: u16 = {
        let raw = w.text("port").trim();
        if raw.is_empty() {
            if tls { 6697 } else { 6667 }
        } else {
            raw.parse()
                .map_err(|_| "Port must be a number 1–65535".to_string())?
        }
    };
    if tls && port == 6667 {
        port = 6697;
    }

    let reconnect_delay = parse_opt_num::<u64>(w.text("reconnect_delay"), "Reconnect delay")?;
    let reconnect_max_retries =
        parse_opt_num::<u32>(w.text("reconnect_max_retries"), "Reconnect max retries")?;

    // id resolution.
    let id = match &w.mode {
        WizardMode::Edit { id } => id.clone(),
        WizardMode::Add => {
            let typed = w.text("id").trim();
            let base = if typed.is_empty() {
                slugify(&network)
            } else {
                slugify(typed)
            };
            unique_id(&base, servers)
        }
    };

    let existing = servers.get(&id);
    let (password, pw_mem) = resolve_cred(w, "password", existing.and_then(|s| s.password.as_deref()));
    let (sasl_pass, sasl_mem) =
        resolve_cred(w, "sasl_pass", existing.and_then(|s| s.sasl_pass.as_deref()));

    let config = ServerConfig {
        label: network,
        address,
        port,
        tls,
        tls_verify: w.boolean("tls_verify"),
        autoconnect: w.boolean("autoconnect"),
        channels: split_channels(w.text("channels")),
        nick: opt(w.text("nick")),
        username: opt(w.text("username")),
        realname: opt(w.text("realname")),
        password: pw_mem,
        sasl_user: opt(w.text("sasl_user")),
        sasl_pass: sasl_mem,
        bind_ip: opt(w.text("bind_ip")),
        encoding: opt(w.text("encoding")),
        auto_reconnect: Some(w.boolean("auto_reconnect")),
        reconnect_delay,
        reconnect_max_retries,
        autosendcmd: opt(w.text("autosendcmd")),
        sasl_mechanism: mech_from_choice(w.choice_str("sasl_mechanism")),
        client_cert_path: opt(w.text("client_cert_path")),
    };

    Ok(BuiltServer {
        id,
        config,
        password,
        sasl_pass,
    })
}

fn split_channels(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn parse_opt_num<T: std::str::FromStr>(raw: &str, what: &str) -> Result<Option<T>, String> {
    let t = raw.trim();
    if t.is_empty() {
        Ok(None)
    } else {
        t.parse::<T>()
            .map(Some)
            .map_err(|_| format!("{what} must be a number"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_servers() -> HashMap<String, ServerConfig> {
        HashMap::new()
    }

    fn dummy() -> ServerConfig {
        ServerConfig {
            label: "x".into(),
            address: "x".into(),
            port: 6667,
            tls: false,
            tls_verify: true,
            autoconnect: false,
            channels: vec![],
            nick: None,
            username: None,
            realname: None,
            password: None,
            sasl_user: None,
            sasl_pass: None,
            bind_ip: None,
            encoding: None,
            auto_reconnect: None,
            reconnect_delay: None,
            reconnect_max_retries: None,
            autosendcmd: None,
            sasl_mechanism: None,
            client_cert_path: None,
        }
    }

    fn set_text(w: &mut WizardState, key: &str, text: &str) {
        let i = w.fields.iter().position(|f| f.key == key).unwrap();
        w.values[i] = FieldValue::Text(text.into());
        w.touched[i] = true;
    }
    fn set_bool(w: &mut WizardState, key: &str, b: bool) {
        let i = w.fields.iter().position(|f| f.key == key).unwrap();
        w.values[i] = FieldValue::Bool(b);
        w.touched[i] = true;
    }

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Libera.Chat"), "libera_chat");
        assert_eq!(slugify("  My Net!! "), "my_net");
        assert_eq!(slugify("OFTC"), "oftc");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn unique_id_suffixes() {
        let mut servers = empty_servers();
        servers.insert("libera".into(), dummy());
        assert_eq!(unique_id("libera", &servers), "libera_2");
        assert_eq!(unique_id("fresh", &servers), "fresh");
        assert_eq!(unique_id("", &servers), "server");
    }

    #[test]
    fn add_build_requires_network_and_address() {
        let w = build_wizard(WizardMode::Add, None);
        let err = build(&w, &empty_servers()).unwrap_err();
        assert!(err.contains("Network Name"));
    }

    #[test]
    fn add_build_happy_path_tls_bumps_port() {
        let mut w = build_wizard(WizardMode::Add, None);
        set_text(&mut w, "network", "Libera.Chat");
        set_text(&mut w, "address", "irc.libera.chat");
        set_bool(&mut w, "tls", true);
        let built = build(&w, &empty_servers()).unwrap();
        assert_eq!(built.id, "libera_chat");
        assert_eq!(built.config.port, 6697); // default 6667 + tls -> 6697
        assert!(built.config.tls);
        assert_eq!(built.config.label, "Libera.Chat");
        assert!(matches!(built.password, CredUpdate::Keep));
    }

    #[test]
    fn add_build_channels_and_mechanism() {
        let mut w = build_wizard(WizardMode::Add, None);
        set_text(&mut w, "network", "Net");
        set_text(&mut w, "address", "host");
        set_text(&mut w, "channels", "#rust, #repartee ,, #ratatui");
        let mech_i = w.fields.iter().position(|f| f.key == "sasl_mechanism").unwrap();
        w.values[mech_i] = FieldValue::Choice(1); // PLAIN
        let built = build(&w, &empty_servers()).unwrap();
        assert_eq!(built.config.channels, vec!["#rust", "#repartee", "#ratatui"]);
        assert_eq!(built.config.sasl_mechanism.as_deref(), Some("PLAIN"));
    }

    #[test]
    fn add_build_invalid_port_errors() {
        let mut w = build_wizard(WizardMode::Add, None);
        set_text(&mut w, "network", "Net");
        set_text(&mut w, "address", "host");
        set_text(&mut w, "port", "notanumber");
        assert!(build(&w, &empty_servers()).unwrap_err().contains("Port"));
    }

    #[test]
    fn add_build_password_sets_credupdate_and_memory() {
        let mut w = build_wizard(WizardMode::Add, None);
        set_text(&mut w, "network", "Net");
        set_text(&mut w, "address", "host");
        set_text(&mut w, "password", "hunter2");
        let built = build(&w, &empty_servers()).unwrap();
        assert!(matches!(built.password, CredUpdate::Set(ref v) if v == "hunter2"));
        assert_eq!(built.config.password.as_deref(), Some("hunter2"));
    }

    #[test]
    fn edit_untouched_password_is_kept() {
        let mut servers = empty_servers();
        let mut s = dummy();
        s.label = "Net".into();
        s.address = "host".into();
        s.password = Some("orig".into());
        servers.insert("net".into(), s.clone());

        let w = build_wizard(WizardMode::Edit { id: "net".into() }, Some(&s));
        let built = build(&w, &servers).unwrap();
        assert!(matches!(built.password, CredUpdate::Keep));
        assert_eq!(built.config.password.as_deref(), Some("orig"));
        assert_eq!(built.id, "net");
    }

    #[test]
    fn edit_prefill_round_trips_every_field() {
        // Guard against edit_values' catch-all silently blanking a field that
        // is added to the schema but not mapped here.
        let mut servers = empty_servers();
        let s = ServerConfig {
            label: "Full Net".into(),
            address: "irc.full.net".into(),
            port: 7000,
            tls: true,
            tls_verify: false,
            autoconnect: false,
            channels: vec!["#one".into(), "#two".into()],
            nick: Some("nick".into()),
            username: Some("user".into()),
            realname: Some("Real Name".into()),
            password: Some("secret".into()),
            sasl_user: Some("sasluser".into()),
            sasl_pass: Some("saslsecret".into()),
            bind_ip: Some("2001:db8::1".into()),
            encoding: Some("latin1".into()),
            auto_reconnect: Some(false),
            reconnect_delay: Some(42),
            reconnect_max_retries: Some(7),
            autosendcmd: Some("/msg NickServ identify".into()),
            sasl_mechanism: Some("PLAIN".into()),
            client_cert_path: Some("/tmp/cert.pem".into()),
        };
        servers.insert("full".into(), s.clone());

        let w = build_wizard(WizardMode::Edit { id: "full".into() }, Some(&s));
        let built = build(&w, &servers).unwrap();
        let c = &built.config;
        assert_eq!(c.label, "Full Net");
        assert_eq!(c.address, "irc.full.net");
        assert_eq!(c.port, 7000);
        assert!(c.tls);
        assert!(!c.tls_verify);
        assert!(!c.autoconnect);
        assert_eq!(c.channels, vec!["#one", "#two"]);
        assert_eq!(c.nick.as_deref(), Some("nick"));
        assert_eq!(c.username.as_deref(), Some("user"));
        assert_eq!(c.realname.as_deref(), Some("Real Name"));
        assert_eq!(c.sasl_user.as_deref(), Some("sasluser"));
        assert_eq!(c.bind_ip.as_deref(), Some("2001:db8::1"));
        assert_eq!(c.encoding.as_deref(), Some("latin1"));
        assert_eq!(c.auto_reconnect, Some(false));
        assert_eq!(c.reconnect_delay, Some(42));
        assert_eq!(c.reconnect_max_retries, Some(7));
        assert_eq!(c.autosendcmd.as_deref(), Some("/msg NickServ identify"));
        assert_eq!(c.sasl_mechanism.as_deref(), Some("PLAIN"));
        assert_eq!(c.client_cert_path.as_deref(), Some("/tmp/cert.pem"));
        // Masked creds are untouched in edit → kept from the existing entry.
        assert_eq!(c.password.as_deref(), Some("secret"));
        assert_eq!(c.sasl_pass.as_deref(), Some("saslsecret"));
    }

    #[test]
    fn edit_prefill_round_trips_core_fields() {
        let mut servers = empty_servers();
        let mut s = dummy();
        s.label = "Libera".into();
        s.address = "irc.libera.chat".into();
        s.port = 6697;
        s.tls = true;
        s.channels = vec!["#a".into(), "#b".into()];
        s.sasl_mechanism = Some("EXTERNAL".into());
        servers.insert("libera".into(), s.clone());

        let w = build_wizard(WizardMode::Edit { id: "libera".into() }, Some(&s));
        let built = build(&w, &servers).unwrap();
        assert_eq!(built.config.label, "Libera");
        assert_eq!(built.config.address, "irc.libera.chat");
        assert_eq!(built.config.port, 6697);
        assert!(built.config.tls);
        assert_eq!(built.config.channels, vec!["#a", "#b"]);
        assert_eq!(built.config.sasl_mechanism.as_deref(), Some("EXTERNAL"));
        // id field is read-only in edit mode.
        let id_field = w.fields.iter().find(|f| f.key == "id").unwrap();
        assert!(id_field.readonly);
    }
}
