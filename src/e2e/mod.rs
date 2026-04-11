//! RPE2E — Repartee End-to-End encryption (v1.0)
//!
//! See `docs/plans/2026-04-10-e2e-encryption-architecture.md` for the full spec
//! and `docs/plans/2026-04-11-rpee2e-implementation.md` for the implementation
//! plan.
//!
//! Model summary:
//! - Twopart identity: `(fingerprint, handle=ident@host)`
//! - Per-sender per-channel symmetric keys
//! - Stateless per-chunk encryption (no reassembly)
//! - CTCP NOTICE handshake with KEYREQ/KEYRSP
//! - Strict handle check on decrypt path

// The e2e module is built in phases; several primitives are written before
// the consumers (handshake, manager, command handlers) land. Silence lints
// that would otherwise fire on intentionally-unused helpers until the later
// phases wire them up.
#![allow(
    dead_code,
    clippy::missing_const_for_fn,
    clippy::unnecessary_wraps,
    clippy::module_name_repetitions,
    clippy::doc_markdown,
    // Every keyring method grabs `self.db.lock()` as its first line and
    // releases it when the single SQL statement returns. Tightening further
    // buys nothing and hurts readability for no real contention win.
    clippy::significant_drop_tightening,
    // Keyring::load_identity returns a 4-tuple read straight from SQLite
    // columns; naming an alias just for the return type would obscure, not
    // clarify.
    clippy::type_complexity
)]

pub mod chunker;
pub mod commands;
pub mod crypto;
pub mod error;
pub mod handshake;
pub mod keyring;
pub mod manager;
pub mod portable;
pub mod wire;

#[allow(unused_imports)]
pub use error::E2eError;
#[allow(unused_imports)]
pub use manager::E2eManager;

#[cfg(test)]
mod integration_tests;

/// Protocol version string embedded in wire format and AAD.
pub const PROTO: &str = "RPE2E01";

/// Max chunks per logical message (hard cap for sender).
pub const MAX_CHUNKS: u8 = 16;

/// Max plaintext bytes per chunk before ciphertext expansion.
/// Chosen so that a chunk fits in ~400 bytes of IRC payload after base64.
pub const MAX_PLAINTEXT_PER_CHUNK: usize = 180;

/// Replay-protection window for `ts` in AAD (seconds).
pub const TS_TOLERANCE_SECS: i64 = 300;

/// Derive the keyring `channel` context for a conversation.
///
/// For real IRC channels (prefixes `#`, `&`, `!`, `+`) the target is
/// passed through unchanged. For private messages we construct the
/// pseudochannel `@<peer_handle>` per spec §6, where `peer_handle` is
/// the server-stamped `ident@host` of the remote peer. Two PMs from
/// peers that happen to share a nick across different hosts — or the
/// same peer reconnecting from a new host — therefore live under
/// distinct keyring rows instead of colliding under a bare nick.
///
/// Callers **must** pass the raw server-stamped peer handle
/// (`ident@host` as seen in the IRC prefix), never the peer's nick.
/// Passing a nick reintroduces the collision the pseudochannel exists
/// to prevent.
#[must_use]
pub fn context_key(target: &str, peer_handle: &str) -> String {
    if target.starts_with(['#', '&', '!', '+']) {
        target.to_string()
    } else {
        format!("@{peer_handle}")
    }
}

#[cfg(test)]
mod context_key_tests {
    use super::context_key;

    #[test]
    fn channels_pass_through_unchanged() {
        assert_eq!(context_key("#rust", "~bob@b.host"), "#rust");
        assert_eq!(context_key("&local", "~bob@b.host"), "&local");
        assert_eq!(context_key("!HXYZ", "~bob@b.host"), "!HXYZ");
        assert_eq!(context_key("+modeless", "~bob@b.host"), "+modeless");
    }

    #[test]
    fn pm_targets_become_pseudochannel() {
        assert_eq!(
            context_key("bob", "~bob@home.example.org"),
            "@~bob@home.example.org"
        );
        // Same nick, different host → different pseudochannel.
        assert_eq!(
            context_key("bob", "~bob@vpn.example.org"),
            "@~bob@vpn.example.org"
        );
        assert_ne!(
            context_key("bob", "~bob@home.example.org"),
            context_key("bob", "~bob@vpn.example.org")
        );
    }
}
