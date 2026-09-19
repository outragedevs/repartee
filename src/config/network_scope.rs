use sha2::{Digest, Sha256};

use super::ServerConfig;

pub const BOUNCER_SCOPE_PREFIX: &str = "bouncer:v1:";

pub fn network_scope(account_id: &str, server: &ServerConfig, default_username: &str) -> String {
    if server.bouncer_network_id.is_none() && !server.bouncer_control {
        return server.label.clone();
    }
    let mut digest = Sha256::new();
    for component in [
        account_id,
        &server.address.to_ascii_lowercase(),
        &server.port.to_string(),
        if server.tls { "tls" } else { "plain" },
        server.sasl_user.as_deref().unwrap_or_default(),
        &server.sasl_mechanism.as_deref().unwrap_or("auto").to_ascii_uppercase(),
        server.username.as_deref().unwrap_or(default_username),
        server.client_cert_path.as_deref().unwrap_or_default(),
        server.sasl_key_path.as_deref().unwrap_or_default(),
    ] {
        digest.update((component.len() as u64).to_be_bytes());
        digest.update(component.as_bytes());
    }
    let network = server.bouncer_network_id.as_deref().map_or_else(
        || "control".to_string(),
        |id| {
            id.parse::<u64>()
                .map_or_else(|_| id.to_string(), |id| id.to_string())
        },
    );
    format!(
        "{BOUNCER_SCOPE_PREFIX}{}:{network}",
        hex::encode(digest.finalize())
    )
}

pub fn is_bouncer_scope(scope: &str) -> bool {
    let network = scope.split('\u{1f}').next().unwrap_or(scope);
    let Some((account, id)) = network
        .strip_prefix(BOUNCER_SCOPE_PREFIX)
        .and_then(|value| value.split_once(':'))
    else {
        return false;
    };
    account.len() == 64
        && account
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && (id == "control"
            || id
                .parse::<u64>()
                .is_ok_and(|value| value > 0 && value.to_string() == id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_survives_rename_and_password_rotation_but_isolates_accounts() {
        let mut server: ServerConfig = toml::from_str(
            "label = 'Same'\naddress = 'bnc.example.org'\nport = 6697\ntls = true\nchannels = []\nbouncer_network_id = '00042'",
        ).unwrap();
        let original = network_scope("account", &server, "default");
        assert_ne!(network_scope("account", &server, "other-login"), original);
        server.username = Some("default".into());
        assert_eq!(network_scope("account", &server, "other-login"), original);
        server.username = None;
        server.sasl_mechanism = Some("PLAIN".into());
        let plain = network_scope("account", &server, "default");
        server.sasl_mechanism = Some("EXTERNAL".into());
        assert_ne!(network_scope("account", &server, "default"), plain);
        server.sasl_mechanism = Some("plain".into());
        assert_eq!(network_scope("account", &server, "default"), plain);
        server.sasl_mechanism = None;
        server.label = "Renamed".into();
        server.bouncer_network_id = Some("42".into());
        server.sasl_pass = Some("rotated-password".into());
        assert_eq!(network_scope("account", &server, "default"), original);
        assert_ne!(network_scope("other-account", &server, "default"), original);
        server.bouncer_network_id = Some("43".into());
        assert_ne!(network_scope("account", &server, "default"), original);
        server.bouncer_network_id = Some("42".into());
        server.sasl_user = Some("another-user".into());
        assert_ne!(network_scope("account", &server, "default"), original);
        server.sasl_user = None;
        server.address = "another.example.org".into();
        assert_ne!(network_scope("account", &server, "default"), original);
        server.bouncer_network_id = None;
        assert_eq!(network_scope("account", &server, "default"), "Renamed");
    }
    #[test]
    fn invalid_network_ids_are_rejected_before_config_can_reach_keyring() {
        for id in ["foo", "0", "-1", "9223372036854775808", "42\u{1f}#channel"] {
            let config = format!("label = 'Bouncer'\naddress = 'bnc.example.org'\nport = 6697\ntls = true\nchannels = []\nbouncer_network_id = '{id}'");
            assert!(toml::from_str::<ServerConfig>(&config).is_err(), "accepted {id:?}");
        }
    }
    #[test]
    fn bouncer_like_display_labels_are_not_scopes() {
        for label in [
            "bouncer:v1:MyNetwork",
            "bouncer:v1:account:42",
            "bouncer:v1:",
            "bouncer:v1:00:42",
        ] {
            assert!(!is_bouncer_scope(label));
        }
        let scope = format!("bouncer:v1:{}:42", "a".repeat(64));
        assert!(is_bouncer_scope(&scope));
        assert!(is_bouncer_scope(&format!("{scope}\u{1f}#channel")));
        assert!(!is_bouncer_scope(&format!("{scope}suffix")));
    }
}
