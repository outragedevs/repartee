//! Mention log line formatting.
//!
//! Lives in `state/`, not `ui/`, because [`crate::state::AppState`] builds
//! mention rows itself — the reorder queue fans a translated line's mention
//! out at RELEASE time, from inside state — and state must stay UI-agnostic.
//! The function is a pure string formatter (irssi `%Z` codes, no ratatui),
//! so nothing about it is terminal-specific; both front ends parse its
//! output through their own renderers.

// Theme colors (same hex values used in default.theme).
const COLOR_TIMESTAMP: &str = "6e738d"; // muted gray — matches {timestamp} abstract
const COLOR_NETWORK: &str = "565f89"; // dim gray — matches hostname in join events
const COLOR_CHANNEL: &str = "7aa2f7"; // accent blue — matches {channel} abstract
const COLOR_SEP: &str = "7aa2f7"; // accent blue — nick separator ❯

/// Build a pre-formatted mention log line with irssi `%Z` color codes.
///
/// Layout: `[datetime] [network] [channel] nick❯ text`
///
/// Called from `state/events.rs` (live mentions and queue-released ones)
/// and `app/mentions.rs` (DB reload).
#[must_use]
pub fn format_mention_line(
    datetime: &str,
    network: &str,
    channel: &str,
    nick: &str,
    text: &str,
    nick_sat: f32,
    nick_lit: f32,
) -> String {
    let nick_hex = crate::nick_color::nick_color_hex(nick, nick_sat, nick_lit);

    format!(
        "%Z{COLOR_TIMESTAMP}[{datetime}]%N \
         %Z{COLOR_NETWORK}[{network}]%N \
         %Z{COLOR_CHANNEL}[{channel}]%N \
         %Z{nick_hex}%_{nick}%_%N\
         %Z{COLOR_SEP}\u{276F}%N {text}",
    )
}
