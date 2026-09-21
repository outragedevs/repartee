mod channels;

use super::App;
use crate::settings_model::SettingChange;

impl App {
    pub fn open_settings(&mut self) {
        self.settings_panel = Some(crate::ui::settings::SettingsPanel::new(&self.config));
    }

    pub fn save_settings(&mut self, changes: &[SettingChange]) -> Result<(), String> {
        let next = crate::config::settings::save_changes(
            &self.config,
            changes,
            &crate::constants::config_path(),
            &crate::constants::env_path(),
        )?;
        self.config = next;
        self.cached_config_toml = None;
        self.inline_previews.invalidate_layout();
        self.state.ignores.clone_from(&self.config.ignores);
        for change in changes {
            if change.path == "image_preview.protocol" {
                self.refresh_image_protocol();
            }
            crate::commands::settings::apply_setting_runtime(self, &change.path, &change.value);
        }
        self.refresh_e2e_configured_networks();
        Ok(())
    }

    pub fn settings_action(&mut self, action: crate::ui::settings::Action) {
        use crate::ui::settings::Action;
        match action {
            Action::None => {}
            Action::Cancel => self.settings_panel = None,
            Action::Channels(scope) => {
                if self.settings_panel.as_ref().is_some_and(|panel| !panel.changes().is_empty()) {
                    if let Some(panel) = &mut self.settings_panel { panel.error = Some("Save or cancel your changes before managing channels.".into()); }
                    return;
                }
                match self.scoped_settings_fields(scope) {
                    Ok(fields) => self.settings_panel = Some(crate::ui::settings::SettingsPanel::from_fields(fields, scope)),
                    Err(error) => if let Some(panel) = &mut self.settings_panel { panel.error = Some(error); },
                }
            }
            Action::Network => {
                if let Some(panel) = &mut self.settings_panel
                    && !panel.changes().is_empty()
                {
                    panel.error =
                        Some("Save or cancel your changes before adding a network.".into());
                    return;
                }
                self.settings_panel = None;
                self.open_server_wizard(None);
            }
            Action::Save => {
                let Some(panel) = &self.settings_panel else {
                    return;
                };
                let changes = panel.changes();
                match self.save_scoped_settings(panel.scope, &changes) {
                    Ok(()) => self.settings_panel = None,
                    Err(error) => {
                        if let Some(panel) = &mut self.settings_panel {
                            panel.error = Some(error);
                        }
                    }
                }
            }
        }
    }
}
