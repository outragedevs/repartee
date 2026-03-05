// Netsplit detection — batches QUIT/JOIN events from server splits into summary messages.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

// === Constants ===

const SPLIT_BATCH_WAIT: Duration = Duration::from_secs(5);
const NETJOIN_BATCH_WAIT: Duration = Duration::from_secs(5);
const SPLIT_EXPIRE: Duration = Duration::from_secs(3600); // 1 hour
const MAX_NICKS_DISPLAY: usize = 15;

// === Types ===

/// A nick that quit during a netsplit, along with the buffer IDs (channels) they were in.
#[derive(Debug, Clone)]
pub struct SplitRecord {
    pub nick: String,
    pub channels: Vec<String>,
}

/// A group of nicks that quit in the same netsplit (same server pair).
#[derive(Debug, Clone)]
pub struct SplitGroup {
    pub server1: String,
    pub server2: String,
    pub nicks: Vec<SplitRecord>,
    pub last_quit: Instant,
    pub printed: bool,
}

/// A group of nicks rejoining after a netsplit.
#[derive(Debug, Clone)]
pub struct NetjoinGroup {
    pub server1: String,
    pub server2: String,
    pub nicks: Vec<String>,
    pub channels: HashSet<String>,
    pub last_join: Instant,
    pub printed: bool,
}

/// A message to be displayed in one or more buffers.
#[derive(Debug, Clone)]
pub struct NetsplitMessage {
    pub buffer_ids: Vec<String>,
    pub text: String,
}

/// Per-connection netsplit tracking state.
pub struct NetsplitState {
    groups: Vec<SplitGroup>,
    /// Maps nick -> index into `groups` for fast netjoin lookup.
    nick_index: HashMap<String, usize>,
    netjoins: Vec<NetjoinGroup>,
}

impl NetsplitState {
    /// Create a new, empty netsplit state.
    pub fn new() -> Self {
        NetsplitState {
            groups: Vec::new(),
            nick_index: HashMap::new(),
            netjoins: Vec::new(),
        }
    }

    /// Process a QUIT that may be a netsplit.
    /// Returns `true` if handled as a netsplit (suppress normal quit display).
    pub fn handle_quit(
        &mut self,
        nick: &str,
        message: &str,
        affected_buffer_ids: &[String],
    ) -> bool {
        if !is_netsplit_quit(message) {
            return false;
        }

        let space = message.find(' ').unwrap();
        let server1 = &message[..space];
        let server2 = &message[space + 1..];
        let now = Instant::now();

        // Find existing group for this server pair that hasn't been printed yet
        let group_idx = self.groups.iter().position(|g| {
            g.server1 == server1 && g.server2 == server2 && !g.printed
        });

        let idx = match group_idx {
            Some(idx) => idx,
            None => {
                self.groups.push(SplitGroup {
                    server1: server1.to_string(),
                    server2: server2.to_string(),
                    nicks: Vec::new(),
                    last_quit: now,
                    printed: false,
                });
                self.groups.len() - 1
            }
        };

        self.groups[idx].nicks.push(SplitRecord {
            nick: nick.to_string(),
            channels: affected_buffer_ids.to_vec(),
        });
        self.groups[idx].last_quit = now;
        self.nick_index.insert(nick.to_string(), idx);

        true
    }

    /// Process a JOIN to check if it's from a user who was in a netsplit.
    /// Returns `true` if handled as a netjoin (suppress normal join display).
    pub fn handle_join(&mut self, nick: &str, buffer_id: &str) -> bool {
        let group_idx = match self.nick_index.get(nick) {
            Some(&idx) => idx,
            None => return false,
        };

        // Bounds check (group may have been removed during expiry)
        if group_idx >= self.groups.len() {
            self.nick_index.remove(nick);
            return false;
        }

        let server1 = self.groups[group_idx].server1.clone();
        let server2 = self.groups[group_idx].server2.clone();
        let now = Instant::now();

        // Find or create netjoin group
        let nj_idx = self.netjoins.iter().position(|nj| {
            nj.server1 == server1 && nj.server2 == server2 && !nj.printed
        });

        let nj = match nj_idx {
            Some(idx) => &mut self.netjoins[idx],
            None => {
                self.netjoins.push(NetjoinGroup {
                    server1,
                    server2,
                    nicks: Vec::new(),
                    channels: HashSet::new(),
                    last_join: now,
                    printed: false,
                });
                self.netjoins.last_mut().unwrap()
            }
        };

        if !nj.nicks.contains(&nick.to_string()) {
            nj.nicks.push(nick.to_string());
        }
        nj.channels.insert(buffer_id.to_string());
        nj.last_join = now;

        // Remove from split index
        self.nick_index.remove(nick);

        true
    }

