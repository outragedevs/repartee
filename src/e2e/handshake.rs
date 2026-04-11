//! RPE2E CTCP handshake: KEYREQ / KEYRSP encode/parse and rate limiting.
//!
//! Wire form (inside the CTCP `\x01 ... \x01` framing, sent via NOTICE):
//!
//! ```text
//! RPEE2E KEYREQ v=1 chan=#x pub=<hex32> eph=<hex32> nonce=<hex16> sig=<hex64>
//! RPEE2E KEYRSP v=1 chan=#x eph=<hex32> wnonce=<hex24> wrap=<b64> nonce=<hex16> sig=<hex64>
//! ```
//!
//! `pub` carries the initiator's long-term Ed25519 identity pubkey. `eph` on
//! `KEYREQ` is the initiator's ephemeral X25519 pubkey; it is bound to the
//! signature so a MitM cannot swap it out. `eph` on `KEYRSP` is the
//! responder's ephemeral X25519 pubkey. The wrap key is derived by either
//! side from an X25519 ECDH of their own ephemeral secret with the peer's
//! ephemeral public (HKDF-SHA256, see `crypto::ecdh`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::e2e::crypto::aead::NONCE_LEN;
use crate::e2e::error::{E2eError, Result};

pub const CTCP_TAG: &str = "RPEE2E";
pub const PROTO_VERSION: u8 = 1;

/// Minimum gap between outgoing KEYREQ to the same peer.
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(30);

/// KEYREQ message. `pubkey` is the initiator's long-term Ed25519 identity;
/// `eph_x25519` is a fresh ephemeral X25519 public used for ECDH. Both are
/// bound to `sig`.
#[derive(Debug, Clone)]
pub struct KeyReq {
    pub channel: String,
    pub pubkey: [u8; 32],
    pub eph_x25519: [u8; 32],
    pub nonce: [u8; 16],
    pub sig: [u8; 64],
}

/// KEYRSP message. Carries the responder's ephemeral X25519 pub and an AEAD
/// ciphertext containing the channel session key wrapped under the derived
/// ECDH+HKDF wrap key. `pubkey` is the responder's long-term Ed25519
/// identity — the initiator verifies the signature against it and uses it
/// as the TOFU pin, so the pubkey does not need to be known out-of-band.
#[derive(Debug, Clone)]
pub struct KeyRsp {
    pub channel: String,
    pub pubkey: [u8; 32],
    pub ephemeral_pub: [u8; 32],
    pub wrap_nonce: [u8; NONCE_LEN],
    pub wrap_ct: Vec<u8>,
    pub nonce: [u8; 16],
    pub sig: [u8; 64],
}

/// Canonical payload signed by the initiator in KEYREQ. Binding
/// `eph_x25519` into the signature prevents a downgrade or swap attack
/// where a MitM would otherwise be able to substitute its own X25519 key
/// without breaking the Ed25519 signature.
fn sig_payload_keyreq(
    channel: &str,
    pubkey: &[u8; 32],
    eph_x25519: &[u8; 32],
    nonce: &[u8; 16],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + channel.len() + 32 + 32 + 16);
    v.extend_from_slice(b"KEYREQ:");
    v.extend_from_slice(channel.as_bytes());
    v.push(b':');
    v.extend_from_slice(pubkey);
    v.push(b':');
    v.extend_from_slice(eph_x25519);
    v.push(b':');
    v.extend_from_slice(nonce);
    v
}

/// Canonical payload signed by the responder in KEYRSP. Binds the
/// responder's identity `pubkey` so a MitM cannot substitute its own
/// long-term key without breaking the Ed25519 signature, even though the
/// initiator has no prior record of the responder.
fn sig_payload_keyrsp(
    channel: &str,
    pubkey: &[u8; 32],
    eph_pub: &[u8; 32],
    wrap_nonce: &[u8; NONCE_LEN],
    wrap_ct: &[u8],
    nonce: &[u8; 16],
) -> Vec<u8> {
    let mut v =
        Vec::with_capacity(16 + channel.len() + 32 + 32 + NONCE_LEN + wrap_ct.len() + 16);
    v.extend_from_slice(b"KEYRSP:");
    v.extend_from_slice(channel.as_bytes());
    v.push(b':');
    v.extend_from_slice(pubkey);
    v.push(b':');
    v.extend_from_slice(eph_pub);
    v.push(b':');
    v.extend_from_slice(wrap_nonce);
    v.push(b':');
    v.extend_from_slice(wrap_ct);
    v.push(b':');
    v.extend_from_slice(nonce);
    v
}

