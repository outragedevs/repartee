use crate::state::buffer::{Buffer, NickEntry};

pub const DEFAULT_PREFIX_ORDER: &str = "~&@%+";

/// Sort buffers by: connection_id (alpha, case-insensitive) -> sort_group -> name (alpha, case-insensitive)
pub fn sort_buffers<'a>(buffers: &[&'a Buffer]) -> Vec<&'a Buffer> {
    let mut sorted: Vec<&Buffer> = buffers.to_vec();
    sorted.sort_by(|a, b| {
        let conn_cmp = a
            .connection_id
            .to_lowercase()
            .cmp(&b.connection_id.to_lowercase());
        if conn_cmp != std::cmp::Ordering::Equal {
            return conn_cmp;
        }
        let group_cmp = a.buffer_type.sort_group().cmp(&b.buffer_type.sort_group());
        if group_cmp != std::cmp::Ordering::Equal {
            return group_cmp;
        }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });
    sorted
}

/// Sort nicks by prefix rank (using prefix_order), then alphabetically (case-insensitive).
/// Nicks with no prefix (empty string) sort last.
pub fn sort_nicks<'a>(nicks: &[&'a NickEntry], prefix_order: &str) -> Vec<&'a NickEntry> {
    let mut sorted: Vec<&NickEntry> = nicks.to_vec();
    sorted.sort_by(|a, b| {
        let rank_a = prefix_rank(&a.prefix, prefix_order);
        let rank_b = prefix_rank(&b.prefix, prefix_order);
        let rank_cmp = rank_a.cmp(&rank_b);
        if rank_cmp != std::cmp::Ordering::Equal {
            return rank_cmp;
        }
        a.nick.to_lowercase().cmp(&b.nick.to_lowercase())
    });
    sorted
}

/// Return the sort rank for a prefix string.
/// Empty prefix -> sorts last (returns prefix_order.len()).
/// Unknown prefix char -> also sorts last.
fn prefix_rank(prefix: &str, prefix_order: &str) -> usize {
    if prefix.is_empty() {
        return prefix_order.len();
    }
    // Use the first character of the prefix for ranking
    let ch = prefix.chars().next().unwrap();
    prefix_order.find(ch).unwrap_or(prefix_order.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::*;
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_buffer(conn_id: &str, btype: BufferType, name: &str) -> Buffer {
        Buffer {
            id: make_buffer_id(conn_id, name),
            connection_id: conn_id.to_string(),
            buffer_type: btype,
            name: name.to_string(),
            messages: Vec::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
        }
    }

    fn make_nick(nick: &str, prefix: &str) -> NickEntry {
        NickEntry {
            nick: nick.to_string(),
            prefix: prefix.to_string(),
            modes: String::new(),
            away: false,
            account: None,
        }
    }

    #[test]
    fn sort_buffers_by_type_then_name() {
        let chan_b = make_buffer("libera", BufferType::Channel, "#beta");
        let chan_a = make_buffer("libera", BufferType::Channel, "#alpha");
        let server = make_buffer("libera", BufferType::Server, "libera");
        let query = make_buffer("libera", BufferType::Query, "someone");

        let input: Vec<&Buffer> = vec![&chan_b, &query, &server, &chan_a];
        let result = sort_buffers(&input);

        assert_eq!(result[0].name, "libera"); // server first
        assert_eq!(result[1].name, "#alpha"); // channels sorted alpha
        assert_eq!(result[2].name, "#beta");
        assert_eq!(result[3].name, "someone"); // query last
    }

    #[test]
    fn sort_nicks_ops_before_voice_before_normal() {
        let op = make_nick("alice", "@");
        let voice = make_nick("bob", "+");
        let normal = make_nick("charlie", "");

        let input: Vec<&NickEntry> = vec![&normal, &voice, &op];
        let result = sort_nicks(&input, DEFAULT_PREFIX_ORDER);

        assert_eq!(result[0].nick, "alice"); // @
        assert_eq!(result[1].nick, "bob"); // +
        assert_eq!(result[2].nick, "charlie"); // no prefix
    }

    #[test]
    fn sort_nicks_alphabetical_same_prefix() {
        let a = make_nick("Zara", "@");
        let b = make_nick("alice", "@");
        let c = make_nick("Bob", "@");

        let input: Vec<&NickEntry> = vec![&a, &b, &c];
        let result = sort_nicks(&input, DEFAULT_PREFIX_ORDER);

        assert_eq!(result[0].nick, "alice");
        assert_eq!(result[1].nick, "Bob");
        assert_eq!(result[2].nick, "Zara");
    }
}
