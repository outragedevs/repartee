use std::collections::HashSet;

/// Capabilities we want to request from every server.
pub const DESIRED_CAPS: &[&str] = &[
    "multi-prefix",
    "extended-join",
    "server-time",
    "account-tag",
    "cap-notify",
    "away-notify",
    "account-notify",
    "chghost",
    "echo-message",
    "invite-notify",
    "batch",
    "userhost-in-names",
    "message-tags",
    "sasl",
];

/// Parsed representation of server-advertised capabilities from `CAP LS`.
///
/// Each capability may optionally have a value (e.g. `sasl=PLAIN,EXTERNAL`).
/// Lookups are case-insensitive per the `IRCv3` specification.
#[derive(Debug, Clone, Default)]
pub struct ServerCaps {
    /// Capability name (lowercase) → optional value.
    caps: Vec<(String, Option<String>)>,
}

#[allow(dead_code)]
impl ServerCaps {
    /// Parse a whitespace-delimited capability string from the server.
    ///
    /// Each token is either `capname` or `capname=value`.
    /// Names are stored lowercase for case-insensitive matching.
    #[must_use]
    pub fn parse(caps_str: &str) -> Self {
        let caps = caps_str
            .split_whitespace()
            .map(|token| {
                if let Some((name, value)) = token.split_once('=') {
                    (name.to_ascii_lowercase(), Some(value.to_string()))
                } else {
                    (token.to_ascii_lowercase(), None)
                }
            })
            .collect();
        Self { caps }
    }

    /// Merge additional capabilities from a continuation line.
    pub fn merge(&mut self, caps_str: &str) {
        let other = Self::parse(caps_str);
        self.caps.extend(other.caps);
    }

    /// Check whether the server advertised a given capability (case-insensitive).
    #[must_use]
    pub fn has(&self, cap: &str) -> bool {
        let lower = cap.to_ascii_lowercase();
        self.caps.iter().any(|(name, _)| *name == lower)
    }

    /// Get the value associated with a capability, if any.
    ///
    /// Returns `None` if the capability is absent or has no value.
    #[must_use]
    pub fn value(&self, cap: &str) -> Option<&str> {
        let lower = cap.to_ascii_lowercase();
        self.caps
            .iter()
            .find(|(name, _)| *name == lower)
            .and_then(|(_, v)| v.as_deref())
    }

    /// Return the subset of `desired` capabilities that the server supports.
    #[must_use]
    pub fn negotiate(&self, desired: &[&str]) -> Vec<String> {
        desired
            .iter()
            .filter(|cap| self.has(cap))
            .map(|cap| cap.to_ascii_lowercase())
            .collect()
    }

    /// Parse the SASL mechanisms advertised by the server.
    ///
    /// The `sasl` capability value is a comma-separated list of mechanisms
    /// (e.g. `PLAIN,EXTERNAL`).  If `sasl` is advertised without a value,
    /// returns `["PLAIN"]` as the default.  Returns an empty vec if `sasl`
    /// is not advertised at all.
    #[must_use]
    pub fn sasl_mechanisms(&self) -> Vec<String> {
        if !self.has("sasl") {
            return Vec::new();
        }
        match self.value("sasl") {
            Some(value) if !value.is_empty() => {
                value.split(',').map(str::to_uppercase).collect()
            }
            _ => vec!["PLAIN".to_string()],
        }
    }

    /// Return all advertised capability names as a set.
    #[must_use]
    pub fn all_names(&self) -> HashSet<String> {
        self.caps.iter().map(|(name, _)| name.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_caps() {
        let caps = ServerCaps::parse("multi-prefix away-notify extended-join");
        assert!(caps.has("multi-prefix"));
        assert!(caps.has("away-notify"));
        assert!(caps.has("extended-join"));
        assert!(!caps.has("sasl"));
    }

    #[test]
    fn parse_caps_with_values() {
        let caps = ServerCaps::parse("sasl=PLAIN,EXTERNAL server-time multi-prefix");
        assert!(caps.has("sasl"));
        assert_eq!(caps.value("sasl"), Some("PLAIN,EXTERNAL"));
        assert!(caps.has("server-time"));
        assert_eq!(caps.value("server-time"), None);
    }

    #[test]
    fn negotiate_filters_to_available() {
        let caps = ServerCaps::parse("multi-prefix server-time cap-notify batch");
        let desired = &["multi-prefix", "sasl", "server-time", "echo-message"];
        let result = caps.negotiate(desired);
        assert_eq!(result, vec!["multi-prefix", "server-time"]);
    }

    #[test]
    fn sasl_mechanisms_parsed() {
        let caps = ServerCaps::parse("sasl=PLAIN,EXTERNAL,SCRAM-SHA-256");
        let mechs = caps.sasl_mechanisms();
        assert_eq!(mechs, vec!["PLAIN", "EXTERNAL", "SCRAM-SHA-256"]);
    }

    #[test]
    fn sasl_no_value_means_plain_default() {
        let caps = ServerCaps::parse("sasl multi-prefix");
        let mechs = caps.sasl_mechanisms();
        assert_eq!(mechs, vec!["PLAIN"]);
    }

    #[test]
    fn sasl_not_advertised_returns_empty() {
        let caps = ServerCaps::parse("multi-prefix server-time");
        let mechs = caps.sasl_mechanisms();
        assert!(mechs.is_empty());
    }

    #[test]
    fn empty_caps() {
        let caps = ServerCaps::parse("");
        assert!(!caps.has("anything"));
        assert_eq!(caps.value("anything"), None);
        assert!(caps.negotiate(DESIRED_CAPS).is_empty());
        assert!(caps.sasl_mechanisms().is_empty());
    }

    #[test]
    fn case_insensitive_lookup() {
        let caps = ServerCaps::parse("SASL=PLAIN Multi-Prefix SERVER-TIME");
        assert!(caps.has("sasl"));
        assert!(caps.has("SASL"));
        assert!(caps.has("multi-prefix"));
        assert!(caps.has("Multi-Prefix"));
        assert_eq!(caps.value("SASL"), Some("PLAIN"));
    }

    #[test]
    fn merge_combines_lines() {
        let mut caps = ServerCaps::parse("multi-prefix sasl=PLAIN");
        caps.merge("server-time batch away-notify");
        assert!(caps.has("multi-prefix"));
        assert!(caps.has("sasl"));
        assert!(caps.has("server-time"));
        assert!(caps.has("batch"));
        assert!(caps.has("away-notify"));
        assert_eq!(caps.value("sasl"), Some("PLAIN"));
    }

    #[test]
    fn negotiate_with_full_desired_list() {
        let caps = ServerCaps::parse(
            "multi-prefix extended-join server-time account-tag cap-notify \
             away-notify account-notify chghost echo-message invite-notify \
             batch userhost-in-names message-tags sasl=PLAIN,EXTERNAL",
        );
        let result = caps.negotiate(DESIRED_CAPS);
        // All desired caps should be returned since the server advertises them all
        assert_eq!(result.len(), DESIRED_CAPS.len());
        for cap in DESIRED_CAPS {
            assert!(
                result.contains(&cap.to_ascii_lowercase()),
                "missing cap: {cap}"
            );
        }
    }
}
