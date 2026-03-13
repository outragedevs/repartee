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

/// A nick that rejoined after a netsplit, along with which channels it joined.
#[derive(Debug, Clone)]
pub struct NetjoinRecord {
    pub nick: String,
    pub channels: HashSet<String>,
}

/// A group of nicks rejoining after a netsplit.
#[derive(Debug, Clone)]
pub struct NetjoinGroup {
    pub server1: String,
    pub server2: String,
    pub records: Vec<NetjoinRecord>,
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
        Self {
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

        let Some(space) = message.find(' ') else {
            return false;
        };
        let server1 = &message[..space];
        let server2 = &message[space + 1..];
        let now = Instant::now();

        // Find existing group for this server pair that hasn't been printed yet
        let group_idx = self
            .groups
            .iter()
            .position(|g| g.server1 == server1 && g.server2 == server2 && !g.printed);

        let idx = if let Some(idx) = group_idx {
            idx
        } else {
            self.groups.push(SplitGroup {
                server1: server1.to_string(),
                server2: server2.to_string(),
                nicks: Vec::new(),
                last_quit: now,
                printed: false,
            });
            self.groups.len() - 1
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
        let Some(&group_idx) = self.nick_index.get(nick) else {
            return false;
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
        let nj_idx = self
            .netjoins
            .iter()
            .position(|nj| nj.server1 == server1 && nj.server2 == server2 && !nj.printed);

        let nj_index = if let Some(idx) = nj_idx {
            idx
        } else {
            self.netjoins.push(NetjoinGroup {
                server1,
                server2,
                records: Vec::new(),
                last_join: now,
                printed: false,
            });
            self.netjoins.len() - 1
        };

        if let Some(nj) = self.netjoins.get_mut(nj_index) {
            if let Some(rec) = nj.records.iter_mut().find(|r| r.nick == nick) {
                rec.channels.insert(buffer_id.to_string());
            } else {
                nj.records.push(NetjoinRecord {
                    nick: nick.to_string(),
                    channels: HashSet::from([buffer_id.to_string()]),
                });
            }
            nj.last_join = now;
        }

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
                messages.extend(format_split_messages(group));
                group.printed = true;
            }
        }

        // Print netjoin groups that have been quiet for NETJOIN_BATCH_WAIT
        for nj in &mut self.netjoins {
            if !nj.printed && now.duration_since(nj.last_join) >= NETJOIN_BATCH_WAIT {
                messages.extend(format_netjoin_messages(nj));
                nj.printed = true;
            }
        }

        // Expire old split records
        self.groups
            .retain(|g| now.duration_since(g.last_quit) < SPLIT_EXPIRE);
        self.netjoins
            .retain(|nj| now.duration_since(nj.last_join) < SPLIT_EXPIRE);

        // Rebuild nick_index from scratch — indices are invalidated by retain()
        self.nick_index.clear();
        for (idx, group) in self.groups.iter().enumerate() {
            for rec in &group.nicks {
                self.nick_index.insert(rec.nick.clone(), idx);
            }
        }

        messages
    }

    /// Check if a nick is known to have quit in an expired (or current) netsplit.
    /// Used for nick list cleanup when a split nick never rejoins.
    #[allow(dead_code)] // Will be used when nick list stale-entry cleanup is wired
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

/// Format per-channel netsplit quit messages (erssi/irssi style).
/// Each channel gets its own message listing only the nicks that were in THAT channel.
fn format_split_messages(group: &SplitGroup) -> Vec<NetsplitMessage> {
    // Group nicks by channel.
    let mut channel_nicks: HashMap<&str, Vec<&str>> = HashMap::new();
    for rec in &group.nicks {
        for ch in &rec.channels {
            channel_nicks.entry(ch.as_str()).or_default().push(&rec.nick);
        }
    }

    channel_nicks
        .into_iter()
        .map(|(channel, nicks)| {
            let nick_str = format_nick_list(&nicks);
            NetsplitMessage {
                buffer_ids: vec![channel.to_string()],
                text: format!(
                    "Netsplit {} \u{21C4} {} quits: {}",
                    group.server1, group.server2, nick_str
                ),
            }
        })
        .collect()
}

/// Format per-channel netjoin messages (erssi/irssi style).
fn format_netjoin_messages(group: &NetjoinGroup) -> Vec<NetsplitMessage> {
    // Group nicks by channel.
    let mut channel_nicks: HashMap<&str, Vec<&str>> = HashMap::new();
    for rec in &group.records {
        for ch in &rec.channels {
            channel_nicks.entry(ch.as_str()).or_default().push(&rec.nick);
        }
    }

    channel_nicks
        .into_iter()
        .map(|(channel, nicks)| {
            let nick_str = format_nick_list(&nicks);
            NetsplitMessage {
                buffer_ids: vec![channel.to_string()],
                text: format!(
                    "Netsplit over {} \u{21C4} {} joins: {}",
                    group.server1, group.server2, nick_str
                ),
            }
        })
        .collect()
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
        state.handle_quit("alice", "hub.net leaf.net", &["conn/#chan".to_string()]);
        state.handle_quit("bob", "hub.net leaf.net", &["conn/#chan".to_string()]);
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
        state.handle_quit("alice", "hub.net leaf.net", &["conn/#chan".to_string()]);
        assert!(state.handle_join("alice", "conn/#chan"));
        assert_eq!(state.netjoins.len(), 1);
        assert_eq!(state.netjoins[0].records.len(), 1);
        assert_eq!(state.netjoins[0].records[0].nick, "alice");
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
        state.handle_quit("alice", "hub.net leaf.net", &["conn/#chan".to_string()]);
        // Immediately calling tick should return nothing (batch wait not elapsed)
        let msgs = state.tick();
        assert!(msgs.is_empty());
    }

