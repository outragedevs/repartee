//! Outbound E2E gate — the single fail-closed chokepoint deciding whether a
//! message leaves as RPE2E ciphertext, plain text, or not at all.
//!
//! Lives as an `impl AppState` block (not `App`) so the gate is directly
//! unit-testable: `AppState` + an in-memory `E2eManager` fully determine its
//! behavior. Every send path that puts a PRIVMSG on the wire MUST route
//! through here — `handle_plain_message` and the deferred shrink path call
//! [`AppState::e2e_encrypt_or_passthrough`] with their live/captured buffer,
//! while the by-target paths (`/msg`, `/query <nick> <text>`, `/me`, the Lua
//! `ScriptAPI` senders) call [`AppState::e2e_send_plan_for_target`].

use crate::state::AppState;
use crate::state::buffer::{Buffer, BufferType, make_buffer_id};

/// Why [`AppState::e2e_encrypt_or_passthrough`] refused to put a message on
/// the wire. Each variant renders a distinct `[E2E]` line so the user knows
/// whether to wait, retry, or `/e2e off` — an E2E-enabled DM is never
/// silently dropped or downgraded to plaintext.
pub enum E2eRefusal {
    /// PM has E2E enabled but the peer's `ident@host` isn't known yet — they
    /// have not spoken this session and nothing is cached.
    NoPeerHandle,
    /// A keyring read failed while resolving the DM context or checking
    /// whether the context is enabled. An enabled config may exist, so we
    /// refuse rather than risk plaintext.
    KeyringRead,
    /// Encryption itself failed on an already-enabled context — refuse rather
    /// than fall back to cleartext.
    EncryptFailed,
}

impl E2eRefusal {
    /// The themed `[E2E]` line shown to the user when a send is refused.
    pub fn user_message(&self) -> String {
        let body = match self {
            Self::NoPeerHandle => {
                "cannot encrypt PM without peer handle — wait for a message from them first"
            }
            Self::KeyringRead => {
                "cannot encrypt — keyring read failed; message NOT sent (E2E stays on)"
            }
            Self::EncryptFailed => {
                "encryption failed — message NOT sent as plaintext (use /e2e off to send cleartext)"
            }
        };
        format!(
            "{err}[E2E] {body}{rst}",
            err = crate::commands::types::C_ERR,
            rst = crate::commands::types::C_RST,
        )
    }
}

/// The gate's verdict for a by-target send: what goes on the wire and
/// whether the wire lines are RPE2E ciphertext (which changes the caller's
/// echo policy — the server echo of ciphertext is swallowed, so the caller
/// must echo the plaintext itself).
pub struct E2eSendPlan {
    pub wire_lines: Vec<String>,
    pub encrypted: bool,
}

/// Resolve the NOTICE target (a nick) for one pending REKEY distribution
/// entry produced by a lazy key rotation inside `encrypt_outgoing`.
///
/// - **Channel**: the recipient is looked up by server-stamped `ident@host`
///   in the buffer's users map (`None` when the peer left the channel between
///   handshake and rotation — the caller drops the entry with a warning).
/// - **Query (DM)**: the users map is EMPTY for queries, so the map lookup
///   can never resolve — but a DM context (`@<peer_handle>`) has exactly one
///   legitimate recipient: the peer the context is keyed under. Match the
///   entry's handle against the encrypt context and address the NOTICE to
///   the buffer name (the peer's nick). Without this arm every `/e2e rotate`
///   in a query silently dropped the REKEY, leaving the peer on the old
///   incoming key — all subsequent DM ciphertext failed AEAD until a manual
///   re-handshake.
fn rekey_notice_target(
    buf: Option<&Buffer>,
    buffer_type: &BufferType,
    buffer_name: &str,
    context: &str,
    target_handle: &str,
) -> Option<String> {
    if *buffer_type == BufferType::Query {
        // `context_key(nick, handle)` yields `@<handle>` for a non-channel
        // name, so this holds exactly when the entry targets the DM peer.
        return (crate::e2e::context_key(buffer_name, target_handle) == context)
            .then(|| buffer_name.to_string());
    }
    buf.and_then(|b| {
        b.users.values().find_map(|u| {
            let ident = u.ident.as_deref().unwrap_or("");
            let host = u.host.as_deref().unwrap_or("");
            if format!("{ident}@{host}") == target_handle {
                Some(u.nick.clone())
            } else {
                None
            }
        })
    })
}

