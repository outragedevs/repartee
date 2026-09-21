use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::e2e::keyring::{ChannelConfig, ChannelMode};
use crate::settings_model::{SettingChange, SettingField, SettingKind, SettingsScope};

use super::App;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EncryptionRule {
    enabled: bool,
    mode: String,
}

struct Network {
    id: String,
    label: String,
    scope: String,
}

impl Network {
    fn path(&self, scope: SettingsScope) -> String {
        let kind = if scope == SettingsScope::EncryptionChannels {
            "encryption"
        } else {
            "translation"
        };
        format!(
            "servers.{}.{kind}_channels_{}",
            self.id,
            hex::encode(&self.scope)
        )
    }
}

impl App {
    fn settings_networks(&self) -> Vec<Network> {
        let mut networks = BTreeMap::new();
        for (id, server) in &self.config.servers {
            if !server.bouncer_control {
                networks.insert(
                    id.clone(),
                    Network {
                        id: id.clone(),
                        label: server.label.clone(),
                        scope: crate::config::network_scope::network_scope(
                            id,
                            server,
                            &self.config.general.username,
                        ),
                    },
                );
            }
        }
        for (id, connection) in &self.state.connections {
            if id != Self::DEFAULT_CONN_ID && !connection.bouncer_control() {
                networks.insert(
                    id.clone(),
                    Network {
                        id: id.clone(),
                        label: connection.label.clone(),
                        scope: connection.network_key().to_string(),
                    },
                );
            }
        }
        networks.into_values().collect()
    }