#[must_use]
pub fn encode_keyreq(req: &KeyReq) -> String {
    format!(
        "{CTCP_TAG} KEYREQ v={PROTO_VERSION} chan={chan} pub={pub_} eph={eph} nonce={nonce} sig={sig}",
        chan = req.channel,
        pub_ = hex::encode(req.pubkey),
        eph = hex::encode(req.eph_x25519),
        nonce = hex::encode(req.nonce),
        sig = hex::encode(req.sig),
    )
}

#[must_use]
pub fn encode_keyrsp(rsp: &KeyRsp) -> String {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    format!(
        "{CTCP_TAG} KEYRSP v={PROTO_VERSION} chan={chan} pub={pub_} eph={eph} wnonce={wnonce} wrap={wrap} nonce={nonce} sig={sig}",
        chan = rsp.channel,
        pub_ = hex::encode(rsp.pubkey),
        eph = hex::encode(rsp.ephemeral_pub),
        wnonce = hex::encode(rsp.wrap_nonce),
        wrap = B64.encode(&rsp.wrap_ct),
        nonce = hex::encode(rsp.nonce),
        sig = hex::encode(rsp.sig),
    )
}

#[derive(Debug)]
pub enum HandshakeMsg {
    Req(KeyReq),
    Rsp(KeyRsp),
}

/// Parse a single RPE2E handshake body (what lives inside the `\x01...\x01`
/// CTCP framing). Returns `Ok(None)` when the body does not start with the
/// `RPEE2E` tag, so callers can fall through to other CTCP handling.
pub fn parse(body: &str) -> Result<Option<HandshakeMsg>> {
    let mut parts = body.split_whitespace();
    if parts.next() != Some(CTCP_TAG) {
        return Ok(None);
    }
    let kind = parts
        .next()
        .ok_or_else(|| E2eError::Handshake("missing type".into()))?;
    let rest: Vec<&str> = parts.collect();

    let kv = parse_kv(&rest);
    let v: u8 = kv
        .get("v")
        .ok_or_else(|| E2eError::Handshake("missing v".into()))?
        .parse()
        .map_err(|e| E2eError::Handshake(format!("bad v: {e}")))?;
    if v != PROTO_VERSION {
        return Err(E2eError::Handshake(format!("unsupported version {v}")));
    }

    match kind {
        "KEYREQ" => {
            let channel = kv
                .get("chan")
                .ok_or_else(|| E2eError::Handshake("chan".into()))?
                .to_string();
            let pubkey = hex32(
                kv.get("pub")
                    .ok_or_else(|| E2eError::Handshake("pub".into()))?,
            )?;
            let eph_x25519 = hex32(
                kv.get("eph")
                    .ok_or_else(|| E2eError::Handshake("eph".into()))?,
            )?;
            let nonce = hex16(
                kv.get("nonce")
                    .ok_or_else(|| E2eError::Handshake("nonce".into()))?,
            )?;
            let sig = hex64(
                kv.get("sig")
                    .ok_or_else(|| E2eError::Handshake("sig".into()))?,
            )?;
            Ok(Some(HandshakeMsg::Req(KeyReq {
                channel,
                pubkey,
                eph_x25519,
                nonce,
                sig,
            })))
        }
        "KEYRSP" => {
            use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
            let channel = kv
                .get("chan")
                .ok_or_else(|| E2eError::Handshake("chan".into()))?
                .to_string();
            let pubkey = hex32(
                kv.get("pub")
                    .ok_or_else(|| E2eError::Handshake("pub".into()))?,
            )?;
            let ephemeral_pub = hex32(
                kv.get("eph")
                    .ok_or_else(|| E2eError::Handshake("eph".into()))?,
            )?;
            let wrap_nonce: [u8; NONCE_LEN] = {
                let raw = hex::decode(
                    kv.get("wnonce")
                        .ok_or_else(|| E2eError::Handshake("wnonce".into()))?,
                )?;
                if raw.len() != NONCE_LEN {
                    return Err(E2eError::Handshake(format!(
                        "wnonce len {} != {NONCE_LEN}",
                        raw.len()
                    )));
                }
                let mut arr = [0u8; NONCE_LEN];
                arr.copy_from_slice(&raw);
                arr
            };
            let wrap_ct = B64.decode(
                kv.get("wrap")
                    .ok_or_else(|| E2eError::Handshake("wrap".into()))?,
            )?;
            let nonce = hex16(
                kv.get("nonce")
                    .ok_or_else(|| E2eError::Handshake("nonce".into()))?,
            )?;
            let sig = hex64(
                kv.get("sig")
                    .ok_or_else(|| E2eError::Handshake("sig".into()))?,
            )?;
            Ok(Some(HandshakeMsg::Rsp(KeyRsp {
                channel,
                pubkey,
                ephemeral_pub,
                wrap_nonce,
                wrap_ct,
                nonce,
                sig,
            })))
        }
        _ => Err(E2eError::Handshake(format!("unknown type {kind}"))),
    }
}

