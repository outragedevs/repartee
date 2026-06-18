//! `IRCv3` `draft/chathistory` request construction and client-side state.
//!
//! repartee treats chathistory as a background *filler* of the `SQLite` store:
//! requests are built here and sent via `Command::Raw`, and the resulting
//! history batches are quietly ingested (see [`crate::irc::batch`]). The UI
//! always reads from `SQLite`, so this module never touches buffers directly.

// Wired into the binary incrementally across the chathistory tasks (request
// state, batch ingest, scroll-up + reconnect triggers). Matches the
// `#[allow(dead_code)]` convention used by `cap.rs` / `isupport.rs`.
#![allow(dead_code)]

/// Which reference type to anchor a `CHATHISTORY` request with.
///
/// Chosen from the server's `MSGREFTYPES` ISUPPORT token, preferring `msgid`
/// (stable across clock skew) over `timestamp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    MsgId,
    Timestamp,
}

/// A resolved anchor for a `CHATHISTORY` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryRef {
    /// Rendered as `msgid=<id>`.
    MsgId(String),
    /// Rendered as `timestamp=<rfc3339>`.
    Timestamp(String),
    /// Rendered as `*` — "the most recent messages", only valid with `LATEST`.
    Latest,
}

/// Choose the reference type the server accepts, preferring `msgid`.
///
/// Falls back to `Timestamp` when `msgid` is not advertised (or the list is
/// empty), since every chathistory-capable server supports timestamps.
#[must_use]
pub fn pick_ref_type(msgreftypes: &[String]) -> RefKind {
    if msgreftypes.iter().any(|t| t == "msgid") {
        RefKind::MsgId
    } else {
        RefKind::Timestamp
    }
}

/// Clamp a desired page size to the server-advertised maximum (if any),
/// never returning less than 1.
#[must_use]
pub fn clamp_limit(want: usize, server_max: Option<usize>) -> usize {
    want.min(server_max.unwrap_or(want)).max(1)
}

/// Render the wire string for a single-anchor `CHATHISTORY` request,
/// to be sent via `Command::Raw`.
///
/// Examples:
/// - `CHATHISTORY BEFORE #chan msgid=abc 100`
/// - `CHATHISTORY AFTER #chan timestamp=2024-01-01T00:00:00.000Z 50`
/// - `CHATHISTORY LATEST #chan * 100`
#[must_use]
pub fn build_command(
    subcommand: &str,
    target: &str,
    anchor: &HistoryRef,
    limit: usize,
) -> String {
    let anchor_str = match anchor {
        HistoryRef::MsgId(id) => format!("msgid={id}"),
        HistoryRef::Timestamp(ts) => format!("timestamp={ts}"),
        HistoryRef::Latest => "*".to_string(),
    };
    format!("CHATHISTORY {subcommand} {target} {anchor_str} {limit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_ref_type_prefers_msgid() {
        assert_eq!(
            pick_ref_type(&["timestamp".into(), "msgid".into()]),
            RefKind::MsgId
        );
    }

    #[test]
    fn pick_ref_type_falls_back_to_timestamp() {
        assert_eq!(pick_ref_type(&["timestamp".into()]), RefKind::Timestamp);
        assert_eq!(pick_ref_type(&[]), RefKind::Timestamp);
    }

    #[test]
    fn clamp_limit_respects_server_max() {
        assert_eq!(clamp_limit(200, Some(100)), 100);
        assert_eq!(clamp_limit(50, Some(100)), 50);
    }

    #[test]
    fn clamp_limit_without_server_max() {
        assert_eq!(clamp_limit(200, None), 200);
    }

    #[test]
    fn clamp_limit_never_zero() {
        assert_eq!(clamp_limit(0, Some(100)), 1);
        assert_eq!(clamp_limit(0, None), 1);
    }

    #[test]
    fn build_before_msgid() {
        let cmd = build_command("BEFORE", "#chan", &HistoryRef::MsgId("abc".into()), 100);
        assert_eq!(cmd, "CHATHISTORY BEFORE #chan msgid=abc 100");
    }

    #[test]
    fn build_after_timestamp() {
        let cmd = build_command(
            "AFTER",
            "#chan",
            &HistoryRef::Timestamp("2024-01-01T00:00:00.000Z".into()),
            50,
        );
        assert_eq!(
            cmd,
            "CHATHISTORY AFTER #chan timestamp=2024-01-01T00:00:00.000Z 50"
        );
    }

    #[test]
    fn build_latest_star() {
        let cmd = build_command("LATEST", "#chan", &HistoryRef::Latest, 100);
        assert_eq!(cmd, "CHATHISTORY LATEST #chan * 100");
    }
}
