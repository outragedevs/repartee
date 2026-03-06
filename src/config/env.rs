use std::collections::HashMap;
use std::path::Path;

use color_eyre::eyre::Result;

/// Load environment variables from a .env file.
/// Format: KEY=VALUE (one per line), # comments, empty lines skipped.
pub fn load_env(path: &Path) -> Result<HashMap<String, String>> {
    let mut vars = HashMap::new();
    if !path.exists() {
        return Ok(vars);
    }
    let content = std::fs::read_to_string(path)?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim().to_string();
            let value = value.trim().trim_matches('"').trim_matches('\'').to_string();
            vars.insert(key, value);
        }
    }
    Ok(vars)
}

/// Apply .env credentials to server configs.
/// For each server with id "foo", looks for `FOO_SASL_USER`, `FOO_SASL_PASS`, `FOO_PASSWORD`.
pub fn apply_credentials(
    servers: &mut HashMap<String, super::ServerConfig>,
    env: &HashMap<String, String>,
) {
    for (id, server) in servers.iter_mut() {
        let prefix = id.to_uppercase();
        let mut key = String::with_capacity(prefix.len() + 10);
        let mut get = |suffix: &str| -> Option<String> {
            key.clear();
            key.push_str(&prefix);
            key.push_str(suffix);
            env.get(&key).cloned()
        };
        if let Some(val) = get("_SASL_USER") {
            server.sasl_user = Some(val);
        }
        if let Some(val) = get("_SASL_PASS") {
            server.sasl_pass = Some(val);
        }
        if let Some(val) = get("_PASSWORD") {
            server.password = Some(val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_env_file() {
        let dir = std::env::temp_dir().join("rustirc_test_env");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "# Comment").unwrap();
        writeln!(f, "SASL_PASS=secret123").unwrap();
        writeln!(f, "SERVER_PASS=\"quoted value\"").unwrap();
        writeln!(f).unwrap();

        let vars = load_env(&path).unwrap();
        assert_eq!(vars.get("SASL_PASS").unwrap(), "secret123");
        assert_eq!(vars.get("SERVER_PASS").unwrap(), "quoted value");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_env_missing_file() {
        let path = std::env::temp_dir().join("rustirc_test_nonexistent/.env");
        let vars = load_env(&path).unwrap();
        assert!(vars.is_empty());
    }

    #[test]
    fn apply_credentials_to_servers() {
        let mut servers = HashMap::new();
        servers.insert(
            "libera".to_string(),
            super::super::ServerConfig {
                label: "Libera".to_string(),
                address: "irc.libera.chat".to_string(),
                port: 6697,
                tls: true,
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
            },
        );

        let mut env = HashMap::new();
        env.insert("LIBERA_SASL_USER".to_string(), "myuser".to_string());
        env.insert("LIBERA_SASL_PASS".to_string(), "mypass".to_string());

        apply_credentials(&mut servers, &env);

        let server = servers.get("libera").unwrap();
        assert_eq!(server.sasl_user.as_deref(), Some("myuser"));
        assert_eq!(server.sasl_pass.as_deref(), Some("mypass"));
        assert!(server.password.is_none());
    }
}