    fn encryption_rules(
        &self,
        network: &Network,
    ) -> Result<BTreeMap<String, EncryptionRule>, String> {
        let manager = self.state.e2e_manager.as_ref().ok_or("Enable logging and E2E, then restart the main process before managing encrypted channels.")?;
        let keyring = manager.keyring();
        let candidates: BTreeSet<_> = keyring
            .list_all_channel_configs()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|c| crate::e2e::wire_context(&c.channel).to_string())
            .filter(|name| crate::e2e::is_channel_target(name))
            .collect();
        let mut rules = BTreeMap::new();
        for channel in candidates {
            if let Some(config) = keyring
                .get_channel_config(&crate::e2e::scoped_context(&network.scope, &channel))
                .map_err(|e| e.to_string())?
            {
                rules.insert(
                    channel,
                    EncryptionRule {
                        enabled: config.enabled,
                        mode: config.mode.as_str().into(),
                    },
                );
            }
        }
        Ok(rules)
    }

    pub(crate) fn scoped_settings_fields(
        &self,
        scope: SettingsScope,
    ) -> Result<Vec<SettingField>, String> {
        if scope == SettingsScope::General {
            return Ok(crate::config::settings::catalog::fields(&self.config));
        }
        let networks = self.settings_networks();
        if networks.is_empty() {
            return Err("Add a network before managing channels.".into());
        }
        networks.iter().map(|network| {
            let value = if scope == SettingsScope::EncryptionChannels {
                serde_json::to_string(&self.encryption_rules(network)?).map_err(|e| e.to_string())?
            } else {
                let prefix = format!("{}/", network.id);
                let rules: BTreeMap<_, _> = self.config.translate.buffers.iter()
                    .filter_map(|(id, rule)| id.strip_prefix(&prefix).map(|target| (target, rule))).collect();
                serde_json::to_string(&rules).map_err(|e| e.to_string())?
            };
            Ok(SettingField {
                path: network.path(scope), label: format!("{}: Channels{}", network.label, if scope == SettingsScope::TranslationChannels { " and queries" } else { "" }),
                section: 0, kind: SettingKind::Json, value, default_value: None, configured: true,
                description: if scope == SettingsScope::EncryptionChannels {
                    "Add a channel, toggle encryption and choose normal, quiet or auto-accept. Removing an entry disables encryption without deleting keys. Encryption takes priority over translation."
                } else {
                    "Add a channel or nickname on this network. Select incoming/outgoing translation and languages. Outgoing needs a channel language. Encrypted conversations are never translated."
                }.into(),
                effect: "Changes take effect when you save this wizard; Cancel discards the draft.".into(),
            })
        }).collect()
    }

    fn normalize_channel_rules<T>(
        &self,
        network: &Network,
        rules: BTreeMap<String, T>,
        channels_only: bool,
    ) -> Result<BTreeMap<String, T>, String> {
        let mapping = self
            .state
            .connections
            .get(&network.id)
            .map_or("rfc1459", |c| c.isupport_parsed.casemapping());
        validate_targets(rules.keys(), channels_only, mapping)?;
        Ok(rules
            .into_iter()
            .map(|(target, rule)| {
                let folded = crate::irc::isupport::casefold(&target, mapping);
                let canonical = self
                    .state
                    .buffers
                    .values()
                    .find(|b| {
                        b.connection_id == network.id
                            && matches!(
                                b.buffer_type,
                                crate::state::buffer::BufferType::Channel
                                    | crate::state::buffer::BufferType::Query
                            )
                            && crate::irc::isupport::casefold(&b.name, mapping) == folded
                    })
                    .map_or(target, |b| b.name.clone());
                (canonical, rule)
            })
            .collect())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn save_scoped_settings(
        &mut self,
        scope: SettingsScope,
        changes: &[SettingChange],
    ) -> Result<(), String> {
        if scope == SettingsScope::General {
            return self.save_settings(changes);
        }
        if changes.is_empty() {
            return Ok(());
        }
        let manager = self.state.e2e_manager.clone();
        let expected = if scope == SettingsScope::EncryptionChannels {
            manager
                .as_ref()
                .ok_or("E2E is not initialized; enable it and restart the main process.")?
                .keyring()
                .list_all_channel_configs()
                .map_err(|e| e.to_string())?
        } else {
            Vec::new()
        };
        let fields = self.scoped_settings_fields(scope)?;
        let networks = self.settings_networks();
        let mut seen = HashSet::new();
        let mut updates = Vec::new();
        let mut translations = self.config.translate.buffers.clone();
        for change in changes {
            if !seen.insert(&change.path) {
                return Err("Duplicate network changes.".into());
            }
            let field = fields
                .iter()
                .find(|f| f.path == change.path)
                .ok_or("Network identity changed. Reopen the wizard before saving.")?;
            if field.value != change.original {
                return Err(
                    "Channel settings changed elsewhere. Reopen the wizard before saving.".into(),
                );
            }
            let network = networks
                .iter()
                .find(|n| n.path(scope) == change.path)
                .ok_or("Unknown network")?;
            if scope == SettingsScope::EncryptionChannels {
                let rules: BTreeMap<String, EncryptionRule> =
                    serde_json::from_str(&change.value)
                        .map_err(|e| format!("Invalid encryption rules: {e}"))?;
                let rules = self.normalize_channel_rules(network, rules, true)?;
                let old: BTreeMap<String, EncryptionRule> =
                    serde_json::from_str(&change.original).map_err(|e| e.to_string())?;
                for (channel, rule) in &rules {
                    let mode = crate::commands::parse_e2e_mode(&rule.mode)?;
                    updates.push(ChannelConfig {
                        channel: crate::e2e::scoped_context(&network.scope, channel),
                        enabled: rule.enabled,
                        mode,
                    });
                }
                for (channel, rule) in old.iter().filter(|(name, _)| !rules.contains_key(*name)) {
                    updates.push(ChannelConfig {
                        channel: crate::e2e::scoped_context(&network.scope, channel),
                        enabled: false,
                        mode: ChannelMode::parse(&rule.mode),
                    });
                }
            } else {
                let rules: BTreeMap<String, crate::config::TranslateBufferConfig> =
                    serde_json::from_str(&change.value)
                        .map_err(|e| format!("Invalid translation rules: {e}"))?;
                let rules = self.normalize_channel_rules(network, rules, false)?;
                for rule in rules.values() {
                    if rule.outgoing && rule.lang.as_ref().is_none_or(|lang| lang.trim().is_empty())
                    {
                        return Err(
                            "Set the channel language before enabling outgoing translation.".into(),
                        );
                    }
                }
                let prefix = format!("{}/", network.id);
                translations.retain(|id, _| !id.starts_with(&prefix));
                for (target, rule) in rules {
                    let id = crate::state::buffer::make_buffer_id(&network.id, &target);
                    if translations.insert(id, rule).is_some() {
                        return Err("Duplicate translation target after buffer-name normalization.".into());
                    }
                }
            }
        }
        if let Some(manager) = manager.filter(|_| scope == SettingsScope::EncryptionChannels) {
            manager
                .keyring()
                .set_channel_configs_if_unchanged(&expected, &updates)
                .map_err(|e| e.to_string())?;
            self.state.push_all_buffer_e2e_statuses();
            Ok(())
        } else {
            let original =
                crate::config::settings::get_config_value(&self.config, "translate.buffers")
                    .ok_or("Missing translation settings")?
                    .value;
            self.save_settings(&[SettingChange {
                path: "translate.buffers".into(),
                original,
                value: serde_json::to_string(&translations).map_err(|e| e.to_string())?,
            }])
        }
    }
}

