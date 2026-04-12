//! Portable JSON export / import for the RPE2E keyring.
//!
//! Produces a single JSON document containing the local identity, every known
//! peer, every incoming and outgoing session key, every channel config, and
//! every autotrust rule. All binary fields are serialised as lowercase hex so
//! that the file diffs cleanly and can be hand-inspected.
//!
//! WARNING: session keys are written in plaintext. The export is an escape
//! hatch for backup / migration between installs of the same user; it must be
//! protected with filesystem ACLs (the export writer sets mode `0600` on Unix)
//! and NEVER redistributed. A future revision may add passphrase-wrapping but
//! v1 keeps the on-disk format transparent on purpose.
//!
//! The JSON schema is versioned (`version: 1`). Import validates the entire
//! structure (version, hex lengths, enum strings) *before* touching the
//! keyring, so a partial/failed import never leaves the caller's keyring in a
//! half-populated state.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::e2e::crypto::aead::SessionKey;
use crate::e2e::crypto::fingerprint::Fingerprint;
use crate::e2e::error::{E2eError, Result};
use crate::e2e::keyring::{
    ChannelConfig, ChannelMode, IncomingSession, Keyring, OutgoingSession, PeerRecord, TrustStatus,
};

/// Current schema version. Bump when a field is added or semantics change.
pub const EXPORT_VERSION: u32 = 1;

// ─── Top-level document ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Portable {
    pub version: u32,
    #[serde(rename = "exportedAt")]
    pub exported_at: i64,
    pub identity: PortableIdentity,
    pub peers: Vec<PortablePeer>,
    #[serde(rename = "incomingSessions")]
    pub incoming_sessions: Vec<PortableIncoming>,
    #[serde(rename = "outgoingSessions")]
    pub outgoing_sessions: Vec<PortableOutgoing>,
    pub channels: Vec<PortableChannel>,
    pub autotrust: Vec<PortableAutotrust>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableIdentity {
    pub pubkey: String,
    pub privkey: String,
    pub fingerprint: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortablePeer {
    pub fingerprint: String,
    pub pubkey: String,
    #[serde(rename = "lastHandle")]
    pub last_handle: Option<String>,
    #[serde(rename = "lastNick")]
    pub last_nick: Option<String>,
    #[serde(rename = "firstSeen")]
    pub first_seen: i64,
    #[serde(rename = "lastSeen")]
    pub last_seen: i64,
    #[serde(rename = "globalStatus")]
    pub global_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableIncoming {
    pub handle: String,
    pub channel: String,
    pub fingerprint: String,
    pub sk: String,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableOutgoing {
    pub channel: String,
    pub sk: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "pendingRotation")]
    pub pending_rotation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableChannel {
    pub channel: String,
    pub enabled: bool,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableAutotrust {
    pub scope: String,
    #[serde(rename = "handlePattern")]
    pub handle_pattern: String,
}

// ─── Summaries returned to the caller ────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct ExportSummary {
    pub peers: usize,
    pub incoming: usize,
    pub outgoing: usize,
    pub channels: usize,
    pub autotrust: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ImportSummary {
    pub identity: bool,
    pub peers: usize,
    pub incoming: usize,
    pub outgoing: usize,
    pub channels: usize,
    pub autotrust: usize,
}

// ─── Path helpers ────────────────────────────────────────────────────────────

/// Expand a leading `~` to the user's home directory. If `path` is relative
/// and does not start with `~`, resolve it against the current working
/// directory; this keeps user-typed relative paths intuitive (they land next
/// to wherever the user ran the client from).
pub fn expand_path(path: &str) -> Result<PathBuf> {
    if let Some(stripped) = path.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| E2eError::Keyring("cannot expand ~: $HOME is not set".to_string()))?;
        Ok(PathBuf::from(home).join(stripped))
    } else if path == "~" {
        let home = std::env::var("HOME")
            .map_err(|_| E2eError::Keyring("cannot expand ~: $HOME is not set".to_string()))?;
        Ok(PathBuf::from(home))
    } else {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            Ok(p)
        } else {
            let cwd = std::env::current_dir()
                .map_err(|e| E2eError::Keyring(format!("cannot get cwd: {e}")))?;
            Ok(cwd.join(p))
        }
    }
}

// ─── Export ──────────────────────────────────────────────────────────────────