    #[test]
    fn format_nick_list_under_limit() {
        let nicks: Vec<&str> = (0..5)
            .map(|i| match i {
                0 => "a",
                1 => "b",
                2 => "c",
                3 => "d",
                _ => "e",
            })
            .collect();
        let result = format_nick_list(&nicks);
        assert_eq!(result, "a, b, c, d, e");
    }

    #[test]
    fn format_nick_list_over_limit() {
        let names: Vec<String> = (0..20).map(|i| format!("nick{i}")).collect();
        let nicks: Vec<&str> = names.iter().map(String::as_str).collect();
        let result = format_nick_list(&nicks);
        assert!(result.contains("(+5 more)"));
        assert!(result.contains("nick0"));
        assert!(result.contains("nick14"));
        assert!(!result.contains("nick15"));
    }

    #[test]
    fn format_split_messages_per_channel() {
        let group = SplitGroup {
            server1: "hub.net".to_string(),
            server2: "leaf.net".to_string(),
            nicks: vec![
                SplitRecord {
                    nick: "alice".to_string(),
                    channels: vec!["conn/#a".to_string(), "conn/#b".to_string()],
                },
                SplitRecord {
                    nick: "bob".to_string(),
                    channels: vec!["conn/#a".to_string()],
                },
            ],
            last_quit: Instant::now(),
            printed: false,
        };
        let msgs = format_split_messages(&group);
        // One message per channel.
        assert_eq!(msgs.len(), 2);
        let chan_a = msgs.iter().find(|m| m.buffer_ids == ["conn/#a"]).unwrap();
        assert!(chan_a.text.contains("alice"));
        assert!(chan_a.text.contains("bob"));
        let chan_b = msgs.iter().find(|m| m.buffer_ids == ["conn/#b"]).unwrap();
        assert!(chan_b.text.contains("alice"));
        assert!(!chan_b.text.contains("bob"));
    }

    #[test]
    fn format_netjoin_messages_per_channel() {
        let mut ch_a = HashSet::new();
        ch_a.insert("conn/#a".to_string());
        let mut ch_both = HashSet::new();
        ch_both.insert("conn/#a".to_string());
        ch_both.insert("conn/#b".to_string());
        let group = NetjoinGroup {
            server1: "hub.net".to_string(),
            server2: "leaf.net".to_string(),
            records: vec![
                NetjoinRecord {
                    nick: "alice".to_string(),
                    channels: ch_both,
                },
                NetjoinRecord {
                    nick: "bob".to_string(),
                    channels: ch_a,
                },
            ],
            last_join: Instant::now(),
            printed: false,
        };
        let msgs = format_netjoin_messages(&group);
        assert_eq!(msgs.len(), 2);
        let chan_a = msgs.iter().find(|m| m.buffer_ids == ["conn/#a"]).unwrap();
        assert!(chan_a.text.contains("alice"));
        assert!(chan_a.text.contains("bob"));
        assert!(chan_a.text.contains("Netsplit over"));
        let chan_b = msgs.iter().find(|m| m.buffer_ids == ["conn/#b"]).unwrap();
        assert!(chan_b.text.contains("alice"));
        assert!(!chan_b.text.contains("bob"));
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