impl AppState {
    /// Resolve a Query buffer's E2E peer handle: the live server-stamped
    /// `peer_handle` if the peer has spoken this session, otherwise the
    /// keyring's network-scoped cached handle for the nick — the SAME
    /// resolution `/e2e on` and [`Self::e2e_encrypt_or_passthrough`] use, so
    /// the DM keys under the identical `@<handle>` pseudochannel the config
    /// was written under.
    ///
    /// Works even when the buffer no longer exists (a `/close` during a shrink
    /// wait): the connection is recovered from the buffer id itself
    /// (`make_buffer_id` joins as `conn_id/name`), so the keyring cache is
    /// STILL consulted and an enabled `@<handle>` config cannot be silently
    /// missed — missing it would downgrade the deferred send to plaintext.
    /// Returns `Ok(None)` when nothing resolves (no peer handle and no cache
    /// entry), in which case no `@<handle>` config can exist for this peer.
    /// Returns `Err` only when the keyring read itself fails — callers on the
    /// send path MUST treat that as a refusal, never as "no E2E state", so a
    /// transient DB error cannot silently downgrade an E2E-enabled DM to
    /// plaintext.
    pub(crate) fn resolve_query_peer_handle(
        &self,
        buffer_id: &str,
        buffer_name: &str,
    ) -> color_eyre::Result<Option<String>> {
        if let Some(h) = self
            .buffers
            .get(buffer_id)
            .and_then(|b| b.peer_handle.clone())
        {
            return Ok(Some(h));
        }
        let Some(mgr) = self.e2e_manager.as_ref() else {
            return Ok(None);
        };
        let conn_id = self
            .buffers
            .get(buffer_id)
            .map(|b| b.connection_id.clone())
            .or_else(|| {
                buffer_id
                    .split_once('/')
                    .map(|(conn_id, _)| conn_id.to_string())
            });
        let Some(net) = conn_id
            .and_then(|c| self.connections.get(&c))
            .map(|c| c.label.clone())
        else {
            return Ok(None);
        };
        Ok(mgr.keyring().last_handle_for_nick(buffer_name, &net)?)
    }