fn validate_targets<'a>(
    targets: impl Iterator<Item = &'a String>,
    channels_only: bool,
    mapping: &str,
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for target in targets {
        if target.is_empty()
            || target.starts_with(':')
            || target
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == ',')
            || channels_only && !crate::e2e::is_channel_target(target)
        {
            return Err("Use an IRC channel name (such as #chat) or, for translation, a nickname; spaces and control characters are not allowed.".into());
        }
        if !seen.insert(crate::irc::isupport::casefold(target, mapping)) {
            return Err("Duplicate channel or nickname using IRC-equivalent spelling.".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for (id, label) in [("first", "First"), ("second", "Second")] {
            let config = toml::from_str(&format!(
                "label='{label}'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'"
            ))
            .unwrap();
            app.setup_connection(id, &config);
            app.config.servers.insert(id.into(), config);
            app.state.add_buffer(crate::state::buffer::Buffer::for_test(
                id,
                crate::state::buffer::BufferType::Channel,
                "#shared",
            ));
        }
        let keyring = crate::e2e::keyring::Keyring::new(std::sync::Arc::new(
            std::sync::Mutex::new(crate::storage::db::open_database(false).unwrap()),
        ));
        app.state.e2e_manager = Some(std::sync::Arc::new(
            crate::e2e::E2eManager::load_or_init(keyring).unwrap(),
        ));
        app.refresh_e2e_configured_networks();
        app
    }

    fn change(app: &App, scope: SettingsScope, network: &str, value: &str) -> SettingChange {
        let field = app
            .scoped_settings_fields(scope)
            .unwrap()
            .into_iter()
            .find(|f| f.network() == Some(network))
            .unwrap();
        SettingChange {
            path: field.path,
            original: field.value,
            value: value.into(),
        }
    }

    #[test]
    fn encryption_channel_wizard_is_scoped_and_disabling_preserves_keys() {
        let mut app = app();
        let scope = SettingsScope::EncryptionChannels;
        let first = change(
            &app,
            scope,
            "first",
            r##"{"#SHARED":{"enabled":true,"mode":"quiet"}}"##,
        );
        app.save_scoped_settings(scope, &[first]).unwrap();
        let manager = app.state.e2e_manager.clone().unwrap();
        let first_context = crate::e2e::scoped_context("First", "#shared");
        let second_context = crate::e2e::scoped_context("Second", "#shared");
        let first = manager
            .keyring()
            .get_channel_config(&first_context)
            .unwrap()
            .unwrap();
        assert!(first.enabled);
        assert_eq!(first.mode, ChannelMode::Quiet);
        assert!(
            manager
                .keyring()
                .get_channel_config(&second_context)
                .unwrap()
                .is_none()
        );
        manager
            .keyring()
            .set_outgoing_session(&first_context, &[42; 32], 1)
            .unwrap();
        let remove = change(&app, scope, "first", "{}");
        app.save_scoped_settings(scope, &[remove]).unwrap();
        assert!(
            !manager
                .keyring()
                .get_channel_config(&first_context)
                .unwrap()
                .unwrap()
                .enabled
        );
        assert!(
            manager
                .keyring()
                .get_outgoing_session(&first_context)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn encryption_channel_wizard_rejects_invalid_and_stale_batches_without_partial_writes() {
        let mut app = app();
        let scope = SettingsScope::EncryptionChannels;
        let first = change(
            &app,
            scope,
            "first",
            r##"{"#shared":{"enabled":true,"mode":"normal"}}"##,
        );
        let invalid = change(
            &app,
            scope,
            "second",
            r##"{"#shared":{"enabled":true,"mode":"bogus"}}"##,
        );
        assert!(
            app.save_scoped_settings(scope, &[first.clone(), invalid])
                .is_err()
        );
        let manager = app.state.e2e_manager.clone().unwrap();
        assert!(
            manager
                .keyring()
                .list_all_channel_configs()
                .unwrap()
                .is_empty()
        );
        let second = change(
            &app,
            scope,
            "second",
            r##"{"#shared":{"enabled":true,"mode":"quiet"}}"##,
        );
        app.save_scoped_settings(scope, std::slice::from_ref(&second)).unwrap();
        assert!(app.save_scoped_settings(scope, &[first, second]).is_err());
        assert!(
            manager
                .keyring()
                .get_channel_config(&crate::e2e::scoped_context("First", "#shared"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn translation_channel_wizard_separates_networks_and_rejects_missing_outgoing_language() {
        let mut app = app();
        app.config.translate.buffers.insert(
            "first/#shared".into(),
            crate::config::TranslateBufferConfig {
                incoming: true,
                lang: Some("en".into()),
                ..Default::default()
            },
        );
        let fields = app
            .scoped_settings_fields(SettingsScope::TranslationChannels)
            .unwrap();
        assert!(
            fields
                .iter()
                .find(|f| f.network() == Some("first"))
                .unwrap()
                .value
                .contains("#shared")
        );
        assert_eq!(
            fields
                .iter()
                .find(|f| f.network() == Some("second"))
                .unwrap()
                .value,
            "{}"
        );
        let invalid = change(
            &app,
            SettingsScope::TranslationChannels,
            "second",
            r##"{"#shared":{"outgoing":true}}"##,
        );
        assert!(
            app.save_scoped_settings(SettingsScope::TranslationChannels, &[invalid])
                .unwrap_err()
                .contains("channel language")
        );
        assert_eq!(app.config.translate.buffers.len(), 1);
    }
}
