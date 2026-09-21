use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Text,
    Number,
    Toggle,
    List,
    OptionalText,
    Select(&'static [&'static str]),
}

#[derive(Clone, Debug)]
pub struct Column {
    pub path: &'static str,
    pub label: &'static str,
    pub kind: CellKind,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub key: String,
    pub value: Value,
}

#[derive(Clone, Debug)]
pub struct Collection {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    pub key_label: Option<&'static str>,
    pub optional: bool,
    pub inherited: bool,
    delimited: bool,
    template: Value,
}

impl Collection {
    #[allow(clippy::too_many_lines)]
    pub fn open(path: &str, raw: &str) -> Result<Self, String> {
        use CellKind::{List, Number, OptionalText, Text, Toggle};
        let column = |path, label, kind| Column { path, label, kind };
        let delimited = matches!(
            path,
            "dcc.autochat_masks" | "general.flood_exemptions" | "spellcheck.languages"
        ) || path.starts_with("servers.") && path.ends_with(".channels");
        let (key_label, columns, template) = match path {
            "aliases" => (
                Some("Command"),
                vec![column("", "Expansion", Text)],
                json!(""),
            ),
            "statusbar.item_formats" => (Some("Item"), vec![column("", "Format", Text)], json!("")),
            "ignores" => (
                None,
                vec![
                    column("/mask", "Mask", Text),
                    column("/levels", "Levels (one per line)", List),
                    column("/channels", "Channels (one per line)", List),
                ],
                json!({"mask":"", "levels":["ALL"]}),
            ),
            path if path.contains(".encryption_channels_") => (
                Some("Channel"),
                vec![column("/enabled", "Encryption enabled", Toggle), column("/mode", "Key-sharing mode", CellKind::Select(&["normal", "quiet", "auto-accept"]))],
                json!({"enabled": true, "mode": "normal"}),
            ),
            path if path == "translate.buffers" || path.contains(".translation_channels_") => (
                Some(if path == "translate.buffers" { "Buffer (network/target)" } else { "Channel or nickname" }),
                vec![
                    column("/incoming", "Translate incoming", Toggle),
                    column("/outgoing", "Translate outgoing", Toggle),
                    column("/lang", "Channel language", OptionalText),
                    column("/my_lang", "Your language", OptionalText),
                ],
                json!({"incoming":false,"outgoing":false}),
            ),
            "translate.ai.models" => (
                None,
                vec![
                    column("/name", "Name", Text),
                    column("/base_url", "Base URL", Text),
                    column("/model", "Model", Text),
                    column("/api_key_env", "API key environment variable", Text),
                    column("/rpm", "Requests per minute", Number),
                    column("/tpm", "Tokens per minute", Number),
                    column("/max_retries", "Maximum retries", Number),
                    column("/max_output_tokens", "Maximum output tokens", Number),
                    column("/reasoning_effort", "Reasoning effort", OptionalText),
                    column("/provider/only", "Providers (one per line)", List),
                    column(
                        "/provider/quantizations",
                        "Quantizations (one per line)",
                        List,
                    ),
                    column(
                        "/provider/allow_fallbacks",
                        "Allow provider fallbacks",
                        Toggle,
                    ),
                ],
                json!({"name":"","base_url":"","model":"","api_key_env":"","rpm":0,"tpm":0,"max_retries":2,"max_output_tokens":256}),
            ),
            _ => (None, vec![column("", "Value", Text)], json!("")),
        };
        let value: Value = if delimited {
            Value::Array(
                raw.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| json!(s))
                    .collect(),
            )
        } else {
            serde_json::from_str(raw).map_err(|e| format!("Cannot read collection: {e}"))?
        };
        let optional = path == "translate.ai.terminal";
        let inherited = optional && value.is_null();
        let rows = if inherited {
            Vec::new()
        } else if key_label.is_some() {
            value
                .as_object()
                .ok_or("Expected a table")?
                .iter()
                .map(|(key, value)| Row {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect()
        } else {
            value
                .as_array()
                .ok_or("Expected a list")?
                .iter()
                .map(|value| Row {
                    key: String::new(),
                    value: value.clone(),
                })
                .collect()
        };
        Ok(Self {
            columns,
            rows,
            key_label,
            optional,
            inherited,
            delimited,
            template,
        })
    }

    pub const fn ordered(&self) -> bool {
        self.key_label.is_none()
    }

    pub fn new_row(&self) -> Row {
        Row {
            key: String::new(),
            value: self.template.clone(),
        }
    }

    pub fn cells(&self, row: &Row) -> Vec<String> {
        self.columns
            .iter()
            .map(|column| {
                let value = row.value.pointer(column.path).unwrap_or(&Value::Null);
                match column.kind {
                    CellKind::List => value
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default(),
                    CellKind::Toggle => value.as_bool().unwrap_or(false).to_string(),
                    _ => value.as_str().map_or_else(
                        || {
                            if value.is_null() {
                                String::new()
                            } else {
                                value.to_string()
                            }
                        },
                        str::to_string,
                    ),
                }
            })
            .collect()
    }

    pub fn update_row(
        &mut self,
        index: Option<usize>,
        key: String,
        cells: &[String],
    ) -> Result<(), String> {
        if self.key_label.is_some()
            && (key.trim().is_empty()
                || self
                    .rows
                    .iter()
                    .enumerate()
                    .any(|(i, row)| Some(i) != index && row.key == key))
        {
            return Err("Use a nonempty, unique name.".into());
        }
        if cells.len() != self.columns.len() {
            return Err("Incomplete entry.".into());
        }
        let mut row = index
            .and_then(|i| self.rows.get(i))
            .cloned()
            .unwrap_or_else(|| self.new_row());
        let before = self.cells(&row);
        row.key = key;
        for ((column, text), original) in self.columns.iter().zip(cells).zip(before) {
            if text == &original {
                continue;
            }
            let value = match column.kind {
                CellKind::Select(options) => {
                    if !options.contains(&text.as_str()) { return Err(format!("Invalid {}", column.label)); }
                    json!(text)
                }
                CellKind::Text => json!(text),
                CellKind::OptionalText => {
                    if text.is_empty() {
                        Value::Null
                    } else {
                        json!(text)
                    }
                }
                CellKind::Number => json!(text.parse::<u32>().map_err(|_| format!(
                    "{} must be a whole number from 0 to {}.",
                    column.label,
                    u32::MAX
                ))?),
                CellKind::Toggle => json!(text == "true"),
                CellKind::List => Value::Array(
                    text.lines()
                        .filter(|line| !line.is_empty())
                        .map(|line| json!(line))
                        .collect(),
                ),
            };
            set_cell(&mut row.value, column.path, value);
        }
        if self.delimited
            && row
                .value
                .as_str()
                .is_some_and(|s| s.contains(',') || s.contains('\n'))
        {
            return Err(
                "Add each entry separately; commas and line breaks separate entries.".into(),
            );
        }
        if let Some(index) = index {
            *self.rows.get_mut(index).ok_or("Entry no longer exists")? = row;
        } else {
            self.rows.push(row);
        }
        self.inherited = false;
        Ok(())
    }

    pub fn serialize(&self) -> String {
        if self.inherited {
            return "null".into();
        }
        if self.delimited {
            return self
                .rows
                .iter()
                .filter_map(|row| row.value.as_str())
                .collect::<Vec<_>>()
                .join(", ");
        }
        if self.key_label.is_some() {
            Value::Object(
                self.rows
                    .iter()
                    .map(|row| (row.key.clone(), row.value.clone()))
                    .collect(),
            )
            .to_string()
        } else {
            Value::Array(self.rows.iter().map(|row| row.value.clone()).collect()).to_string()
        }
    }

    pub fn summary(&self, row: &Row) -> String {
        if self.key_label.is_some() {
            if self.columns.first().is_some_and(|c| c.path == "/enabled") {
                return format!("{} · {} · {}", row.key,
                    if row.value["enabled"].as_bool().unwrap_or(false) { "on" } else { "off" },
                    row.value["mode"].as_str().unwrap_or("normal"));
            }
            if self.columns.first().is_some_and(|c| c.path == "/incoming") {
                return format!("{} · in:{} out:{} · {}", row.key,
                    if row.value["incoming"].as_bool().unwrap_or(false) { "on" } else { "off" },
                    if row.value["outgoing"].as_bool().unwrap_or(false) { "on" } else { "off" },
                    row.value["lang"].as_str().unwrap_or("auto"));
            }
            return row.key.clone();
        }
        self.cells(row).first().cloned().unwrap_or_default()
    }
}

fn set_cell(target: &mut Value, path: &str, value: Value) {
    if path.is_empty() {
        *target = value;
        return;
    }
    let mut keys = path.trim_start_matches('/').split('/').peekable();
    let mut target = target;
    while let Some(key) = keys.next() {
        if !target.is_object() {
            *target = json!({});
        }
        if keys.peek().is_none() {
            target[key] = value;
            return;
        }
        target = &mut target[key];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_terminal_selection_remains_distinct_from_an_empty_list() {
        let mut automatic = Collection::open("translate.ai.terminal", "null").unwrap();
        assert!(automatic.inherited);
        assert_eq!(automatic.serialize(), "null");
        automatic.inherited = false;
        assert_eq!(automatic.serialize(), "[]");
        automatic.inherited = true;
        automatic
            .update_row(None, String::new(), &["local-model".into()])
            .unwrap();
        assert_eq!(automatic.serialize(), r#"["local-model"]"#);
    }

    #[test]
    fn editing_model_preserves_unexposed_fields_and_optional_provider() {
        let source =
            json!([{"name":"local","rpm":30,"provider":null,"future_option":{"enabled":true}}]);
        let mut collection = Collection::open("translate.ai.models", &source.to_string()).unwrap();
        let mut cells = collection.cells(&collection.rows[0]);
        cells[0] = "renamed".into();
        collection
            .update_row(Some(0), String::new(), &cells)
            .unwrap();
        let saved: Value = serde_json::from_str(&collection.serialize()).unwrap();
        assert_eq!(saved[0]["provider"], Value::Null);
        assert_eq!(saved[0]["future_option"], source[0]["future_option"]);
        assert_eq!(saved[0]["name"], "renamed");
        assert_eq!(saved[0]["rpm"], 30);
    }

    #[test]
    fn duplicate_map_key_and_invalid_number_leave_collection_unchanged() {
        let mut aliases = Collection::open("aliases", r#"{"a":"one","b":"two"}"#).unwrap();
        let before = aliases.serialize();
        assert!(
            aliases
                .update_row(Some(0), "b".into(), &["changed".into()])
                .is_err()
        );
        assert_eq!(aliases.serialize(), before);
        let mut models = Collection::open("translate.ai.models", "[]").unwrap();
        let mut cells = models.cells(&models.new_row());
        cells[0] = "changed".into();
        cells[4] = "-1".into();
        assert!(models.update_row(None, String::new(), &cells).is_err());
        assert_eq!(models.serialize(), "[]");
    }

    #[test]
    fn list_order_and_literal_format_characters_survive_editing() {
        let mut formats =
            Collection::open("statusbar.item_formats", r#"{"time":"%Zff9900$0%N "}"#).unwrap();
        let cells = formats.cells(&formats.rows[0]);
        formats.update_row(Some(0), "clock".into(), &cells).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&formats.serialize()).unwrap()["clock"],
            "%Zff9900$0%N "
        );
        let mut items = Collection::open("statusbar.items", r#"["nick_info","time"]"#).unwrap();
        items.rows.swap(0, 1);
        assert_eq!(items.serialize(), r#"["time","nick_info"]"#);
    }
}