    /// Return `Some((wire_lines, local_plain))` where `wire_lines` is what
    /// goes out on IRC (encrypted when e2e is enabled on the conversation,
    /// otherwise the plain text split at IRC byte boundaries) and
    /// `local_plain` is what is echoed into the local buffer for the user.
    ///
    /// Returns `Err(E2eRefusal)` to signal a hard refusal: the conversation
    /// is a PM with an E2E config enabled under the `@<peer_handle>`
    /// pseudochannel (spec §6), but we could not safely encrypt — the peer's
    /// `ident@host` isn't known yet, a keyring read failed, or encryption
    /// itself failed. The caller must surface `reason.user_message()` and
    /// drop the message. This prevents silently encrypting under a nick-keyed
    /// row that would collide with other peers sharing the same nick, and —
    /// critically — never downgrades an E2E-enabled DM to plaintext.
    ///
    /// For real IRC channels (`#&!+`) the context is the channel name,
    /// unchanged. For Query buffers the context is `@<peer_handle>`
    /// derived from the buffer's cached handle.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn e2e_encrypt_or_passthrough(
        &mut self,
        buffer_id: &str,
        buffer_name: &str,
        buffer_type: &BufferType,
        text: &str,
        // Pre-resolved Query peer_handle. Inline callers pass None
        // (live buffer is guaranteed present). The deferred shrink
        // path captures the handle at dispatch time and passes
        // Some(handle) so a `/close` during the shrink wait can't
        // make this fall through to plain_passthrough() and leak
        // ciphertext-intended plaintext on the wire.
        captured_peer_handle: Option<&str>,
    ) -> Result<(Vec<String>, String), E2eRefusal> {
        let plain_passthrough = || -> Result<(Vec<String>, String), E2eRefusal> {
            Ok((
                crate::irc::split_irc_message(text, crate::irc::MESSAGE_MAX_BYTES),
                text.to_string(),
            ))
        };

        if text.starts_with(['.', '!']) {
            return plain_passthrough();
        }

        let Some(mgr) = self.e2e_manager.clone() else {
            return plain_passthrough();
        };

        // Derive the keyring context from the conversation. Channels pass
        // through unchanged; PMs require a server-stamped peer handle we
        // cached on the Query buffer at the first incoming PRIVMSG.
        //
        // CRITICAL: look the buffer up by `buffer_id` (the caller's
        // captured target), NOT the active buffer. The deferred
        // shrink path runs this helper from a main-loop arm long after
        // the user typed the message; the active buffer may have
        // moved on. Resolving peer_handle from the active buffer would
        // encrypt under the WRONG peer's session key (or fall back to
        // plain) and produce a confidentiality regression.
        let context: String = match buffer_type {
            BufferType::Channel => buffer_name.to_string(),
            BufferType::Query => {
                // Resolve LIVE first — the buffer's server-stamped peer_handle
                // or the keyring's network-scoped cached handle (the SAME
                // resolution `/e2e on` used to write its config under
                // `@<cached>`; works even after a `/close`, see the helper).
                // Live state is authoritative: a handle migration during a
                // shrink wait moves the config to `@<new>`, and encrypting
                // under a dispatch-time capture of `@<old>` would produce
                // ciphertext the peer can no longer decrypt. The captured
                // handle is only a fallback for when the live resolution
                // finds nothing (e.g. `/e2e forget` or a keyring import
                // cleared the cache row mid-wait) or the retry read fails.
                let peer_handle = match self.resolve_query_peer_handle(buffer_id, buffer_name) {
                    Ok(live) => live.or_else(|| captured_peer_handle.map(str::to_string)),
                    Err(e) if captured_peer_handle.is_some() => {
                        tracing::warn!(
                            "e2e: keyring read failed resolving DM handle for \
                             {buffer_name}: {e}; using the dispatch-time capture"
                        );
                        captured_peer_handle.map(str::to_string)
                    }
                    Err(e) => {
                        // Keyring read failed — an enabled `@<handle>` config
                        // may well exist, so refusing (dropping the message)
                        // is the only safe choice. Never fall through to
                        // plaintext on a read error.
                        tracing::warn!(
                            "e2e: keyring read failed resolving DM handle for \
                             {buffer_name}: {e}; refusing to send rather than \
                             risk plaintext"
                        );
                        return Err(E2eRefusal::KeyringRead);
                    }
                };
                let Some(peer_handle) = peer_handle else {
                    // Nothing resolvable, so no `@<handle>` config can exist.
                    // Refuse only if a legacy bare-nick enabled row exists;
                    // otherwise plain passthrough is safe (no E2E state).
                    let legacy_enabled = match mgr.keyring().get_channel_config(buffer_name) {
                        Ok(cfg) => cfg.is_some_and(|c| c.enabled),
                        Err(e) => {
                            // Same fail-closed rule as the resolution above: a
                            // read error is NOT "no config" — refuse rather
                            // than risk plaintext.
                            tracing::warn!(
                                "e2e: keyring read failed on legacy config for \
                                 {buffer_name}: {e}; refusing to send"
                            );
                            return Err(E2eRefusal::KeyringRead);
                        }
                    };
                    if legacy_enabled {
                        return Err(E2eRefusal::NoPeerHandle);
                    }
                    return plain_passthrough();
                };
                crate::e2e::context_key(buffer_name, &peer_handle)
            }
            // Server/Status/DccChat/Shell/Mentions/Special: E2E does not
            // apply. handle_plain_message already gates messaging on
            // Channel|Query|DccChat, so we only reach this arm if a new
            // sendable type is added in the future — passthrough is the
            // safe default.
            _ => return plain_passthrough(),
        };

        // The enabled check is as load-bearing as the handle resolution: an
        // Err here does NOT mean "not enabled". Swallowing it would send an
        // E2E-enabled conversation as plaintext on a transient DB fault —
        // refuse instead (same rule as `resolve_query_peer_handle`).
        let enabled = match mgr.keyring().get_channel_config(&context) {
            Ok(cfg) => cfg.is_some_and(|c| c.enabled),
            Err(e) => {
                tracing::warn!(
                    "e2e: keyring read failed checking enabled for {context}: {e}; \
                     refusing to send rather than risk plaintext"
                );
                return Err(E2eRefusal::KeyringRead);
            }
        };
        if !enabled {
            return plain_passthrough();
        }
        let result = mgr.encrypt_outgoing(&context, text);

        // Drain any REKEY CTCPs produced by a lazy rotate that happened
        // inside `encrypt_outgoing`. These must go out as NOTICEs to the
        // remaining trusted peers on this conversation. Recipient → nick
        // resolution is per buffer type (see `rekey_notice_target`): channel
        // members via the users map, a DM's single peer via the encrypt
        // context. An unresolvable entry (peer left the channel between
        // handshake and rotation) is dropped with a warning — the peer
        // will re-handshake on next ciphertext if they come back.
        let rekey_sends = mgr.take_pending_rekey_sends();
        if !rekey_sends.is_empty() {
            // Resolve the connection from the caller-passed
            // buffer_id, NOT the active buffer, so REKEY NOTICEs
            // from a deferred-shrink encrypt land on the correct
            // connection. Fall back to splitting buffer_id on '/'
            // — `make_buffer_id` joins as `conn_id/channel`, so the
            // first segment is recoverable even when the buffer was
            // closed during the shrink wait window.
            let conn_id_opt = self
                .buffers
                .get(buffer_id)
                .map(|b| b.connection_id.clone())
                .or_else(|| {
                    buffer_id
                        .split_once('/')
                        .map(|(conn_id, _)| conn_id.to_string())
                });
            if let Some(conn_id) = conn_id_opt {
                // Use the caller-passed `buffer_id` directly. For Channel
                // buffers `buffer_id` already keys to the right buffer; for
                // Query (PM) E2E the `context` we'd reconstruct from is
                // `@<peer_handle>`, which does NOT match how Query buffers
                // are stored (keyed by nick), so reconstructing via
                // `make_buffer_id(&conn_id, &context)` would always miss
                // and silently drop REKEY NOTICEs for every E2E PM.
                for rk in rekey_sends {
                    let nick = rekey_notice_target(
                        self.buffers.get(buffer_id),
                        buffer_type,
                        buffer_name,
                        &context,
                        &rk.target_handle,
                    );
                    let Some(nick) = nick else {
                        tracing::warn!(
                            target_handle = %rk.target_handle,
                            channel = %context,
                            "rekey drop: no nick resolved for handle on current channel"
                        );
                        continue;
                    };
                    self.pending_e2e_sends.push(crate::state::PendingE2eSend {
                        connection_id: conn_id.clone(),
                        target: nick,
                        notice_text: rk.notice_text,
                    });
                }
            } else {
                tracing::warn!("rekey drop: no active buffer to resolve connection");
            }
        }

        match result {
            Ok(wires) => Ok((wires, text.to_string())),
            Err(e) => {
                // E2E is enabled for this context (checked above), so a failed
                // encrypt must NOT downgrade to cleartext — refuse and let the
                // caller tell the user. The message stays unsent unless they
                // explicitly `/e2e off`.
                tracing::warn!("e2e encrypt failed on {context}: {e}; refusing to send plaintext");
                Err(E2eRefusal::EncryptFailed)
            }
        }
    }

    /// Run the outbound E2E gate for a send addressed by TARGET NAME rather
    /// than by an open buffer — `/msg`, `/query <nick> <text>`, `/me`, and
    /// the Lua `ScriptAPI` senders. Classifies the target exactly the way
    /// `context_key` does (a `#&!+` prefix is a channel, anything else a DM)
    /// and delegates to [`Self::e2e_encrypt_or_passthrough`], so a target
    /// with E2E enabled gets ciphertext and a refusal is fail-closed —
    /// typing the message as `/msg <peer> <text>` instead of in the query
    /// buffer must never downgrade an E2E-enabled DM to plaintext.
    pub(crate) fn e2e_send_plan_for_target(
        &mut self,
        conn_id: &str,
        target: &str,
        text: &str,
    ) -> Result<E2eSendPlan, E2eRefusal> {
        let buffer_type = if crate::e2e::is_channel_target(target) {
            BufferType::Channel
        } else {
            BufferType::Query
        };
        let buffer_id = make_buffer_id(conn_id, target);
        let (wire_lines, _plain_echo) =
            self.e2e_encrypt_or_passthrough(&buffer_id, target, &buffer_type, text, None)?;
        let encrypted = wire_lines
            .first()
            .is_some_and(|w| w.starts_with("+RPE2E01"));
        Ok(E2eSendPlan {
            wire_lines,
            encrypted,
        })
    }

    /// `true` when TARGET names an E2E-enabled conversation on `conn_id` —
    /// the read-only advisory twin of the gate, used to warn before sending
    /// content RPE2E cannot protect (a cleartext `/notice`, a DCC CHAT
    /// offer). Read errors resolve to `false`: the advisory must never block
    /// a by-design-plaintext path, only annotate it.
    pub(crate) fn e2e_enabled_for_target(&self, conn_id: &str, target: &str) -> bool {
        let Some(mgr) = self.e2e_manager.as_ref() else {
            return false;
        };
        let context = if crate::e2e::is_channel_target(target) {
            target.to_string()
        } else {
            let buffer_id = make_buffer_id(conn_id, target);
            match self.resolve_query_peer_handle(&buffer_id, target) {
                Ok(Some(handle)) => crate::e2e::context_key(target, &handle),
                Ok(None) => return false,
                Err(e) => {
                    tracing::warn!("e2e: advisory handle resolution failed for {target}: {e}");
                    return false;
                }
            }
        };
        mgr.keyring()
            .get_channel_config(&context)
            .ok()
            .flatten()
            .is_some_and(|c| c.enabled)
    }
}