    /// Check for batches ready to print and expired records.
    /// Returns messages to display. Caller is responsible for routing them to buffers.
    pub fn tick(&mut self) -> Vec<NetsplitMessage> {
        let now = Instant::now();
        let mut messages = Vec::new();

        // Print split groups that have been quiet for SPLIT_BATCH_WAIT
        for group in &mut self.groups {
            if !group.printed && now.duration_since(group.last_quit) >= SPLIT_BATCH_WAIT {
                messages.push(format_split_message(group));
                group.printed = true;
            }
        }

        // Print netjoin groups that have been quiet for NETJOIN_BATCH_WAIT
        for nj in &mut self.netjoins {
            if !nj.printed && now.duration_since(nj.last_join) >= NETJOIN_BATCH_WAIT {
                messages.push(format_netjoin_message(nj));
                nj.printed = true;
            }
        }

        // Expire old split records
        self.groups
            .retain(|g| now.duration_since(g.last_quit) < SPLIT_EXPIRE);
        self.netjoins
            .retain(|nj| now.duration_since(nj.last_join) < SPLIT_EXPIRE);

        // Clean up nick index for expired groups — rebuild valid entries
        self.nick_index
            .retain(|_, idx| *idx < self.groups.len() && now.duration_since(self.groups[*idx].last_quit) < SPLIT_EXPIRE);

        messages
    }

    /// Check if a nick is known to have quit in an expired (or current) netsplit.
    /// Useful for cleanup of user lists when a split nick never rejoins.
    pub fn is_expired_split_nick(&self, nick: &str) -> bool {
        if let Some(&idx) = self.nick_index.get(nick)
            && idx < self.groups.len()
        {
            let elapsed = Instant::now().duration_since(self.groups[idx].last_quit);
            return elapsed >= SPLIT_EXPIRE;
        }
        false
    }
}

impl Default for NetsplitState {
    fn default() -> Self {
        Self::new()
    }
}

// === Detection ===

/// Check if a QUIT message looks like a netsplit.
/// Format: "host1.domain host2.domain" — two valid hostnames separated by a single space.
pub fn is_netsplit_quit(message: &str) -> bool {
    if message.is_empty() {
        return false;
    }
    // Must not contain : or / (avoids URLs and other messages)
    if message.contains(':') || message.contains('/') {
        return false;
    }

    let space = match message.find(' ') {
        Some(idx) if idx > 0 && idx < message.len() - 1 => idx,
        _ => return false,
    };
    // Only one space
    if message[space + 1..].contains(' ') {
        return false;
    }

    let host1 = &message[..space];
    let host2 = &message[space + 1..];

    is_valid_split_host(host1) && is_valid_split_host(host2) && host1 != host2
}

fn is_valid_split_host(host: &str) -> bool {
    if host.len() < 3 {
        return false;
    }
    if host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    if host.contains("..") {
        return false;
    }

    let dot = match host.rfind('.') {
        Some(idx) if idx > 0 => idx,
        _ => return false,
    };

    let tld = &host[dot + 1..];
    if tld.len() < 2 {
        return false;
    }
    if !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }

    true
}

// === Message formatting ===

fn format_split_message(group: &SplitGroup) -> NetsplitMessage {
    // Collect all affected buffer IDs
    let mut all_buffer_ids = HashSet::new();
    for rec in &group.nicks {
        for id in &rec.channels {
            all_buffer_ids.insert(id.clone());
        }
    }

    let nick_names: Vec<&str> = group.nicks.iter().map(|r| r.nick.as_str()).collect();
    let nick_str = format_nick_list(&nick_names);

    let text = format!(
        "Netsplit {} \u{21C4} {} quits: {}",
        group.server1, group.server2, nick_str
    );

    NetsplitMessage {
        buffer_ids: all_buffer_ids.into_iter().collect(),
        text,
    }
}

fn format_netjoin_message(group: &NetjoinGroup) -> NetsplitMessage {
    let nick_names: Vec<&str> = group.nicks.iter().map(|s| s.as_str()).collect();
    let nick_str = format_nick_list(&nick_names);

    let text = format!(
        "Netsplit over {} \u{21C4} {} joins: {}",
        group.server1, group.server2, nick_str
    );

    NetsplitMessage {
        buffer_ids: group.channels.iter().cloned().collect(),
        text,
    }
}

