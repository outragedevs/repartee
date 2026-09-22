use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

const MAX_EXPANSIONS: usize = 4096;
const MAX_SEQUENCE: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub action: String,
    #[serde(default)]
    pub data: String,
}

impl Binding {
    pub fn new(action: &str, data: &str) -> Self {
        Self {
            action: action.to_ascii_lowercase(),
            data: data.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyboardConfig {
    pub key_timeout: u64,
    pub bindings: BTreeMap<String, Binding>,
    pub disabled: Vec<String>,
}

pub const ACTIONS: &[&str] = &[
    "key",
    "command",
    "multi",
    "nothing",
    "change_window",
    "active_window",
    "previous_window",
    "next_window",
    "backward_character",
    "forward_character",
    "backward_word",
    "forward_word",
    "beginning_of_line",
    "end_of_line",
    "backward_history",
    "forward_history",
    "backspace",
    "delete_character",
    "delete_previous_word",
    "erase_line",
    "erase_to_beg_of_line",
    "erase_to_end_of_line",
    "send_line",
    "word_completion",
    "scroll_backward",
    "scroll_forward",
    "scroll_start",
    "scroll_end",
    "refresh_screen",
    "insert_text",
];

pub fn defaults() -> BTreeMap<String, Binding> {
    let mut bindings = BTreeMap::new();
    for (key, action, data) in [
        ("^[", "key", "meta"),
        ("meta-a", "active_window", ""),
        ("meta-A", "active_window", ""),
        ("meta-left", "previous_window", ""),
        ("meta-right", "next_window", ""),
    ] {
        bindings.insert(key.into(), Binding::new(action, data));
    }
    for digit in 0..=9 {
        bindings.insert(
            format!("meta-{digit}"),
            Binding::new("change_window", &digit.to_string()),
        );
    }
    bindings
}

impl KeyboardConfig {
    pub fn effective(&self) -> BTreeMap<String, Binding> {
        let mut entries = defaults();
        for key in &self.disabled {
            entries.remove(key);
        }
        entries.extend(self.bindings.clone());
        entries
    }

    pub fn set(&mut self, key: &str, binding: Binding) -> Result<(), String> {
        let mut next = self.clone();
        next.disabled.retain(|entry| entry != key);
        next.bindings.insert(key.into(), binding);
        next.compile()?;
        *self = next;
        Ok(())
    }

    pub fn remove(&mut self, key: &str) {
        self.bindings.remove(key);
        if !self.disabled.iter().any(|entry| entry == key) {
            self.disabled.push(key.into());
        }
    }

    pub fn reset(&mut self, key: &str) {
        self.bindings.remove(key);
        self.disabled.retain(|entry| entry != key);
    }

    pub fn compile(&self) -> Result<Keymap, String> {
        let entries = self.effective();
        let mut aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (key, binding) in &entries {
            validate_binding(binding, 0)?;
            if key.is_empty() {
                return Err("A binding needs a key".into());
            }
            if binding.action == "key" {
                aliases
                    .entry(binding.data.clone())
                    .or_default()
                    .push(key.clone());
            }
        }
        let mut executable = BTreeMap::new();
        let mut budget = MAX_EXPANSIONS;
        let mut entries: Vec<_> = entries.into_iter().collect();
        entries.sort_by_key(|(key, _)| self.bindings.contains_key(key));
        for (key, binding) in entries {
            let expanded = expand(&key, &aliases, &mut Vec::new(), &mut budget)?;
            if binding.action != "key" {
                for sequence in expanded {
                    executable.insert(sequence, binding.clone());
                }
            }
        }
        Ok(Keymap {
            executable,
            timeout: self.key_timeout,
        })
    }
}

fn validate_binding(binding: &Binding, depth: usize) -> Result<(), String> {
    if depth > 8 {
        return Err("Nested multi actions exceed the limit".into());
    }
    if !ACTIONS.contains(&binding.action.as_str()) {
        return Err(format!("Unsupported binding action: {}", binding.action));
    }
    match binding.action.as_str() {
        "key"
            if binding.data.is_empty()
                || !binding
                    .data
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_') =>
        {
            return Err("A named key must contain letters, digits or underscores".into());
        }
        "change_window" if binding.data.parse::<usize>().is_err() => {
            return Err("change_window needs a nonnegative window number".into());
        }
        "command"
            if binding.data.trim().is_empty() || binding.data.contains(['\r', '\n', '\0']) =>
        {
            return Err("command needs a single nonempty command line".into());
        }
        "multi" => {
            if binding.data.is_empty() {
                return Err("multi needs actions separated by semicolons".into());
            }
            for item in binding.data.split(';') {
                let (action, data) = item
                    .trim_start()
                    .split_once(' ')
                    .unwrap_or_else(|| (item.trim(), ""));
                let child = Binding::new(action, data);
                if child.action == "key" {
                    return Err("key cannot run inside multi".into());
                }
                validate_binding(&child, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn components(key: &str) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut chars = key.chars().peekable();
    let mut start = true;
    while let Some(ch) = chars.next() {
        match ch {
            '-' if start => {
                result.push("-".into());
                start = false;
            }
            '-' => start = true,
            '^' => {
                match chars.peek().copied() {
                    Some('-') | None => result.push("^".into()),
                    Some(control) if ('@'..='_').contains(&control) || control == '?' => {
                        chars.next();
                        result.push(format!("^{control}"));
                    }
                    _ => return Err("Control keys use uppercase caret notation, such as ^W".into()),
                }
                start = false;
            }
            ch if start && ch.is_ascii_alphabetic() => {
                let mut name = ch.to_string();
                while chars
                    .peek()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                {
                    name.push(chars.next().unwrap());
                }
                result.push(name);
                start = false;
            }
            ch => {
                result.push(ch.to_string());
                start = false;
            }
        }
    }
    if result.is_empty() || result.len() > MAX_SEQUENCE {
        return Err("Invalid key sequence length".into());
    }
    Ok(result)
}

pub fn terminal_tokens(name: &str) -> Vec<String> {
    let encoded = match name {
        "left" => Some("^[[D"),
        "right" => Some("^[[C"),
        "up" => Some("^[[A"),
        "down" => Some("^[[B"),
        "home" => Some("^[[H"),
        "end" => Some("^[[F"),
        "prior" => Some("^[[5~"),
        "next" => Some("^[[6~"),
        "delete" => Some("^[[3~"),
        "stab" => Some("^[[Z"),
        "f1" => Some("^[OP"),
        "f2" => Some("^[OQ"),
        "f3" => Some("^[OR"),
        "f4" => Some("^[OS"),
        "f5" => Some("^[[15~"),
        "f6" => Some("^[[17~"),
        "f7" => Some("^[[18~"),
        "f8" => Some("^[[19~"),
        "f9" => Some("^[[20~"),
        "f10" => Some("^[[21~"),
        "f11" => Some("^[[23~"),
        "f12" => Some("^[[24~"),
        _ => None,
    };
    encoded.map_or_else(
        || vec![name.to_string()],
        |value| {
            components(value)
                .unwrap_or_default()
                .into_iter()
                .flat_map(|part| {
                    if part.starts_with('^') {
                        vec![part]
                    } else {
                        part.chars().map(|ch| ch.to_string()).collect()
                    }
                })
                .collect()
        },
    )
}

fn expand(
    key: &str,
    aliases: &BTreeMap<String, Vec<String>>,
    stack: &mut Vec<String>,
    budget: &mut usize,
) -> Result<Vec<Vec<String>>, String> {
    *budget = budget
        .checked_sub(1)
        .ok_or("Key expansion exceeds the limit")?;
    let mut sequences = vec![Vec::new()];
    for component in components(key)? {
        let alternatives = if let Some(sources) = aliases.get(&component) {
            if stack.contains(&component) || stack.len() >= 32 {
                return Err(format!("Recursive named key: {component}"));
            }
            stack.push(component.clone());
            let mut alternatives = Vec::new();
            for source in sources {
                alternatives.extend(expand(source, aliases, stack, budget)?);
            }
            stack.pop();
            alternatives
        } else {
            let tokens = match component.as_str() {
                "space" => vec![" ".into()],
                "return" => vec!["^M".into()],
                "tab" => vec!["^I".into()],
                "escape" => vec!["^[".into()],
                "left" | "right" | "up" | "down" | "home" | "end" | "prior" | "next"
                | "backspace" | "delete" | "stab" | "f1" | "f2" | "f3" | "f4" | "f5" | "f6"
                | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => terminal_tokens(&component),
                _ if component.starts_with('^') => vec![component],
                _ => component.chars().map(|ch| ch.to_string()).collect(),
            };
            vec![tokens]
        };
        let mut product = Vec::new();
        for prefix in &sequences {
            for suffix in &alternatives {
                if prefix.len() + suffix.len() > MAX_SEQUENCE {
                    return Err("Key sequence exceeds the limit".into());
                }
                *budget = budget
                    .checked_sub(1)
                    .ok_or("Key expansion exceeds the limit")?;
                let mut sequence = prefix.clone();
                sequence.extend(suffix.iter().cloned());
                product.push(sequence);
            }
        }
        sequences = product;
    }
    Ok(sequences)
}

#[derive(Debug, Clone)]
pub struct Keymap {
    executable: BTreeMap<Vec<String>, Binding>,
    timeout: u64,
}

impl Keymap {
    pub fn navigation_only(&self) -> Self {
        let mut map = self.clone();
        map.executable.retain(|_, binding| {
            matches!(
                binding.action.as_str(),
                "change_window" | "active_window" | "previous_window" | "next_window"
            )
        });
        map
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatch {
    Action(Binding),
    Replay(String),
}

#[derive(Debug, Default)]
pub struct Sequence {
    pending: Vec<String>,
    last_input: u64,
}

impl Sequence {
    #[cfg(test)]
    pub const fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn clear(&mut self) {
        self.pending.clear();
    }

    pub fn drain(&mut self) -> Vec<Dispatch> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(Dispatch::Replay)
            .collect()
    }

    pub fn deadline(&self, keymap: &Keymap) -> Option<u64> {
        let timeout = if self.pending == ["^["] {
            500
        } else {
            keymap.timeout
        };
        if self.pending.is_empty() || timeout == 0 {
            None
        } else {
            Some(self.last_input.saturating_add(timeout))
        }
    }

    pub fn expire(&mut self, keymap: &Keymap, now: u64) -> Vec<Dispatch> {
        if self.deadline(keymap).is_none_or(|deadline| now < deadline) {
            return Vec::new();
        }
        if let Some(action) = keymap.executable.get(&self.pending) {
            let action = action.clone();
            self.clear();
            vec![Dispatch::Action(action)]
        } else {
            self.drain()
        }
    }

    #[cfg(test)]
    pub fn feed(&mut self, keymap: &Keymap, token: String, now: u64) -> Vec<Dispatch> {
        self.feed_event(keymap, vec![token], now)
    }

    pub fn feed_event(&mut self, keymap: &Keymap, tokens: Vec<String>, now: u64) -> Vec<Dispatch> {
        let mut output = self.expire(keymap, now);
        self.last_input = now;
        let previous_len = self.pending.len();
        self.pending.extend(tokens.iter().cloned());
        if keymap
            .executable
            .keys()
            .any(|key| key.len() > self.pending.len() && key.starts_with(&self.pending))
        {
            return output;
        }
        if let Some(action) = keymap.executable.get(&self.pending) {
            output.push(Dispatch::Action(action.clone()));
            self.clear();
        } else if previous_len > 0 {
            self.pending.truncate(previous_len);
            output.extend(self.drain());
            output.extend(self.feed_event(keymap, tokens, now));
        } else {
            output.extend(self.drain());
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_complete_hundred_window_article_expands_and_dispatches() {
        let mut config = KeyboardConfig::default();
        for (key, digit) in "qwertyuiop".chars().zip("1234567890".chars()) {
            config
                .set(
                    &format!("meta-{key}"),
                    Binding::new("key", &format!("win{digit}")),
                )
                .unwrap();
        }
        for tens in 0..=9 {
            for units in 0..=9 {
                config
                    .set(
                        &format!("win{tens}-win{units}"),
                        Binding::new("change_window", &format!("{tens}{units}")),
                    )
                    .unwrap();
            }
        }
        let map = config.compile().unwrap();
        for (tens, first) in "pqwertyuio".chars().enumerate() {
            for (units, second) in "pqwertyuio".chars().enumerate() {
                let mut state = Sequence::default();
                for token in ["^[".into(), first.to_string(), "^[".into()] {
                    assert!(state.feed(&map, token, 1).is_empty());
                }
                assert_eq!(
                    state.feed(&map, second.to_string(), 1),
                    vec![Dispatch::Action(Binding::new(
                        "change_window",
                        &format!("{tens}{units}")
                    ))]
                );
            }
        }
    }

    #[test]
    fn ambiguous_prefix_timeout_and_failed_unicode_input_preserve_content() {
        let mut config = KeyboardConfig {
            key_timeout: 100,
            ..KeyboardConfig::default()
        };
        config
            .set("g", Binding::new("insert_text", "short"))
            .unwrap();
        config
            .set("g-g", Binding::new("insert_text", "long"))
            .unwrap();
        let map = config.compile().unwrap();
        let mut state = Sequence::default();
        assert!(state.feed(&map, "g".into(), 0).is_empty());
        assert_eq!(
            state.expire(&map, 100),
            vec![Dispatch::Action(Binding::new("insert_text", "short"))]
        );
        assert!(state.feed(&map, "g".into(), 200).is_empty());
        assert_eq!(
            state.feed(&map, "❤".into(), 201),
            vec![Dispatch::Replay("g".into()), Dispatch::Replay("❤".into())]
        );
        assert!(state.feed(&map, "^[".into(), 300).is_empty());
        assert_eq!(state.expire(&map, 800), vec![Dispatch::Replay("^[".into())]);
    }

    #[test]
    fn deleted_defaults_survive_serialization_and_invalid_edits_are_atomic() {
        let mut config = KeyboardConfig::default();
        config.remove("meta-a");
        let mut config: KeyboardConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert!(!config.effective().contains_key("meta-a"));
        config.set("meta-q", Binding::new("key", "win1")).unwrap();
        let before = config.clone();
        assert!(config.set("win1", Binding::new("key", "win1")).is_err());
        assert_eq!(config, before);
        config.reset("meta-a");
        assert_eq!(config.effective()["meta-a"].action, "active_window");
        assert_eq!(components("^W^C").unwrap(), ["^W", "^C"]);
    }
}
