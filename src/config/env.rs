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
            let value = value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            vars.insert(key, value);
        }
    }
    Ok(vars)
}

/// Set a key in the `.env` file. Creates the file if it doesn't exist.
/// Updates existing keys in place, appends new ones at the end.
pub fn set_env_value(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut lines: Vec<String> = if path.exists() {
        std::fs::read_to_string(path)?
            .lines()
            .map(String::from)
            .collect()
    } else {
        Vec::new()
    };

    let prefix = format!("{key}=");
    let new_line = format!("{key}={value}");
    let mut found = false;

    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.starts_with(&prefix) {
            *line = new_line.clone();
            found = true;
            break;
        }
    }

    if !found {
        // Add a blank line separator if the file is non-empty and doesn't end with one.
        if !lines.is_empty() && !lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(new_line);
    }

    std::fs::write(path, lines.join("\n") + "\n")?;
    Ok(())
}

/// Apply .env credentials to the web config.
/// Reads `WEB_PASSWORD` from the env map.
pub fn apply_web_credentials(web: &mut super::WebConfig, env: &HashMap<String, String>) {
    if let Some(val) = env.get("WEB_PASSWORD") {
        web.password.clone_from(val);
    }
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
        let dir = std::env::temp_dir().join("repartee_test_env");
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
        let path = std::env::temp_dir().join("repartee_test_nonexistent/.env");
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

    #[test]
    fn set_env_value_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");

        set_env_value(&path, "WEB_PASSWORD", "secret").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("WEB_PASSWORD=secret"));
    }

    #[test]
    fn set_env_value_updates_existing_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "FOO=old\nWEB_PASSWORD=old\nBAR=keep\n").unwrap();

        set_env_value(&path, "WEB_PASSWORD", "new").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("WEB_PASSWORD=new"));
        assert!(content.contains("FOO=old"));
        assert!(content.contains("BAR=keep"));
        assert!(!content.contains("WEB_PASSWORD=old"));
    }

    #[test]
    fn set_env_value_appends_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "EXISTING=value\n").unwrap();

        set_env_value(&path, "NEW_KEY", "new_value").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("EXISTING=value"));
        assert!(content.contains("NEW_KEY=new_value"));
    }
}
