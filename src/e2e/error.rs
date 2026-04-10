//! Error type for the RPE2E module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum E2eError {
    #[error("crypto error: {0}")]
    Crypto(String),

    #[error("wire format parse error: {0}")]
    Wire(String),

    #[error("keyring error: {0}")]
    Keyring(String),

    #[error("handshake error: {0}")]
    Handshake(String),

    #[error("peer not trusted: handle={handle} channel={channel}")]
    PeerNotTrusted { handle: String, channel: String },

    #[error("handle mismatch: expected {expected}, got {got}")]
    HandleMismatch { expected: String, got: String },

    #[error("replay window violation: ts={ts} now={now}")]
    ReplayWindow { ts: i64, now: i64 },

    #[error("chunk limit exceeded: {0} > 16")]
    ChunkLimit(u8),

    #[error("rate limit exceeded for peer {0}")]
    RateLimit(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("base64 error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("hex error: {0}")]
    Hex(#[from] hex::FromHexError),
}

pub type Result<T> = std::result::Result<T, E2eError>;
