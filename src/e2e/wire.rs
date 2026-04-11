//! RPE2E01 wire format: encode/decode `+RPE2E01 <msgid> <ts> <part>/<total> <nonce_b64>:<ct_b64>`.
//!
//! Each chunk is a standalone cryptographic unit — receivers decrypt and
//! render immediately without reassembly state (see architecture spec §6).

// Items in this module are wired into `E2eManager` in later tasks (12+).
#![allow(dead_code)]

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

use crate::e2e::error::{E2eError, Result};
use crate::e2e::{MAX_CHUNKS, PROTO};

/// Wire prefix magic.
pub const WIRE_PREFIX: &str = "+RPE2E01";

/// 8-byte random message ID (shared across chunks of a single logical message).
pub type MsgId = [u8; 8];

/// Parsed components of a single RPE2E01 chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireChunk {
    pub msgid: MsgId,
    pub ts: i64,
    pub part: u8,
    pub total: u8,
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

impl WireChunk {
    /// Serialize into an IRC-safe one-line payload (no trailing newline).
    pub fn encode(&self) -> Result<String> {
        if self.total == 0 || self.total > MAX_CHUNKS {
            return Err(E2eError::ChunkLimit(self.total));
        }
        if self.part == 0 || self.part > self.total {
            return Err(E2eError::Wire(format!(
                "invalid part/total: {}/{}",
                self.part, self.total
            )));
        }
        let msgid = hex::encode(self.msgid);
        let nonce_b64 = B64.encode(self.nonce);
        let ct_b64 = B64.encode(&self.ciphertext);
        Ok(format!(
            "{WIRE_PREFIX} {msgid} {ts} {part}/{total} {nonce_b64}:{ct_b64}",
            ts = self.ts,
            part = self.part,
            total = self.total,
        ))
    }

    /// Parse an incoming wire line. Returns `Ok(None)` if the line does not
    /// start with the RPE2E01 prefix (i.e. cleartext), `Err` on malformed.
    pub fn parse(line: &str) -> Result<Option<Self>> {
        let rest = match line.strip_prefix(WIRE_PREFIX) {
            Some(r) => r.trim_start(),
            None => return Ok(None),
        };
        let mut fields = rest.split_whitespace();
        let msgid_hex = fields.next().ok_or_else(|| E2eError::Wire("missing msgid".into()))?;
        let ts_str = fields.next().ok_or_else(|| E2eError::Wire("missing ts".into()))?;
        let parttot = fields.next().ok_or_else(|| E2eError::Wire("missing part/total".into()))?;
        let body = fields.next().ok_or_else(|| E2eError::Wire("missing body".into()))?;
        if fields.next().is_some() {
            return Err(E2eError::Wire("extra fields".into()));
        }

        if msgid_hex.len() != 16 {
            return Err(E2eError::Wire("msgid must be 16 hex chars".into()));
        }
        let msgid_vec = hex::decode(msgid_hex)?;
        let mut msgid = [0u8; 8];
        msgid.copy_from_slice(&msgid_vec);

        let ts: i64 = ts_str
            .parse()
            .map_err(|e| E2eError::Wire(format!("bad ts: {e}")))?;

        let (p, t) = parttot
            .split_once('/')
            .ok_or_else(|| E2eError::Wire("part/total missing slash".into()))?;
        let part: u8 = p.parse().map_err(|e| E2eError::Wire(format!("bad part: {e}")))?;
        let total: u8 = t.parse().map_err(|e| E2eError::Wire(format!("bad total: {e}")))?;
        if total == 0 || total > MAX_CHUNKS || part == 0 || part > total {
            return Err(E2eError::Wire(format!("bad part/total {part}/{total}")));
        }

        let (nonce_b64, ct_b64) = body
            .split_once(':')
            .ok_or_else(|| E2eError::Wire("missing nonce:ct separator".into()))?;
        let nonce_vec = B64.decode(nonce_b64)?;
        if nonce_vec.len() != 24 {
            return Err(E2eError::Wire(format!("nonce must be 24 bytes, got {}", nonce_vec.len())));
        }
        let mut nonce = [0u8; 24];
        nonce.copy_from_slice(&nonce_vec);
        let ciphertext = B64.decode(ct_b64)?;

        Ok(Some(Self { msgid, ts, part, total, nonce, ciphertext }))
    }
}

