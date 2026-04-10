pub const APP_NAME: &str = "repartee";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_URL: &str = "https://repart.ee/";

/// Base URL for the dictionary repository (raw GitHub content).
pub const DICTS_REPO_URL: &str =
    "https://raw.githubusercontent.com/outragedevs/repartee-dicts/main";

/// URL for the dictionary manifest file.
pub const DICTS_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/outragedevs/repartee-dicts/main/manifest.json";

/// Default quit/part message: "repartee 0.8.0 — <https://repart.ee/>"
pub fn default_quit_message() -> String {
    format!("{APP_NAME} {APP_VERSION} — {APP_URL}")
}

/// WHOX field selector string.
/// Fields requested: t=token, c=channel, u=user, i=ip, h=host,
/// n=nick, f=flags, a=account, r=realname.
/// Note: `s` (server), `d` (hopcount), `l` (idle) are omitted because
/// `IRCnet` ircd 2.12 silently drops unsupported fields, causing arg count
/// mismatches in the parser.
pub const WHOX_FIELDS: &str = "%tcuihnfar";

/// All themes shipped in the binary via `include_str!`.
///
/// `default.theme` gets special treatment in [`sync_bundled_themes_in`]:
/// if the user's copy differs from the bundled version, the user's copy
/// is backed up to `default.theme.bak` and then overwritten. Other
/// themes are copied **only if missing** so that user customizations
/// are never silently clobbered.
const BUNDLED_THEMES: &[(&str, &str)] = &[
    ("default.theme", include_str!("../themes/default.theme")),
    ("spring.theme", include_str!("../themes/spring.theme")),
];

/// Filename of the canonical default theme — the only entry in
/// [`BUNDLED_THEMES`] that is force-updated on version bumps.
const DEFAULT_THEME_FILE: &str = "default.theme";

use std::path::{Path, PathBuf};

pub fn home_dir() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(format!(".{APP_NAME}"))
}

pub fn config_path() -> PathBuf {
    home_dir().join("config.toml")
}

pub fn theme_dir() -> PathBuf {
    home_dir().join("themes")
}

pub fn env_path() -> PathBuf {
    home_dir().join(".env")
}

pub fn log_dir() -> PathBuf {
    home_dir().join("logs")
}

pub fn scripts_dir() -> PathBuf {
    home_dir().join("scripts")
}

pub fn sessions_dir() -> PathBuf {
    home_dir().join("sessions")
}

pub fn dicts_dir() -> PathBuf {
    home_dir().join("dicts")
}

pub fn certs_dir() -> PathBuf {
    home_dir().join("certs")
}

/// Create config directory and write default files on first run.
pub fn ensure_config_dir() {
    if let Err(e) = std::fs::create_dir_all(theme_dir()) {
        tracing::warn!("failed to create theme dir: {e}");
    }
    if let Err(e) = std::fs::create_dir_all(log_dir()) {
        tracing::warn!("failed to create log dir: {e}");
    }
    if let Err(e) = std::fs::create_dir_all(scripts_dir()) {
        tracing::warn!("failed to create scripts dir: {e}");
    }
    if let Err(e) = std::fs::create_dir_all(sessions_dir()) {
        tracing::warn!("failed to create sessions dir: {e}");
    }
    if let Err(e) = std::fs::create_dir_all(dicts_dir()) {
        tracing::warn!("failed to create dicts dir: {e}");
    }

    // Write default config if missing
    let cfg = config_path();
    if !cfg.exists() {
        let default_cfg = crate::config::default_config();
        if let Err(e) = crate::config::save_config(&cfg, &default_cfg) {
            tracing::warn!("failed to write default config: {e}");
        } else {
            tracing::info!("Created default config at {}", cfg.display());
        }
    }

    // Sync bundled themes to the user's theme directory (creates missing
    // themes, force-updates `default.theme` with backup).
    sync_bundled_themes_in(&theme_dir());
}

/// Sync bundled themes into the user's theme directory.
///
/// Behavior per theme:
/// - If the user doesn't have it → write the bundled copy.
/// - If the theme is [`DEFAULT_THEME_FILE`] **and** the user's content
///   differs from the bundled content → back up the user's version to
///   `default.theme.bak` (overwriting any previous backup) and write
///   the new bundled version in its place.
/// - Otherwise (non-default theme that already exists) → leave it alone.
///   Users may have customized these files.
///
/// All failures are logged and skipped — theme sync is best-effort and
/// must never block startup.
fn sync_bundled_themes_in(dir: &Path) {
    for (name, content) in BUNDLED_THEMES {
        let path = dir.join(name);

        if !path.exists() {
            match std::fs::write(&path, content) {
                Ok(()) => tracing::info!("Installed bundled theme: {}", path.display()),
                Err(e) => tracing::warn!("failed to write bundled theme {name}: {e}"),
            }
            continue;
        }

        // Only the canonical default theme is force-updated.
        if *name != DEFAULT_THEME_FILE {
            continue;
        }

        let Ok(current) = std::fs::read_to_string(&path) else {
            tracing::warn!("failed to read existing {name} for diff check");
            continue;
        };

        if current == *content {
            continue;
        }

        // Content differs — back up user's copy, then overwrite.
        let backup = dir.join(format!("{name}.bak"));
        if let Err(e) = std::fs::copy(&path, &backup) {
            tracing::warn!(
                "failed to back up {name} to {}: {e} — skipping update",
                backup.display()
            );
            continue;
        }
        match std::fs::write(&path, content) {
            Ok(()) => tracing::info!(
                "Updated {} (previous version backed up to {})",
                path.display(),
                backup.display()
            ),
            Err(e) => tracing::warn!("failed to update {name}: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled_default() -> &'static str {
        BUNDLED_THEMES
            .iter()
            .find(|(name, _)| *name == DEFAULT_THEME_FILE)
            .map(|(_, c)| *c)
            .expect("default.theme must be in BUNDLED_THEMES")
    }

    #[test]
    fn sync_installs_all_themes_on_first_run() {
        let dir = tempfile::tempdir().unwrap();
        sync_bundled_themes_in(dir.path());

        for (name, content) in BUNDLED_THEMES {
            let path = dir.path().join(name);
            assert!(path.exists(), "{name} should have been created");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), *content);
        }
    }

    #[test]
    fn sync_leaves_matching_default_theme_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DEFAULT_THEME_FILE);
        std::fs::write(&path, bundled_default()).unwrap();

        sync_bundled_themes_in(dir.path());

        assert!(
            !dir.path().join("default.theme.bak").exists(),
            "no backup should be created when content matches"
        );
    }

    #[test]
    fn sync_backs_up_and_overwrites_changed_default_theme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DEFAULT_THEME_FILE);
        let user_content = "# user's old default theme\n";
        std::fs::write(&path, user_content).unwrap();

        sync_bundled_themes_in(dir.path());

        // Backup contains the user's old version.
        let backup = dir.path().join("default.theme.bak");
        assert!(backup.exists(), "backup should exist after overwrite");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), user_content);

        // Original now has the bundled content.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bundled_default());
    }

    #[test]
    fn sync_preserves_user_customized_non_default_theme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spring.theme");
        let user_customization = "# my customized spring theme\n";
        std::fs::write(&path, user_customization).unwrap();

        sync_bundled_themes_in(dir.path());

        // User's spring.theme is untouched — no backup, no overwrite.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            user_customization,
            "non-default themes must never be overwritten"
        );
        assert!(
            !dir.path().join("spring.theme.bak").exists(),
            "non-default themes must not produce .bak files"
        );
    }
}