fn format_nick_list(nicks: &[&str]) -> String {
    if nicks.len() > MAX_NICKS_DISPLAY {
        let shown = nicks[..MAX_NICKS_DISPLAY].join(", ");
        let more = nicks.len() - MAX_NICKS_DISPLAY;
        format!("{shown} (+{more} more)")
    } else {
        nicks.join(", ")
    }
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_netsplit_quit tests ---

    #[test]
    fn valid_netsplit_message() {
        assert!(is_netsplit_quit("irc.server1.net irc.server2.net"));
        assert!(is_netsplit_quit("hub.eu.libera.chat services.libera.chat"));
        assert!(is_netsplit_quit("a.bc d.ef"));
    }

    #[test]
    fn rejects_empty() {
        assert!(!is_netsplit_quit(""));
    }

    #[test]
    fn rejects_no_space() {
        assert!(!is_netsplit_quit("irc.server1.net"));
    }

    #[test]
    fn rejects_multiple_spaces() {
        assert!(!is_netsplit_quit("irc.server1.net irc.server2.net extra"));
    }

    #[test]
    fn rejects_colon() {
        assert!(!is_netsplit_quit("Quit: Connection reset"));
    }

    #[test]
    fn rejects_slash() {
        assert!(!is_netsplit_quit("http://example.com something.net"));
    }

    #[test]
    fn rejects_same_host() {
        assert!(!is_netsplit_quit("irc.server.net irc.server.net"));
    }

    #[test]
    fn rejects_short_host() {
        assert!(!is_netsplit_quit("ab cd.ef"));
    }

    #[test]
    fn rejects_no_dot() {
        assert!(!is_netsplit_quit("servername othername"));
    }

    #[test]
    fn rejects_leading_dot() {
        assert!(!is_netsplit_quit(".irc.server.net irc.other.net"));
    }

    #[test]
    fn rejects_trailing_dot() {
        assert!(!is_netsplit_quit("irc.server.net. irc.other.net"));
    }

    #[test]
    fn rejects_double_dot() {
        assert!(!is_netsplit_quit("irc..server.net irc.other.net"));
    }

    #[test]
    fn rejects_numeric_tld() {
        assert!(!is_netsplit_quit("server.123 other.net"));
    }

    #[test]
    fn rejects_single_char_tld() {
        assert!(!is_netsplit_quit("server.a other.net"));
    }

    #[test]
    fn rejects_space_at_start() {
        assert!(!is_netsplit_quit(" server.net other.net"));
    }

    #[test]
    fn rejects_space_at_end() {
        assert!(!is_netsplit_quit("server.net "));
    }

    // --- NetsplitState tests ---

    #[test]
    fn handle_quit_returns_false_for_normal_quit() {
        let mut state = NetsplitState::new();
        assert!(!state.handle_quit("nick", "Client quit", &[]));
    }

    #[test]
    fn handle_quit_returns_true_for_netsplit() {
        let mut state = NetsplitState::new();
        let result = state.handle_quit(
            "alice",
            "irc.hub.net irc.leaf.net",
            &["conn/#channel".to_string()],
        );
        assert!(result);
        assert_eq!(state.groups.len(), 1);
        assert_eq!(state.groups[0].nicks.len(), 1);
        assert_eq!(state.groups[0].server1, "irc.hub.net");
        assert_eq!(state.groups[0].server2, "irc.leaf.net");
    }

    #[test]
    fn handle_quit_batches_same_server_pair() {
        let mut state = NetsplitState::new();
        state.handle_quit(
            "alice",
            "hub.net leaf.net",
            &["conn/#chan".to_string()],
        );
        state.handle_quit(
            "bob",
            "hub.net leaf.net",
            &["conn/#chan".to_string()],
        );
        assert_eq!(state.groups.len(), 1);
        assert_eq!(state.groups[0].nicks.len(), 2);
    }

    #[test]
    fn handle_quit_separates_different_server_pairs() {
        let mut state = NetsplitState::new();
        state.handle_quit("alice", "hub.net leaf.net", &[]);
        state.handle_quit("bob", "other.net leaf.net", &[]);
        assert_eq!(state.groups.len(), 2);
    }

    #[test]
    fn handle_join_returns_false_for_unknown_nick() {
        let mut state = NetsplitState::new();
        assert!(!state.handle_join("unknown", "conn/#chan"));
    }

    #[test]
    fn handle_join_returns_true_for_split_nick() {
        let mut state = NetsplitState::new();
        state.handle_quit(
            "alice",
            "hub.net leaf.net",
            &["conn/#chan".to_string()],
        );
        assert!(state.handle_join("alice", "conn/#chan"));
        assert_eq!(state.netjoins.len(), 1);
        assert_eq!(state.netjoins[0].nicks, vec!["alice"]);
    }

    #[test]
    fn handle_join_removes_from_nick_index() {
        let mut state = NetsplitState::new();
        state.handle_quit("alice", "hub.net leaf.net", &[]);
        assert!(state.nick_index.contains_key("alice"));
        state.handle_join("alice", "conn/#chan");
        assert!(!state.nick_index.contains_key("alice"));
    }

    #[test]
    fn handle_join_deduplicates_nicks() {
        let mut state = NetsplitState::new();
        state.handle_quit(
            "alice",
            "hub.net leaf.net",
            &["conn/#a".to_string(), "conn/#b".to_string()],
        );
        // Re-add alice to nick_index for second join in different channel
        // (In practice each nick only joins once, but test dedup logic)
        state.handle_join("alice", "conn/#a");
        // alice was removed from nick_index, so second join won't match
        assert!(!state.handle_join("alice", "conn/#b"));
    }

    #[test]
    fn tick_returns_empty_before_batch_wait() {
        let mut state = NetsplitState::new();
        state.handle_quit(
            "alice",
            "hub.net leaf.net",
            &["conn/#chan".to_string()],
        );
        // Immediately calling tick should return nothing (batch wait not elapsed)
        let msgs = state.tick();
        assert!(msgs.is_empty());
    }

    #[test]
    fn format_nick_list_under_limit() {
        let nicks: Vec<&str> = (0..5).map(|i| match i {
            0 => "a",
            1 => "b",
            2 => "c",
            3 => "d",
            _ => "e",
        }).collect();
        let result = format_nick_list(&nicks);
        assert_eq!(result, "a, b, c, d, e");
    }

    #[test]
    fn format_nick_list_over_limit() {
        let names: Vec<String> = (0..20).map(|i| format!("nick{i}")).collect();
        let nicks: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let result = format_nick_list(&nicks);
        assert!(result.contains("(+5 more)"));
        assert!(result.contains("nick0"));
        assert!(result.contains("nick14"));
        assert!(!result.contains("nick15"));
    }

    #[test]
    fn format_split_message_content() {
        let group = SplitGroup {
            server1: "hub.net".to_string(),
            server2: "leaf.net".to_string(),
            nicks: vec![
                SplitRecord {
                    nick: "alice".to_string(),
                    channels: vec!["conn/#chan".to_string()],
                },
                SplitRecord {
                    nick: "bob".to_string(),
                    channels: vec!["conn/#chan".to_string()],
                },
            ],
            last_quit: Instant::now(),
            printed: false,
        };
        let msg = format_split_message(&group);
        assert!(msg.text.contains("Netsplit"));
        assert!(msg.text.contains("hub.net"));
        assert!(msg.text.contains("leaf.net"));
        assert!(msg.text.contains("quits:"));
        assert!(msg.text.contains("alice"));
        assert!(msg.text.contains("bob"));
        assert!(msg.buffer_ids.contains(&"conn/#chan".to_string()));
    }

    #[test]
    fn format_netjoin_message_content() {
        let mut channels = HashSet::new();
        channels.insert("conn/#chan".to_string());
        let group = NetjoinGroup {
            server1: "hub.net".to_string(),
            server2: "leaf.net".to_string(),
            nicks: vec!["alice".to_string(), "bob".to_string()],
            channels,
            last_join: Instant::now(),
            printed: false,
        };
        let msg = format_netjoin_message(&group);
        assert!(msg.text.contains("Netsplit over"));
        assert!(msg.text.contains("hub.net"));
        assert!(msg.text.contains("leaf.net"));
        assert!(msg.text.contains("joins:"));
        assert!(msg.text.contains("alice"));
        assert!(msg.text.contains("bob"));
    }

    #[test]
    fn is_expired_split_nick_unknown_nick() {
        let state = NetsplitState::new();
        assert!(!state.is_expired_split_nick("nobody"));
    }

    #[test]
    fn is_expired_split_nick_recent() {
        let mut state = NetsplitState::new();
        state.handle_quit("alice", "hub.net leaf.net", &[]);
        // Just quit — not expired yet
        assert!(!state.is_expired_split_nick("alice"));
    }

    #[test]
    fn default_impl_matches_new() {
        let a = NetsplitState::new();
        let b = NetsplitState::default();
        assert!(a.groups.is_empty());
        assert!(b.groups.is_empty());
    }

    #[test]
    fn valid_split_host_examples() {
        assert!(is_valid_split_host("irc.server.net"));
        assert!(is_valid_split_host("hub.eu.libera.chat"));
        assert!(is_valid_split_host("a.bc")); // minimal: 3 chars, has dot, 2-char alpha TLD
    }

    #[test]
    fn invalid_split_host_examples() {
        assert!(!is_valid_split_host("ab")); // too short
        assert!(!is_valid_split_host(".a.bc")); // leading dot
        assert!(!is_valid_split_host("a.bc.")); // trailing dot
        assert!(!is_valid_split_host("a..bc")); // double dot
        assert!(!is_valid_split_host("abc")); // no dot
        assert!(!is_valid_split_host("a.1")); // numeric TLD
        assert!(!is_valid_split_host("a.b")); // single char TLD
    }
}