fn parse_kv<'a>(fields: &'a [&'a str]) -> HashMap<&'a str, &'a str> {
    let mut out = HashMap::new();
    for f in fields {
        if let Some((k, v)) = f.split_once('=') {
            out.insert(k, v);
        }
    }
    out
}

fn hex32(s: &str) -> Result<[u8; 32]> {
    let raw = hex::decode(s)?;
    if raw.len() != 32 {
        return Err(E2eError::Handshake(format!(
            "expected 32 bytes, got {}",
            raw.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&raw);
    Ok(arr)
}

fn hex16(s: &str) -> Result<[u8; 16]> {
    let raw = hex::decode(s)?;
    if raw.len() != 16 {
        return Err(E2eError::Handshake(format!(
            "expected 16 bytes, got {}",
            raw.len()
        )));
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(&raw);
    Ok(arr)
}

fn hex64(s: &str) -> Result<[u8; 64]> {
    let raw = hex::decode(s)?;
    if raw.len() != 64 {
        return Err(E2eError::Handshake(format!(
            "expected 64 bytes, got {}",
            raw.len()
        )));
    }
    let mut arr = [0u8; 64];
    arr.copy_from_slice(&raw);
    Ok(arr)
}

// ---------- rate limiter ----------

/// Per-peer rate limiter for outgoing KEYREQ. Enforces a minimum 30 second
/// window between requests to the same `peer_handle` to avoid spamming
/// passive/offline peers.
#[derive(Debug, Default)]
pub struct RateLimiter {
    last_sent: HashMap<String, Instant>,
}

impl RateLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if sending to `peer_handle` is allowed right now and
    /// records the attempt.
    pub fn allow_outgoing(&mut self, peer_handle: &str) -> bool {
        let now = Instant::now();
        if let Some(ts) = self.last_sent.get(peer_handle)
            && now.duration_since(*ts) < RATE_LIMIT_WINDOW
        {
            return false;
        }
        self.last_sent.insert(peer_handle.to_string(), now);
        true
    }
}

/// Public accessor: canonical signed payload for KEYREQ (for use by
/// `E2eManager` when signing / verifying).
#[must_use]
pub fn signed_keyreq_payload(
    channel: &str,
    pubkey: &[u8; 32],
    eph_x25519: &[u8; 32],
    nonce: &[u8; 16],
) -> Vec<u8> {
    sig_payload_keyreq(channel, pubkey, eph_x25519, nonce)
}

/// Public accessor: canonical signed payload for KEYRSP. `pubkey` is the
/// responder's long-term Ed25519 identity — see `sig_payload_keyrsp` for
/// the MitM-resistance rationale.
#[must_use]
pub fn signed_keyrsp_payload(
    channel: &str,
    pubkey: &[u8; 32],
    eph_pub: &[u8; 32],
    wrap_nonce: &[u8; NONCE_LEN],
    wrap_ct: &[u8],
    nonce: &[u8; 16],
) -> Vec<u8> {
    sig_payload_keyrsp(channel, pubkey, eph_pub, wrap_nonce, wrap_ct, nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_req() -> KeyReq {
        KeyReq {
            channel: "#x".into(),
            pubkey: [1; 32],
            eph_x25519: [9; 32],
            nonce: [2; 16],
            sig: [3; 64],
        }
    }

    fn sample_rsp() -> KeyRsp {
        KeyRsp {
            channel: "#x".into(),
            pubkey: [12; 32],
            ephemeral_pub: [4; 32],
            wrap_nonce: [5; NONCE_LEN],
            wrap_ct: vec![6, 7, 8, 9],
            nonce: [10; 16],
            sig: [11; 64],
        }
    }

    #[test]
    fn keyreq_roundtrip() {
        let req = sample_req();
        let enc = encode_keyreq(&req);
        let parsed = parse(&enc).unwrap().unwrap();
        match parsed {
            HandshakeMsg::Req(r) => {
                assert_eq!(r.channel, req.channel);
                assert_eq!(r.pubkey, req.pubkey);
                assert_eq!(r.eph_x25519, req.eph_x25519);
                assert_eq!(r.nonce, req.nonce);
                assert_eq!(r.sig, req.sig);
            }
            HandshakeMsg::Rsp(_) => panic!("expected Req"),
        }
    }

    #[test]
    fn keyrsp_roundtrip() {
        let rsp = sample_rsp();
        let enc = encode_keyrsp(&rsp);
        let parsed = parse(&enc).unwrap().unwrap();
        match parsed {
            HandshakeMsg::Rsp(r) => {
                assert_eq!(r.channel, rsp.channel);
                assert_eq!(r.pubkey, rsp.pubkey);
                assert_eq!(r.ephemeral_pub, rsp.ephemeral_pub);
                assert_eq!(r.wrap_nonce, rsp.wrap_nonce);
                assert_eq!(r.wrap_ct, rsp.wrap_ct);
                assert_eq!(r.nonce, rsp.nonce);
                assert_eq!(r.sig, rsp.sig);
            }
            HandshakeMsg::Req(_) => panic!("expected Rsp"),
        }
    }

    #[test]
    fn parse_non_rpee2e_returns_none() {
        assert!(parse("SOMETHING ELSE").unwrap().is_none());
        assert!(parse("").unwrap().is_none());
    }

    #[test]
    fn parse_rejects_unknown_version() {
        let line = format!(
            "{CTCP_TAG} KEYREQ v=9 chan=#x pub={p} eph={e} nonce={n} sig={s}",
            p = hex::encode([0u8; 32]),
            e = hex::encode([0u8; 32]),
            n = hex::encode([0u8; 16]),
            s = hex::encode([0u8; 64]),
        );
        assert!(parse(&line).is_err());
    }

    #[test]
    fn rate_limiter_blocks_within_window() {
        let mut rl = RateLimiter::new();
        assert!(rl.allow_outgoing("~bob@host"));
        assert!(!rl.allow_outgoing("~bob@host"));
        assert!(rl.allow_outgoing("~alice@host"));
    }

    #[test]
    fn keyreq_sig_payload_binds_eph_x25519() {
        let p1 = signed_keyreq_payload("#x", &[1; 32], &[9; 32], &[2; 16]);
        let p2 = signed_keyreq_payload("#x", &[1; 32], &[8; 32], &[2; 16]);
        // Changing only the ephemeral X25519 must change the signed payload;
        // otherwise a MitM could swap it without invalidating the Ed25519 sig.
        assert_ne!(p1, p2);
    }
}
