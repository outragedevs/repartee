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
    clippy::doc_markdown
)]

pub mod chunker;
pub mod commands;
pub mod crypto;
pub mod error;
pub mod handshake;
pub mod keyring;
pub mod manager;
pub mod wire;

#[allow(unused_imports)]
pub use error::E2eError;
// E2eManager re-exported after Task 12b lands.

/// Protocol version string embedded in wire format and AAD.
pub const PROTO: &str = "RPE2E01";

/// Max chunks per logical message (hard cap for sender).
pub const MAX_CHUNKS: u8 = 16;

/// Max plaintext bytes per chunk before ciphertext expansion.
/// Chosen so that a chunk fits in ~400 bytes of IRC payload after base64.
pub const MAX_PLAINTEXT_PER_CHUNK: usize = 180;

/// Replay-protection window for `ts` in AAD (seconds).
pub const TS_TOLERANCE_SECS: i64 = 300;