/// Dump the entire keyring to a JSON file at `path`.
///
/// On Unix the file is created with mode `0600`. Overwrites any existing file
/// at the destination. Session keys are written in plaintext — the caller is
/// responsible for warning the user.
pub fn export_to_path(keyring: &Keyring, path: &Path) -> Result<ExportSummary> {
    let doc = build_portable(keyring)?;
    let json = serde_json::to_string_pretty(&doc)
        .map_err(|e| E2eError::Keyring(format!("serialize export json: {e}")))?;

    write_private_file(path, json.as_bytes())?;

    Ok(ExportSummary {
        peers: doc.peers.len(),
        incoming: doc.incoming_sessions.len(),
        outgoing: doc.outgoing_sessions.len(),
        channels: doc.channels.len(),
        autotrust: doc.autotrust.len(),
    })
}

fn build_portable(keyring: &Keyring) -> Result<Portable> {
    let identity_row = keyring.load_identity()?.ok_or_else(|| {
        E2eError::Keyring(
            "no identity present — nothing to export (run the client once to generate one)"
                .to_string(),
        )
    })?;
    let (pubkey, privkey, fingerprint, created_at) = identity_row;

    let peers = keyring.list_all_peers()?;
    let incoming = keyring.list_all_incoming_sessions()?;
    let outgoing = keyring.list_all_outgoing_sessions()?;
    let channels = keyring.list_all_channel_configs()?;
    let autotrust = keyring.list_autotrust()?;

    Ok(Portable {
        version: EXPORT_VERSION,
        exported_at: chrono::Utc::now().timestamp(),
        identity: PortableIdentity {
            pubkey: hex::encode(pubkey),
            privkey: hex::encode(privkey),
            fingerprint: hex::encode(fingerprint),
            created_at,
        },
        peers: peers
            .into_iter()
            .map(|p| PortablePeer {
                fingerprint: hex::encode(p.fingerprint),
                pubkey: hex::encode(p.pubkey),
                last_handle: p.last_handle,
                last_nick: p.last_nick,
                first_seen: p.first_seen,
                last_seen: p.last_seen,
                global_status: p.global_status.as_str().to_string(),
            })
            .collect(),
        incoming_sessions: incoming
            .into_iter()
            .map(|s| PortableIncoming {
                handle: s.handle,
                channel: s.channel,
                fingerprint: hex::encode(s.fingerprint),
                sk: hex::encode(s.sk),
                status: s.status.as_str().to_string(),
                created_at: s.created_at,
            })
            .collect(),
        outgoing_sessions: outgoing
            .into_iter()
            .map(|o| PortableOutgoing {
                channel: o.channel,
                sk: hex::encode(o.sk),
                created_at: o.created_at,
                pending_rotation: o.pending_rotation,
            })
            .collect(),
        channels: channels
            .into_iter()
            .map(|c| PortableChannel {
                channel: c.channel,
                enabled: c.enabled,
                mode: c.mode.as_str().to_string(),
            })
            .collect(),
        autotrust: autotrust
            .into_iter()
            .map(|(scope, handle_pattern)| PortableAutotrust {
                scope,
                handle_pattern,
            })
            .collect(),
    })
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| E2eError::Keyring(format!("open {}: {e}", path.display())))?;
    file.write_all(bytes)
        .map_err(|e| E2eError::Keyring(format!("write {}: {e}", path.display())))?;
    file.sync_all()
        .map_err(|e| E2eError::Keyring(format!("fsync {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| E2eError::Keyring(format!("open {}: {e}", path.display())))?;
    file.write_all(bytes)
        .map_err(|e| E2eError::Keyring(format!("write {}: {e}", path.display())))?;
    file.sync_all()
        .map_err(|e| E2eError::Keyring(format!("fsync {}: {e}", path.display())))?;
    Ok(())
}

// ─── Import ──────────────────────────────────────────────────────────────────

/// Parse a portable JSON document from `path`, validate it in full, then
/// apply every row to the caller's keyring in a single pass. Validation is
/// strict: version mismatch, wrong hex lengths, and unknown enum strings all
/// cause `Err` before the keyring is touched.
pub fn import_from_path(keyring: &Keyring, path: &Path) -> Result<ImportSummary> {
    let mut file =
        File::open(path).map_err(|e| E2eError::Keyring(format!("open {}: {e}", path.display())))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|e| E2eError::Keyring(format!("read {}: {e}", path.display())))?;

    let doc: Portable =
        serde_json::from_str(&raw).map_err(|e| E2eError::Keyring(format!("parse json: {e}")))?;

    // Validate EVERYTHING first — no writes on failure.
    let validated = validate(&doc)?;

    // Apply.
    keyring.save_identity(
        &validated.identity.pubkey,
        &validated.identity.privkey,
        &validated.identity.fingerprint,
        validated.identity.created_at,
    )?;

    for p in &validated.peers {
        keyring.upsert_peer(p)?;
    }
    for s in &validated.incoming {
        keyring.set_incoming_session(s)?;
    }
    for o in &validated.outgoing {
        keyring.set_outgoing_session(&o.channel, &o.sk, o.created_at)?;
        if o.pending_rotation {
            keyring.mark_outgoing_pending_rotation(&o.channel)?;
        }
    }
    for c in &validated.channels {
        keyring.set_channel_config(c)?;
    }
    for (scope, pat, created_at) in &validated.autotrust {
        keyring.add_autotrust(scope, pat, *created_at)?;
    }

    Ok(ImportSummary {
        identity: true,
        peers: validated.peers.len(),
        incoming: validated.incoming.len(),
        outgoing: validated.outgoing.len(),
        channels: validated.channels.len(),
        autotrust: validated.autotrust.len(),
    })
}

/// Intermediate form after validation — all strings have been decoded into
/// their binary representations and the enum strings have been converted to
/// typed values. Produced atomically: any failure here aborts before we touch
/// the keyring.
struct Validated {
    identity: ValidatedIdentity,
    peers: Vec<PeerRecord>,
    incoming: Vec<IncomingSession>,
    outgoing: Vec<OutgoingSession>,
    channels: Vec<ChannelConfig>,
    autotrust: Vec<(String, String, i64)>,
}

struct ValidatedIdentity {
    pubkey: [u8; 32],
    privkey: [u8; 32],
    fingerprint: Fingerprint,
    created_at: i64,
}

fn validate(doc: &Portable) -> Result<Validated> {
    if doc.version != EXPORT_VERSION {
        return Err(E2eError::Keyring(format!(
            "unsupported export version: got {}, expected {}",
            doc.version, EXPORT_VERSION
        )));
    }

    let identity = ValidatedIdentity {
        pubkey: parse_hex_array::<32>("identity.pubkey", &doc.identity.pubkey)?,
        privkey: parse_hex_array::<32>("identity.privkey", &doc.identity.privkey)?,
        fingerprint: parse_hex_array::<16>("identity.fingerprint", &doc.identity.fingerprint)?,
        created_at: doc.identity.created_at,
    };

    let mut peers = Vec::with_capacity(doc.peers.len());
    for (idx, p) in doc.peers.iter().enumerate() {
        peers.push(PeerRecord {
            fingerprint: parse_hex_array::<16>(
                &format!("peers[{idx}].fingerprint"),
                &p.fingerprint,
            )?,
            pubkey: parse_hex_array::<32>(&format!("peers[{idx}].pubkey"), &p.pubkey)?,
            last_handle: p.last_handle.clone(),
            last_nick: p.last_nick.clone(),
            first_seen: p.first_seen,
            last_seen: p.last_seen,
            global_status: parse_trust_status(
                &format!("peers[{idx}].globalStatus"),
                &p.global_status,
            )?,
        });
    }

    let mut incoming = Vec::with_capacity(doc.incoming_sessions.len());
    for (idx, s) in doc.incoming_sessions.iter().enumerate() {
        incoming.push(IncomingSession {
            handle: s.handle.clone(),
            channel: s.channel.clone(),
            fingerprint: parse_hex_array::<16>(
                &format!("incomingSessions[{idx}].fingerprint"),
                &s.fingerprint,
            )?,
            sk: parse_hex_array::<32>(&format!("incomingSessions[{idx}].sk"), &s.sk)?,
            status: parse_trust_status(&format!("incomingSessions[{idx}].status"), &s.status)?,
            created_at: s.created_at,
        });
    }

    let mut outgoing: Vec<OutgoingSession> = Vec::with_capacity(doc.outgoing_sessions.len());
    for (idx, o) in doc.outgoing_sessions.iter().enumerate() {
        let sk: SessionKey = parse_hex_array::<32>(&format!("outgoingSessions[{idx}].sk"), &o.sk)?;
        outgoing.push(OutgoingSession {
            channel: o.channel.clone(),
            sk,
            created_at: o.created_at,
            pending_rotation: o.pending_rotation,
        });
    }

    let mut channels = Vec::with_capacity(doc.channels.len());
    for (idx, c) in doc.channels.iter().enumerate() {
        channels.push(ChannelConfig {
            channel: c.channel.clone(),
            enabled: c.enabled,
            mode: parse_channel_mode(&format!("channels[{idx}].mode"), &c.mode)?,
        });
    }

    let mut autotrust = Vec::with_capacity(doc.autotrust.len());
    for a in &doc.autotrust {
        autotrust.push((a.scope.clone(), a.handle_pattern.clone(), doc.exported_at));
    }

    Ok(Validated {
        identity,
        peers,
        incoming,
        outgoing,
        channels,
        autotrust,
    })
}

fn parse_hex_array<const N: usize>(field: &str, s: &str) -> Result<[u8; N]> {
    let bytes =
        hex::decode(s).map_err(|e| E2eError::Keyring(format!("{field}: invalid hex ({e})")))?;
    if bytes.len() != N {
        return Err(E2eError::Keyring(format!(
            "{field}: expected {N} bytes (hex length {}), got {}",
            N * 2,
            bytes.len()
        )));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn parse_trust_status(field: &str, s: &str) -> Result<TrustStatus> {
    match s {
        "pending" => Ok(TrustStatus::Pending),
        "trusted" => Ok(TrustStatus::Trusted),
        "revoked" => Ok(TrustStatus::Revoked),
        other => Err(E2eError::Keyring(format!(
            "{field}: unknown status '{other}' (expected pending|trusted|revoked)"
        ))),
    }
}

fn parse_channel_mode(field: &str, s: &str) -> Result<ChannelMode> {
    match s {
        "auto-accept" | "auto" => Ok(ChannelMode::AutoAccept),
        "normal" => Ok(ChannelMode::Normal),
        "quiet" => Ok(ChannelMode::Quiet),
        other => Err(E2eError::Keyring(format!(
            "{field}: unknown mode '{other}' (expected auto-accept|normal|quiet)"
        ))),
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_array_rejects_wrong_length() {
        let r: Result<[u8; 32]> = parse_hex_array::<32>("x.field", "aabb");
        let err = r.err().unwrap();
        let msg = format!("{err}");
        assert!(
            msg.contains("x.field"),
            "message should name the field: {msg}"
        );
        assert!(msg.contains("expected 32 bytes"), "{msg}");
    }

    #[test]
    fn parse_hex_array_rejects_non_hex() {
        let r: Result<[u8; 16]> = parse_hex_array::<16>("y.field", "not-hex-value!");
        assert!(r.is_err());
    }

    #[test]
    fn parse_trust_status_known_values() {
        assert_eq!(
            parse_trust_status("f", "pending").unwrap(),
            TrustStatus::Pending
        );
        assert_eq!(
            parse_trust_status("f", "trusted").unwrap(),
            TrustStatus::Trusted
        );
        assert_eq!(
            parse_trust_status("f", "revoked").unwrap(),
            TrustStatus::Revoked
        );
    }

    #[test]
    fn parse_trust_status_rejects_unknown() {
        let err = parse_trust_status("f", "bogus").err().unwrap();
        assert!(format!("{err}").contains("bogus"));
    }

    #[test]
    fn parse_channel_mode_known_values() {
        assert_eq!(
            parse_channel_mode("f", "auto-accept").unwrap(),
            ChannelMode::AutoAccept
        );
        assert_eq!(
            parse_channel_mode("f", "auto").unwrap(),
            ChannelMode::AutoAccept
        );
        assert_eq!(
            parse_channel_mode("f", "normal").unwrap(),
            ChannelMode::Normal
        );
        assert_eq!(
            parse_channel_mode("f", "quiet").unwrap(),
            ChannelMode::Quiet
        );
    }

    #[test]
    fn expand_path_absolute_passthrough() {
        let p = expand_path("/tmp/out.json").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/out.json"));
    }

    #[test]
    fn expand_path_tilde() {
        let home = std::env::var("HOME").unwrap();
        let p = expand_path("~/out.json").unwrap();
        assert_eq!(p, PathBuf::from(home).join("out.json"));
    }
}