/// Local-echo request for a gated by-target send.
pub struct GatedEcho<'a> {
    /// Buffer receiving the echo (a missing buffer makes the echo a no-op —
    /// script sends may target conversations that were never opened).
    pub buffer_id: &'a str,
    /// Plaintext to display (the ACTION body for `/me`, the raw text
    /// otherwise — never the CTCP-wrapped wire form).
    pub text: &'a str,
    pub message_type: crate::state::buffer::MessageType,
    /// Also echo when the send went out as PLAINTEXT and echo-message is
    /// off. Command handlers (`/msg`, `/query`, `/me`) pass true — the user
    /// expects to see what they sent. Lua script sends pass false: their
    /// plaintext sends historically never echoed locally, and inventing an
    /// echo would double-render for scripts that already display their own
    /// output. Encrypted sends ALWAYS echo (when the buffer exists): the
    /// server echo is ciphertext and gets swallowed, so without a local
    /// echo the message would vanish from the sender's view.
    pub even_without_encryption: bool,
}

impl super::App {
    /// The by-target twin of `handle_plain_message`'s send pipeline: run the
    /// outbound E2E gate for TARGET, put the resulting wire lines on
    /// `conn_id`, flush any lazy-rotate REKEY NOTICEs, and locally echo per
    /// ECHO. `wire_text` may be a `\x01…\x01` CTCP (e.g. an ACTION) — when
    /// it leaves as plaintext it is sent unsplit, exactly as the legacy
    /// handlers did, because byte-splitting would sever the trailing `\x01`.
    ///
    /// Returns `false` when nothing reached the wire: the gate refused
    /// (fail-closed — the `[E2E]` reason has already been surfaced) or the
    /// connection was down/failed. Callers must NOT retry with plaintext.
    pub(crate) fn send_gated_message(
        &mut self,
        conn_id: &str,
        target: &str,
        wire_text: &str,
        echo: Option<GatedEcho<'_>>,
    ) -> bool {
        use crate::state::buffer::{Message, MessageType};

        let plan = match self.state.e2e_send_plan_for_target(conn_id, target, wire_text) {
            Ok(p) => p,
            Err(reason) => {
                crate::commands::helpers::add_local_event(self, &reason.user_message());
                return false;
            }
        };
        let encrypted = plan.encrypted;
        let wires = if !encrypted && wire_text.starts_with('\x01') {
            vec![wire_text.to_string()]
        } else {
            plan.wire_lines
        };
        for wire in &wires {
            let send_result = self
                .irc_handles
                .get(conn_id)
                .ok_or_else(|| "connection unavailable".to_string())
                .and_then(|h| {
                    h.sender
                        .send_privmsg(target, wire)
                        .map_err(|e| e.to_string())
                });
            if let Err(e) = send_result {
                crate::commands::helpers::add_local_event(
                    self,
                    &format!("Failed to send message: {e}"),
                );
                return false;
            }
        }
        // Lazy-rotate REKEYs queued by the gate must reach the peers on the
        // same tick (same policy as handle_plain_message: drain only after
        // ALL wires went out, never after a failed send).
        if !self.state.pending_e2e_sends.is_empty() {
            self.drain_pending_e2e_sends();
        }
        if let Some(echo) = echo {
            let echo_cap = self
                .state
                .connections
                .get(conn_id)
                .is_some_and(|c| c.enabled_caps.contains("echo-message"));
            if encrypted || (echo.even_without_encryption && !echo_cap) {
                let nick = self
                    .state
                    .connections
                    .get(conn_id)
                    .map(|c| c.nick.clone())
                    .unwrap_or_default();
                let own_mode = self.state.nick_prefix(echo.buffer_id, &nick);
                // Encrypted sends and ACTIONs echo as ONE logical message
                // (matching what the peer renders); plain multi-chunk text
                // echoes per wire chunk, matching the legacy handlers.
                let echo_chunks: Vec<String> =
                    if encrypted || echo.message_type == MessageType::Action {
                        vec![echo.text.to_string()]
                    } else {
                        wires
                    };
                for chunk in echo_chunks {
                    let id = self.state.next_message_id();
                    self.state.add_message(
                        echo.buffer_id,
                        Message {
                            id,
                            timestamp: chrono::Utc::now(),
                            message_type: echo.message_type.clone(),
                            nick: Some(nick.clone()),
                            nick_mode: own_mode.map(|c| c.to_string()),
                            text: chunk,
                            highlight: false,
                            event_key: None,
                            event_params: None,
                            log_msg_id: None,
                            log_ref_id: None,
                            tags: None,
                        },
                    );
                }
            }
        }
        true
    }

