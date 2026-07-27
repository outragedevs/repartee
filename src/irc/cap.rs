use std::collections::HashSet;

/// Capabilities we want to request from every server.
pub const DESIRED_CAPS: &[&str] = &[
    "multi-prefix",
    "extended-join",
    // IRCnet ircd 2.12.0 extension — takes precedence over extended-join
    // when both are acked; JOIN then carries <uid> <ip> <netjoin> too.
    "ircnet.com/extended-join",
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
    "draft/multiline",
    "draft/chathistory",
    "draft/event-playback",
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
        self.caps
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(cap))
    }

    /// Get the value associated with a capability, if any.
    ///
    /// Returns `None` if the capability is absent or has no value.
    #[must_use]
    pub fn value(&self, cap: &str) -> Option<&str> {
        self.caps
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(cap))
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
    ///
    /// Prefer [`Self::sasl_mechanisms_advertised`] when the difference between
    /// "the server offers only PLAIN" and "the server named no mechanisms"
    /// matters — this accessor flattens the two together.
    #[must_use]
    pub fn sasl_mechanisms(&self) -> Vec<String> {
        if !self.has("sasl") {
            return Vec::new();
        }
        self.sasl_mechanisms_advertised()
            .unwrap_or_else(|| vec!["PLAIN".to_string()])
    }

    /// The SASL mechanism list the server actually named, if it named one.
    ///
    /// `Some(list)` when the `sasl` cap carries a value; `None` when `sasl` is
    /// advertised bare **or** not advertised at all — in both cases the server
    /// has told us nothing about which mechanisms it speaks. Callers that need
    /// to distinguish those two check [`Self::has`] first.
    ///
    /// This distinction is load-bearing: a bare `sasl` used to be reported as
    /// `["PLAIN"]`, which is indistinguishable from a server that genuinely
    /// offers only PLAIN, and it caused a configured `SCRAM-SHA-512` to be
    /// dropped instead of attempted.
    #[must_use]
    pub fn sasl_mechanisms_advertised(&self) -> Option<Vec<String>> {
        match self.value("sasl") {
            Some(value) if !value.is_empty() => {
                Some(value.split(',').map(str::to_uppercase).collect())
            }
            _ => None,
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
    fn advertised_list_distinguishes_bare_sasl_from_a_plain_only_server() {
        // A server that names its mechanisms.
        let named = ServerCaps::parse("sasl=PLAIN,SCRAM-SHA-512");
        assert_eq!(
            named.sasl_mechanisms_advertised(),
            Some(vec!["PLAIN".to_string(), "SCRAM-SHA-512".to_string()])
        );

        // A server that offers only PLAIN — a real, closed list.
        let plain_only = ServerCaps::parse("sasl=PLAIN");
        assert_eq!(
            plain_only.sasl_mechanisms_advertised(),
            Some(vec!["PLAIN".to_string()])
        );

        // A server that advertises sasl without saying what it speaks. The
        // flattened accessor guesses PLAIN; this one admits it does not know,
        // which is what lets a configured mechanism still be attempted.
        let bare = ServerCaps::parse("sasl multi-prefix");
        assert!(bare.has("sasl"));
        assert_eq!(bare.sasl_mechanisms_advertised(), None);
        assert_eq!(bare.sasl_mechanisms(), vec!["PLAIN".to_string()]);

        // No sasl at all.
        let absent = ServerCaps::parse("multi-prefix");
        assert!(!absent.has("sasl"));
        assert_eq!(absent.sasl_mechanisms_advertised(), None);
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
            "multi-prefix extended-join ircnet.com/extended-join server-time \
             account-tag cap-notify away-notify account-notify chghost \
             echo-message invite-notify batch userhost-in-names message-tags \
             draft/multiline draft/chathistory draft/event-playback \
             sasl=PLAIN,EXTERNAL",
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

    #[test]
    fn desired_caps_include_both_extended_join_variants() {
        // IRCnet ircd 2.12.0 sends its own extended JOIN format instead of the
        // IRCv3 one when both caps are acked — we must request both.
        assert!(DESIRED_CAPS.contains(&"extended-join"));
        assert!(DESIRED_CAPS.contains(&"ircnet.com/extended-join"));
    }

    #[test]
    fn desired_caps_include_chathistory() {
        assert!(DESIRED_CAPS.contains(&"draft/chathistory"));
        assert!(DESIRED_CAPS.contains(&"draft/event-playback"));
        // chathistory requires batch
        assert!(DESIRED_CAPS.contains(&"batch"));
    }

    #[test]
    fn desired_caps_include_multiline_prereqs() {
        assert!(DESIRED_CAPS.contains(&"draft/multiline"));
        // multiline requires batch + message-tags
        assert!(DESIRED_CAPS.contains(&"batch"));
        assert!(DESIRED_CAPS.contains(&"message-tags"));
    }

    #[test]
    fn server_caps_retains_multiline_value() {
        // split_once('=') splits on the FIRST '=', so the whole RHS is kept.
        let caps = ServerCaps::parse("draft/multiline=max-bytes=4096,max-lines=24 batch");
        assert_eq!(
            caps.value("draft/multiline"),
            Some("max-bytes=4096,max-lines=24")
        );
    }
}
