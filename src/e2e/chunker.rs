//! Stateless plaintext chunker. Splits plaintext into N ≤ 16 pieces, each
//! fitting within `MAX_PLAINTEXT_PER_CHUNK` bytes.
//!
//! This is "stateless" in the sense that each chunk becomes an independent
//! encrypted PRIVMSG — the receiver never reassembles. See architecture §6.

// Wired into `E2eManager` in later tasks (12+).
#![allow(dead_code)]

use crate::e2e::error::{E2eError, Result};
use crate::e2e::{MAX_CHUNKS, MAX_PLAINTEXT_PER_CHUNK};

/// Split a plaintext message into chunks. Each chunk is at most
/// `MAX_PLAINTEXT_PER_CHUNK` bytes. Returns an error if the message would
/// require more than `MAX_CHUNKS` chunks.
///
/// Splitting is byte-based on UTF-8 boundaries. Input is assumed to be
/// valid UTF-8 (it came from the user's terminal).
///
/// Empty plaintext is refused (`Err(E2eError::Wire("empty plaintext"))`)
/// so upstream callers cannot accidentally ship a zero-length-ciphertext
/// chunk the peer would render as a blank message. Input layers that
/// might generate a blank line (empty /say, trimmed whitespace, etc.)
/// must guard before calling `encrypt_outgoing`.
pub fn split_plaintext(plaintext: &str) -> Result<Vec<Vec<u8>>> {
    if plaintext.is_empty() {
        return Err(E2eError::Wire("empty plaintext".into()));
    }

    let bytes = plaintext.as_bytes();
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        let mut end = (cursor + MAX_PLAINTEXT_PER_CHUNK).min(bytes.len());
        // Walk back to a UTF-8 char boundary if we're in the middle of a
        // multi-byte sequence.
        while end > cursor && !plaintext.is_char_boundary(end) {
            end -= 1;
        }
        if end == cursor {
            return Err(E2eError::Wire(
                "cannot split: single UTF-8 char exceeds chunk budget".into(),
            ));
        }
        chunks.push(bytes[cursor..end].to_vec());
        cursor = end;

        if chunks.len() > usize::from(MAX_CHUNKS) {
            return Err(E2eError::ChunkLimit(
                u8::try_from(chunks.len()).unwrap_or(u8::MAX),
            ));
        }
    }

    let total = chunks.len();
    if total > usize::from(MAX_CHUNKS) {
        return Err(E2eError::ChunkLimit(
            u8::try_from(total).unwrap_or(u8::MAX),
        ));
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_message_is_one_chunk() {
        let chunks = split_plaintext("hello").unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(&chunks[0], b"hello");
    }

    #[test]
    fn empty_plaintext_is_rejected() {
        // G13: refuse to chunk empty input so `encrypt_outgoing` cannot
        // ship a zero-length-ciphertext chunk that peers would render as
        // a blank message.
        match split_plaintext("") {
            Err(E2eError::Wire(msg)) => assert!(
                msg.contains("empty plaintext"),
                "expected 'empty plaintext', got: {msg}"
            ),
            other => panic!("expected Err(Wire(empty plaintext)), got {other:?}"),
        }
    }

    #[test]
    fn long_message_is_multi_chunk() {
        let s = "x".repeat(500);
        let chunks = split_plaintext(&s).unwrap();
        assert!(chunks.len() >= 2);
        let rejoined: Vec<u8> = chunks.iter().flatten().copied().collect();
        assert_eq!(rejoined, s.as_bytes());
    }

    #[test]
    fn boundary_respects_utf8_chars() {
        // Each 💩 is 4 bytes; pad to force split inside character.
        let prefix = "x".repeat(MAX_PLAINTEXT_PER_CHUNK - 2);
        let s = format!("{prefix}💩{prefix}");
        let chunks = split_plaintext(&s).unwrap();
        for c in &chunks {
            // Each chunk on its own must be valid UTF-8.
            assert!(std::str::from_utf8(c).is_ok());
        }
    }

    #[test]
    fn message_beyond_max_chunks_errors() {
        let s = "a".repeat(MAX_PLAINTEXT_PER_CHUNK * 17);
        assert!(split_plaintext(&s).is_err());
    }
}