    /// Themed `[E2E]` warning that WHAT is about to travel in cleartext to a
    /// target whose conversation has E2E enabled — used for the paths RPE2E
    /// deliberately does not cover (`/notice`, Lua `notice()`, DCC CHAT).
    /// Advisory only: it never blocks the send.
    pub(crate) fn warn_cleartext_to_e2e_target(&mut self, conn_id: &str, target: &str, what: &str) {
        use crate::state::buffer::{Message, MessageType};

        if !self.state.e2e_enabled_for_target(conn_id, target) {
            return;
        }
        let Some(active_id) = self.state.active_buffer_id.as_deref() else {
            return;
        };
        let active_id = active_id.to_string();
        let text = format!(
            "[E2E] {what} to {target} is sent in CLEARTEXT — RPE2E protects PRIVMSGs only"
        );
        let id = self.state.next_message_id();
        self.state.add_local_message(
            &active_id,
            Message {
                id,
                timestamp: chrono::Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: text.clone(),
                highlight: false,
                event_key: Some("e2e_warning".to_string()),
                event_params: Some(vec![text]),
                log_msg_id: None,
                log_ref_id: None,
                tags: None,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test code")]

    use super::*;
    use crate::e2e::keyring::{ChannelConfig, ChannelMode, Keyring};
    use crate::e2e::manager::E2eManager;
    use crate::state::buffer::ActivityLevel;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    // ── E2eRefusal messaging ──

    #[test]
    fn e2e_refusal_messages_are_distinct_and_never_leak_intent() {
        let variants = [
            E2eRefusal::NoPeerHandle,
            E2eRefusal::KeyringRead,
            E2eRefusal::EncryptFailed,
        ];
        let msgs: Vec<String> = variants.iter().map(E2eRefusal::user_message).collect();
        // Every reason renders a themed `[E2E]` line...
        assert!(msgs.iter().all(|m| m.contains("[E2E]")));
        // ...and each cause produces a different message, so the user can tell
        // "peer hasn't spoken" from a keyring/encrypt failure.
        assert_ne!(msgs[0], msgs[1]);
        assert_ne!(msgs[1], msgs[2]);
        assert_ne!(msgs[0], msgs[2]);
        // The failure reasons make clear the message was NOT downgraded.
        assert!(msgs[1].contains("NOT sent"));
        assert!(msgs[2].contains("NOT sent"));
    }

    // ── fixtures ──

    fn make_buf(buffer_type: BufferType, name: &str) -> Buffer {
        Buffer {
            id: make_buffer_id("test", name),
            connection_id: "test".to_string(),
            buffer_type,
            name: name.to_string(),
            messages: std::collections::VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
        }
    }

    fn make_manager() -> E2eManager {
        let conn = crate::storage::db::open_database(false).unwrap();
        E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap()
    }

    fn make_state_with_manager() -> crate::state::AppState {
        let mut state = crate::state::AppState::new();
        state.add_connection(crate::state::connection::Connection {
            id: "test".to_string(),
            label: "TestServer".to_string(),
            status: crate::state::connection::ConnectionStatus::Connected,
            own_handle: None,
            nick: "me".to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: 30,
            next_reconnect: None,
            should_reconnect: true,
            joined_channels: Vec::new(),
            origin_config: crate::config::ServerConfig {
                label: "TestServer".to_string(),
                address: "irc.test.net".to_string(),
                port: 6697,
                tls: true,
                tls_verify: true,
                autoconnect: false,
                channels: vec![],
                nick: None,
                username: None,
                realname: None,
                password: None,
                sasl_user: None,
                sasl_pass: None,
                bind_ip: None,
                encoding: None,
                auto_reconnect: Some(true),
                reconnect_delay: None,
                reconnect_max_retries: None,
                autosendcmd: None,
                sasl_mechanism: None,
                client_cert_path: None,
            },
            local_ip: None,
            enabled_caps: std::collections::HashSet::new(),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            multiline: None,
            batch_ref_counter: 0,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
        });
        state.e2e_manager = Some(Arc::new(make_manager()));
        state
    }

    fn enable(state: &crate::state::AppState, context: &str) {
        state
            .e2e_manager
            .as_ref()
            .unwrap()
            .keyring()
            .set_channel_config(&ChannelConfig {
                channel: context.to_string(),
                enabled: true,
                mode: ChannelMode::Normal,
            })
            .unwrap();
    }

    // ── e2e_send_plan_for_target ──

    #[test]
    fn send_plan_encrypts_dm_via_cached_handle_without_open_buffer() {
        // `/msg bob secret` with no query buffer open: the network-scoped
        // handle cache alone must key the DM to `@<handle>` and encrypt.
        let mut state = make_state_with_manager();
        let keyring = state.e2e_manager.as_ref().unwrap().keyring().clone();
        keyring
            .cache_dm_handle("TestServer", "bob", "~bob@b.host")
            .unwrap();
        enable(&state, "@~bob@b.host");

        let plan = state
            .e2e_send_plan_for_target("test", "bob", "meet at 5")
            .unwrap_or_else(|e| panic!("expected ciphertext plan, got refusal: {}", e.user_message()));
        assert!(plan.encrypted);
        assert!(!plan.wire_lines.is_empty());
        assert!(plan.wire_lines.iter().all(|w| w.starts_with("+RPE2E01")));
        assert!(
            plan.wire_lines.iter().all(|w| !w.contains("meet at 5")),
            "plaintext must never appear in a wire line"
        );

    }

    #[test]
    fn send_plan_encrypts_dm_via_live_buffer_peer_handle() {
        // `/msg bob …` with the query open and the peer's server-stamped
        // handle already on the buffer — the live handle wins.
        let mut state = make_state_with_manager();
        let mut buf = make_buf(BufferType::Query, "bob");
        buf.peer_handle = Some("~bob@b.host".to_string());
        state.add_buffer(buf);
        enable(&state, "@~bob@b.host");

        let plan = state
            .e2e_send_plan_for_target("test", "bob", "secret")
            .unwrap_or_else(|e| panic!("expected ciphertext plan, got refusal: {}", e.user_message()));
        assert!(plan.encrypted);
        assert!(plan.wire_lines.iter().all(|w| w.starts_with("+RPE2E01")));
    }

    #[test]
    fn send_plan_refuses_dm_when_enabled_but_handle_unknown() {
        // A legacy bare-nick enabled row with no resolvable handle must
        // refuse (fail-closed), not fall through to plaintext.
        let mut state = make_state_with_manager();
        enable(&state, "bob");

        let refusal = state
            .e2e_send_plan_for_target("test", "bob", "secret")
            .err()
            .expect("must refuse, never send plaintext");
        assert!(matches!(refusal, E2eRefusal::NoPeerHandle));
    }

    #[test]
    fn send_plan_passthrough_when_no_e2e_state() {
        let mut state = make_state_with_manager();
        let plan = state
            .e2e_send_plan_for_target("test", "carol", "hello there")
            .ok()
            .expect("no E2E state → plain passthrough");
        assert!(!plan.encrypted);
        assert_eq!(plan.wire_lines, vec!["hello there".to_string()]);

    }

    #[test]
    fn send_plan_encrypts_channel_when_enabled() {
        let mut state = make_state_with_manager();
        state.add_buffer(make_buf(BufferType::Channel, "#sec"));
        enable(&state, "#sec");

        let plan = state
            .e2e_send_plan_for_target("test", "#sec", "channel secret")
            .unwrap_or_else(|e| panic!("expected ciphertext plan, got refusal: {}", e.user_message()));
        assert!(plan.encrypted);
        assert!(plan.wire_lines.iter().all(|w| w.starts_with("+RPE2E01")));
    }

    // ── e2e_enabled_for_target (advisory) ──

    #[test]
    fn advisory_reports_enabled_dm_and_stays_quiet_otherwise() {
        let mut state = make_state_with_manager();
        let keyring = state.e2e_manager.as_ref().unwrap().keyring().clone();
        keyring
            .cache_dm_handle("TestServer", "bob", "~bob@b.host")
            .unwrap();
        enable(&state, "@~bob@b.host");
        state.add_buffer(make_buf(BufferType::Channel, "#sec"));
        enable(&state, "#sec");

        assert!(state.e2e_enabled_for_target("test", "bob"));
        assert!(state.e2e_enabled_for_target("test", "#sec"));
        assert!(!state.e2e_enabled_for_target("test", "carol"));
        assert!(!state.e2e_enabled_for_target("test", "#open"));
    }

    // ── rekey_notice_target ──

    #[test]
    fn rekey_target_dm_resolves_peer_from_context_despite_empty_users() {
        // Regression: /e2e rotate in a query rotated the real @<peer> context,
        // but the users-map lookup (empty for queries) dropped the REKEY —
        // the peer kept the old incoming key and all further DM ciphertext
        // failed AEAD until a manual re-handshake.
        let buf = make_buf(BufferType::Query, "bob");
        assert_eq!(
            rekey_notice_target(
                Some(&buf),
                &BufferType::Query,
                "bob",
                "@~bob@b.host",
                "~bob@b.host",
            ),
            Some("bob".to_string()),
            "the DM peer must be addressed by the query buffer's name"
        );
        // Works even when the buffer is gone (closed during a shrink wait) —
        // the context alone identifies the recipient.
        assert_eq!(
            rekey_notice_target(None, &BufferType::Query, "bob", "@~bob@b.host", "~bob@b.host"),
            Some("bob".to_string()),
        );
        // A stray entry for some OTHER handle must not be misdirected to bob.
        assert_eq!(
            rekey_notice_target(
                Some(&buf),
                &BufferType::Query,
                "bob",
                "@~bob@b.host",
                "~mallory@m.host",
            ),
            None,
        );
    }

    #[test]
    fn rekey_target_channel_resolves_via_users_map() {
        let mut buf = make_buf(BufferType::Channel, "#rust");
        buf.users.insert(
            "carol".to_string(),
            crate::state::buffer::NickEntry {
                nick: "carol".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
                ident: Some("~carol".to_string()),
                host: Some("c.host".to_string()),
            },
        );
        assert_eq!(
            rekey_notice_target(
                Some(&buf),
                &BufferType::Channel,
                "#rust",
                "#rust",
                "~carol@c.host",
            ),
            Some("carol".to_string()),
        );
        // Peer left the channel between handshake and rotation → dropped.
        assert_eq!(
            rekey_notice_target(
                Some(&buf),
                &BufferType::Channel,
                "#rust",
                "#rust",
                "~dave@d.host",
            ),
            None,
        );
    }
}