/// Construct the AAD (Additional Authenticated Data) for a chunk.
///
/// AAD = `"RPE2E01" || channel || msgid || ts || part || total`
///
/// Note — the sender handle is **not** in AAD. Sender authentication is
/// enforced at the keyring layer: on decrypt the receiver looks up the
/// incoming session by `(handle_from_IRC_prefix, channel)`, so only the
/// real server-stamped sender can produce a ciphertext the receiver will
/// even attempt to decrypt. Duplicating that binding inside AAD would
/// force the sender to know its own `ident@host` before encrypting, which
/// it does not on every IRC network.
pub fn build_aad(channel: &str, msgid: MsgId, ts: i64, part: u8, total: u8) -> Vec<u8> {
    let mut aad = Vec::with_capacity(32 + channel.len());
    aad.extend_from_slice(PROTO.as_bytes());
    aad.push(b':');
    aad.extend_from_slice(channel.as_bytes());
    aad.push(b':');
    aad.extend_from_slice(&msgid);
    aad.push(b':');
    aad.extend_from_slice(&ts.to_be_bytes());
    aad.push(b':');
    aad.push(part);
    aad.push(b':');
    aad.push(total);
    aad
}

/// Generate a fresh 8-byte random message ID.
pub fn fresh_msgid() -> MsgId {
    let mut id = [0u8; 8];
    rand::fill(&mut id);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_chunk() -> WireChunk {
        WireChunk {
            msgid: [0xab; 8],
            ts: 1_712_000_000,
            part: 1,
            total: 1,
            nonce: [0x42; 24],
            ciphertext: vec![0xde, 0xad, 0xbe, 0xef],
        }
    }

    #[test]
    fn encode_starts_with_prefix() {
        let enc = sample_chunk().encode().unwrap();
        assert!(enc.starts_with(WIRE_PREFIX));
    }

    #[test]
    fn encode_roundtrip() {
        let c = sample_chunk();
        let enc = c.encode().unwrap();
        let parsed = WireChunk::parse(&enc).unwrap().unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn parse_cleartext_returns_none() {
        assert_eq!(WireChunk::parse("hello world").unwrap(), None);
        assert_eq!(WireChunk::parse("").unwrap(), None);
    }

    #[test]
    fn parse_rejects_invalid_part_total() {
        let mut c = sample_chunk();
        c.total = 0;
        assert!(c.encode().is_err());
        c.total = 17;
        assert!(c.encode().is_err());
        c.total = 3;
        c.part = 4;
        assert!(c.encode().is_err());
    }

    #[test]
    fn parse_rejects_bad_nonce_length() {
        let bad = "+RPE2E01 abababababababab 1712000000 1/1 YWJj:ZGVm";
        // "YWJj" decodes to 3 bytes, not 24
        assert!(WireChunk::parse(bad).is_err());
    }

    #[test]
    fn build_aad_is_deterministic() {
        let a = build_aad("#chan", [1; 8], 100, 1, 3);
        let b = build_aad("#chan", [1; 8], 100, 1, 3);
        assert_eq!(a, b);
    }

    #[test]
    fn build_aad_sensitive_to_every_field() {
        let base = build_aad("#chan", [1; 8], 100, 1, 3);
        assert_ne!(base, build_aad("#other", [1; 8], 100, 1, 3));
        assert_ne!(base, build_aad("#chan", [2; 8], 100, 1, 3));
        assert_ne!(base, build_aad("#chan", [1; 8], 101, 1, 3));
        assert_ne!(base, build_aad("#chan", [1; 8], 100, 2, 3));
        assert_ne!(base, build_aad("#chan", [1; 8], 100, 1, 4));
    }

    #[test]
    fn fresh_msgid_is_random_ish() {
        let a = fresh_msgid();
        let b = fresh_msgid();
        assert_ne!(a, b);
    }
}
