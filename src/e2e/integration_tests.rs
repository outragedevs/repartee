//! Integration tests for the RPE2E handshake + encrypt/decrypt pipeline.
//!
//! These live inside the crate (not in `tests/`) because `repartee` is a
//! binary crate with no `lib.rs`, so external integration tests cannot
//! reach private modules. The file is `#[cfg(test)]`-gated by
//! `src/e2e/mod.rs`.

#![allow(clippy::unwrap_used, reason = "test code")]

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::e2e::crypto::fingerprint::fingerprint_hex;
use crate::e2e::error::E2eError;
use crate::e2e::keyring::{ChannelConfig, ChannelMode, IncomingSession, Keyring, TrustStatus};
use crate::e2e::manager::{DecryptOutcome, E2eManager, ReverifyOutcome, TrustChange};

const SCHEMA: &str = "
CREATE TABLE e2e_identity (id INTEGER PRIMARY KEY CHECK (id = 1), pubkey BLOB NOT NULL, privkey BLOB NOT NULL, fingerprint BLOB NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE e2e_peers (fingerprint BLOB PRIMARY KEY, pubkey BLOB NOT NULL, last_handle TEXT, last_nick TEXT, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, global_status TEXT NOT NULL DEFAULT 'pending');
CREATE TABLE e2e_dm_handle_cache (network TEXT NOT NULL, nick TEXT NOT NULL COLLATE NOCASE, handle TEXT NOT NULL, PRIMARY KEY (network, nick));
CREATE TABLE e2e_outgoing_sessions (channel TEXT PRIMARY KEY, sk BLOB NOT NULL, created_at INTEGER NOT NULL, pending_rotation INTEGER NOT NULL DEFAULT 0);
CREATE TABLE e2e_incoming_sessions (handle TEXT NOT NULL, channel TEXT NOT NULL, fingerprint BLOB NOT NULL, sk BLOB NOT NULL, status TEXT NOT NULL DEFAULT 'pending', created_at INTEGER NOT NULL, prev_sk BLOB, prev_created_at INTEGER, PRIMARY KEY (handle, channel));
CREATE TABLE e2e_seen_rekeys (fingerprint BLOB NOT NULL, channel TEXT NOT NULL, nonce BLOB NOT NULL, seen_at INTEGER NOT NULL, PRIMARY KEY (fingerprint, channel, nonce));
CREATE TABLE e2e_channel_config (channel TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 0, mode TEXT NOT NULL DEFAULT 'normal');
CREATE TABLE e2e_autotrust (id INTEGER PRIMARY KEY AUTOINCREMENT, scope TEXT NOT NULL, handle_pattern TEXT NOT NULL, created_at INTEGER NOT NULL, UNIQUE(scope, handle_pattern));
CREATE TABLE e2e_outgoing_recipients (channel TEXT NOT NULL, handle TEXT NOT NULL, fingerprint BLOB NOT NULL, first_sent_at INTEGER NOT NULL, PRIMARY KEY (channel, handle));
";

fn make_manager() -> E2eManager {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    let kr = Keyring::new(Arc::new(Mutex::new(conn)));
    E2eManager::load_or_init(kr).unwrap()
}

/// Build a fresh manager with a custom replay-window tolerance. Used by
/// the `ts_tolerance` configuration test to verify the per-instance value
/// is honoured by `decrypt_incoming` (spec §5.4 + G11 gap 4).
fn make_manager_with_tolerance(ts_tolerance_secs: i64) -> E2eManager {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    let kr = Keyring::new(Arc::new(Mutex::new(conn)));
    let cfg = crate::config::E2eConfig {
        enabled: true,
        default_mode: "normal".to_string(),
        ts_tolerance_secs,
    };
    E2eManager::load_or_init_with_config(kr, &cfg).unwrap()
}

fn enable_channel(mgr: &E2eManager, channel: &str, mode: ChannelMode) {
    mgr.keyring()
        .set_channel_config(&ChannelConfig {
            channel: channel.to_string(),
            enabled: true,
            mode,
        })
        .unwrap();
}

#[test]
fn full_handshake_and_encrypted_exchange() {
    let alice = make_manager();
    let bob = make_manager();

    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let alice_handle = "~alice@a.host";
    let bob_handle = "~bob@b.host";

    // Bob initiates KEYREQ to Alice.
    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice
        .handle_keyreq(bob_handle, &req)
        .unwrap()
        .expect("auto-accept should produce a KEYRSP");

    // Bob receives KEYRSP from Alice and installs alice's outgoing session
    // as his incoming session for alice. `rsp.pubkey` carries Alice's
    // long-term identity, so bob doesn't need it out-of-band.
    bob.handle_keyrsp(alice_handle, &rsp).unwrap();

    // Alice encrypts a message for #x.
    let wire_lines = alice.encrypt_outgoing("#x", "hello bob").unwrap();
    assert_eq!(wire_lines.len(), 1);

    // Bob decrypts using the session installed by the handshake.
    let out = bob
        .decrypt_incoming(alice_handle, "#x", &wire_lines[0])
        .unwrap();
    match out {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "hello bob"),
        other => panic!("expected Plaintext, got {other:?}"),
    }
}

#[test]
fn long_encrypted_action_splits_into_complete_ctcp_frames() {
    // Chunks decrypt and render STANDALONE (no reassembly, spec §6), so a
    // `\x01ACTION …\x01` frame longer than one chunk must be split into
    // independent, individually-framed ACTIONs before encryption — never
    // fragmented mid-frame, which would render as raw \x01 garbage on the
    // peer. A short action stays a single frame; an overlong non-ACTION
    // CTCP is refused rather than silently shipped broken.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    let alice_handle = "~alice@a.host";
    let bob_handle = "~bob@b.host";
    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq(bob_handle, &req).unwrap().unwrap();
    bob.handle_keyrsp(alice_handle, &rsp).unwrap();

    let body = "zażółć gęślą jaźń ".repeat(30); // multi-byte, ≫ one chunk
    let frame = format!("\x01ACTION {body}\x01");
    let wire_lines = alice.encrypt_outgoing_ctcp("#x", &frame).unwrap();
    assert!(wire_lines.len() > 1, "an overlong action must split");

    let mut reassembled = String::new();
    for wire in &wire_lines {
        let DecryptOutcome::Plaintext(plain) =
            bob.decrypt_incoming(alice_handle, "#x", wire).unwrap()
        else {
            panic!("every piece must decrypt standalone");
        };
        let piece_body = plain
            .strip_prefix("\x01ACTION ")
            .and_then(|p| p.strip_suffix('\x01'))
            .unwrap_or_else(|| panic!("piece is not a complete CTCP ACTION frame: {plain:?}"));
        assert!(
            !piece_body.contains('\x01'),
            "no stray CTCP framing inside a piece"
        );
        reassembled.push_str(piece_body);
    }
    assert_eq!(reassembled, body, "no bytes lost or duplicated across pieces");

    let short = alice.encrypt_outgoing_ctcp("#x", "\x01ACTION waves\x01").unwrap();
    assert_eq!(short.len(), 1, "a short action stays a single frame");

    let overlong_other = format!("\x01VERSION {}\x01", "x".repeat(300));
    assert!(
        alice.encrypt_outgoing_ctcp("#x", &overlong_other).is_err(),
        "an overlong non-ACTION CTCP must refuse, not fragment"
    );
}

#[test]
fn dm_round_trip_recipient_keyed_both_directions() {
    // A DM is recipient-keyed: the context for a message is the RECIPIENT's
    // handle. So Alice->Bob uses @<bob_handle> on BOTH sides and Bob->Alice
    // uses @<alice_handle>. The recipient stamps the KEYREQ c= with its own
    // handle; both sides use it verbatim. This proves the manager needs no
    // change for DMs — only the IRC callers must compute the recipient-keyed
    // context (own handle on decrypt, peer handle on encrypt).
    let alice = make_manager();
    let bob = make_manager();
    let alice_handle = "~alice@a.host";
    let bob_handle = "~bob@b.host";
    let ctx_to_bob = format!("@{bob_handle}"); // recipient = Bob
    let ctx_to_alice = format!("@{alice_handle}"); // recipient = Alice

    enable_channel(&alice, &ctx_to_bob, ChannelMode::AutoAccept);
    enable_channel(&bob, &ctx_to_alice, ChannelMode::AutoAccept);

    // Dir Alice->Bob: Bob (the recipient) sends a KEYREQ stamped with his own
    // handle; Alice responds; Bob installs the incoming session under @<bob>.
    let req = bob.build_keyreq(&ctx_to_bob).unwrap();
    let rsp = alice.handle_keyreq(bob_handle, &req).unwrap().unwrap();
    bob.handle_keyrsp(alice_handle, &rsp).unwrap();
    let wire = alice.encrypt_outgoing(&ctx_to_bob, "hi bob").unwrap();
    match bob
        .decrypt_incoming(alice_handle, &ctx_to_bob, &wire[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "hi bob"),
        other => panic!("A->B: expected Plaintext, got {other:?}"),
    }

    // Dir Bob->Alice: symmetric, recipient = Alice.
    let req2 = alice.build_keyreq(&ctx_to_alice).unwrap();
    let rsp2 = bob.handle_keyreq(alice_handle, &req2).unwrap().unwrap();
    alice.handle_keyrsp(bob_handle, &rsp2).unwrap();
    let wire2 = bob.encrypt_outgoing(&ctx_to_alice, "hey alice").unwrap();
    match alice
        .decrypt_incoming(bob_handle, &ctx_to_alice, &wire2[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "hey alice"),
        other => panic!("B->A: expected Plaintext, got {other:?}"),
    }
}

#[test]
fn chghost_migrates_enabled_dm_config_to_new_context() {
    // A peer's vhost/host change must carry the E2E policy to the new
    // pseudochannel, or the next DM would key under @<new>, find no config,
    // and go plaintext.
    let mgr = make_manager();
    let old_ctx = "@~bob@old.host";
    let new_ctx = "@~bob@user/bob";
    enable_channel(&mgr, old_ctx, ChannelMode::Normal);

    assert!(
        crate::irc::events::migrate_dm_e2e_config(&mgr, old_ctx, new_ctx, false).unwrap(),
        "an enabled config must report as migrated"
    );

    // New context is now enabled; the old context stays enabled too (copy,
    // not move) so the encrypt path's cached @<old> fallback still encrypts
    // rather than going plaintext — and the TOFU last_handle is left untouched.
    assert!(
        mgr.keyring()
            .get_channel_config(new_ctx)
            .unwrap()
            .is_some_and(|c| c.enabled),
        "config must follow the peer to the new handle"
    );
    assert!(
        mgr.keyring()
            .get_channel_config(old_ctx)
            .unwrap()
            .is_some_and(|c| c.enabled),
        "old-context config stays enabled (copy, not move)"
    );

    // A disabled old config is NOT migrated (no policy to preserve).
    let mgr2 = make_manager();
    mgr2.keyring()
        .set_channel_config(&ChannelConfig {
            channel: old_ctx.to_string(),
            enabled: false,
            mode: ChannelMode::Normal,
        })
        .unwrap();
    assert!(
        !crate::irc::events::migrate_dm_e2e_config(&mgr2, old_ctx, new_ctx, false).unwrap(),
        "a disabled config must report as not migrated"
    );
    assert!(
        mgr2.keyring().get_channel_config(new_ctx).unwrap().is_none(),
        "a disabled config must not be migrated"
    );
}

#[test]
fn dm_keyreq_skips_reciprocal_but_channel_keeps_it() {
    // DM: Alice receives Bob's KEYREQ for the DM context (Bob's own handle).
    // No reciprocal is queued — the reverse (Alice-receives-from-Bob) direction
    // is keyed by Alice's OWN handle, which the manager can't derive from
    // req.channel; it self-heals via the live auto-KEYREQ instead. Building one
    // under @<bob> would orphan rows and spend the shared rate-limit slot.
    let alice = make_manager();
    let bob = make_manager();
    let bob_handle = "~bob@b.host";
    let dm_ctx = format!("@{bob_handle}");
    enable_channel(&alice, &dm_ctx, ChannelMode::AutoAccept);
    let dm_req = bob.build_keyreq(&dm_ctx).unwrap();
    alice.handle_keyreq(bob_handle, &dm_req).unwrap();
    assert!(
        alice.take_pending_outbound_keyreqs().is_empty(),
        "a DM KEYREQ must not build a mis-keyed reciprocal"
    );

    // Channel: the proactive reciprocal IS still built (own == peer == channel).
    let carol = make_manager();
    let dave = make_manager();
    let dave_handle = "~dave@d.host";
    enable_channel(&carol, "#x", ChannelMode::AutoAccept);
    let chan_req = dave.build_keyreq("#x").unwrap();
    carol.handle_keyreq(dave_handle, &chan_req).unwrap();
    assert!(
        !carol.take_pending_outbound_keyreqs().is_empty(),
        "a channel KEYREQ still builds the proactive reciprocal"
    );
}

#[test]
fn strict_handle_check_rejects_wrong_sender() {
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq("~bob@b.host", &req).unwrap().unwrap();
    bob.handle_keyrsp("~alice@a.host", &rsp).unwrap();

    let wire = alice.encrypt_outgoing("#x", "secret").unwrap();

    // Bob tries to decrypt claiming the sender is someone else. Because the
    // session is indexed by (handle, channel) there is no entry for the
    // imposter handle, so we get MissingKey rather than a silent decrypt.
    let outcome = bob
        .decrypt_incoming("~mallory@evil.host", "#x", &wire[0])
        .unwrap();
    match outcome {
        DecryptOutcome::MissingKey { .. } => {}
        other => panic!("expected MissingKey, got {other:?}"),
    }
}

#[test]
fn revoke_then_lazy_rotate_locks_out_revoked_peer() {
    let alice = make_manager();
    let bob = make_manager();
    let carol = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    enable_channel(&carol, "#x", ChannelMode::AutoAccept);

    // Bob and Carol both handshake with Alice.
    for (peer, peer_handle) in [(&bob, "~bob@b.host"), (&carol, "~carol@c.host")] {
        let req = peer.build_keyreq("#x").unwrap();
        let rsp = alice.handle_keyreq(peer_handle, &req).unwrap().unwrap();
        peer.handle_keyrsp("~alice@a.host", &rsp).unwrap();
    }

    // Alice sends msg 1; bob decrypts successfully.
    let w1 = alice.encrypt_outgoing("#x", "msg-1").unwrap();
    let bob_out1 = bob.decrypt_incoming("~alice@a.host", "#x", &w1[0]).unwrap();
    assert!(matches!(
        &bob_out1,
        DecryptOutcome::Plaintext(s) if s == "msg-1"
    ));

    // Alice marks outgoing session pending_rotation (simulating /e2e revoke bob).
    alice
        .keyring()
        .mark_outgoing_pending_rotation("#x")
        .unwrap();

    // Alice sends msg 2 → lazy rotate generates a fresh key; bob's old
    // incoming session key no longer decrypts it.
    let w2 = alice.encrypt_outgoing("#x", "msg-2").unwrap();
    let bob_out2 = bob.decrypt_incoming("~alice@a.host", "#x", &w2[0]).unwrap();
    assert!(matches!(bob_out2, DecryptOutcome::Rejected(_)));
}

#[test]
fn export_import_roundtrip() {
    let alice = make_manager();
    enable_channel(&alice, "#rust", ChannelMode::AutoAccept);

    // Populate with peers, an outgoing session, AND an incoming session via
    // a reciprocal handshake: bob asks alice for her key (alice gets a peer
    // record + an outgoing session), and alice asks bob for his key (alice
    // then installs an incoming session keyed to bob's handle).
    let bob = make_manager();
    enable_channel(&bob, "#rust", ChannelMode::AutoAccept);

    let req_from_bob = bob.build_keyreq("#rust").unwrap();
    let rsp_to_bob = alice
        .handle_keyreq("~bob@b.host", &req_from_bob)
        .unwrap()
        .unwrap();
    bob.handle_keyrsp("~alice@a.host", &rsp_to_bob).unwrap();

    let req_from_alice = alice.build_keyreq("#rust").unwrap();
    let rsp_to_alice = bob
        .handle_keyreq("~alice@a.host", &req_from_alice)
        .unwrap()
        .unwrap();
    alice.handle_keyrsp("~bob@b.host", &rsp_to_alice).unwrap();

    // Add more state to alice: an autotrust rule and a scheduled rotation
    // so pending_rotation is exercised through the round-trip.
    alice
        .keyring()
        .add_autotrust("#rust", "~carol@*", 1_000)
        .unwrap();
    alice
        .keyring()
        .mark_outgoing_pending_rotation("#rust")
        .unwrap();

    // Sanity-check alice before export.
    let alice_peers_before = alice.keyring().list_all_peers().unwrap();
    let alice_incoming_before = alice.keyring().list_all_incoming_sessions().unwrap();
    let alice_outgoing_before = alice.keyring().list_all_outgoing_sessions().unwrap();
    let alice_channels_before = alice.keyring().list_all_channel_configs().unwrap();
    let alice_autotrust_before = alice.keyring().list_autotrust().unwrap();
    let alice_identity_before = alice.keyring().load_identity().unwrap().unwrap();
    assert!(!alice_peers_before.is_empty());
    assert!(!alice_incoming_before.is_empty());
    assert!(!alice_outgoing_before.is_empty());
    assert!(alice_outgoing_before[0].pending_rotation);

    // Export alice to a tempfile.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let summary = crate::e2e::portable::export_to_path(alice.keyring(), tmp.path()).unwrap();
    assert!(summary.peers >= 1);
    assert!(summary.incoming >= 1);
    assert!(summary.outgoing >= 1);
    assert_eq!(summary.channels, alice_channels_before.len());
    assert_eq!(summary.autotrust, alice_autotrust_before.len());

    // Confirm file permissions are 0600 on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = std::fs::metadata(tmp.path()).unwrap().mode();
        assert_eq!(mode & 0o777, 0o600, "export should be 0600");
    }

    // Create a fresh manager with an empty keyring; import alice's snapshot
    // and verify every table matches row-for-row.
    let carol = make_manager();
    let imported = crate::e2e::portable::import_from_path(carol.keyring(), tmp.path()).unwrap();
    assert!(imported.identity);
    assert_eq!(imported.peers, alice_peers_before.len());
    assert_eq!(imported.incoming, alice_incoming_before.len());
    assert_eq!(imported.outgoing, alice_outgoing_before.len());
    assert_eq!(imported.channels, alice_channels_before.len());
    assert_eq!(imported.autotrust, alice_autotrust_before.len());

    // Identity is byte-exact (including the private key).
    let carol_identity = carol.keyring().load_identity().unwrap().unwrap();
    assert_eq!(carol_identity.0, alice_identity_before.0); // pubkey
    assert_eq!(carol_identity.1, alice_identity_before.1); // privkey
    assert_eq!(carol_identity.2, alice_identity_before.2); // fingerprint
    assert_eq!(carol_identity.3, alice_identity_before.3); // created_at

    // Peers, sessions, channels, autotrust all have matching cardinality
    // and content.
    assert_eq!(
        alice.keyring().list_all_peers().unwrap().len(),
        carol.keyring().list_all_peers().unwrap().len()
    );
    assert_eq!(
        alice.keyring().list_all_channel_configs().unwrap().len(),
        carol.keyring().list_all_channel_configs().unwrap().len()
    );
    assert_eq!(
        alice.keyring().list_autotrust().unwrap(),
        carol.keyring().list_autotrust().unwrap()
    );

    // The outgoing session's pending_rotation flag round-trips correctly.
    let carol_outgoing = carol.keyring().list_all_outgoing_sessions().unwrap();
    assert_eq!(carol_outgoing.len(), 1);
    assert!(carol_outgoing[0].pending_rotation);
    assert_eq!(carol_outgoing[0].sk, alice_outgoing_before[0].sk);

    // The incoming session key matches bit-for-bit.
    let carol_incoming = carol.keyring().list_all_incoming_sessions().unwrap();
    assert_eq!(carol_incoming.len(), 1);
    assert_eq!(carol_incoming[0].sk, alice_incoming_before[0].sk);
    assert_eq!(
        carol_incoming[0].fingerprint,
        alice_incoming_before[0].fingerprint
    );
    assert_eq!(carol_incoming[0].handle, alice_incoming_before[0].handle);
    assert_eq!(carol_incoming[0].status, alice_incoming_before[0].status);
}

#[test]
fn import_rejects_bad_version() {
    let alice = make_manager();
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        tmp.path(),
        r#"{"version": 99, "exportedAt": 0, "identity": {"pubkey":"aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899","privkey":"aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899","fingerprint":"aabbccddeeff00112233445566778899","createdAt":0}, "peers":[], "incomingSessions":[], "outgoingSessions":[], "channels":[], "autotrust":[]}"#,
    )
    .unwrap();
    let err = crate::e2e::portable::import_from_path(alice.keyring(), tmp.path())
        .err()
        .unwrap();
    let msg = format!("{err}");
    assert!(msg.contains("version"), "error should name version: {msg}");
}

#[test]
fn import_rejects_truncated_hex() {
    let alice = make_manager();
    let tmp = tempfile::NamedTempFile::new().unwrap();
    // Truncated identity fingerprint (only 15 bytes / 30 hex chars instead of 16 bytes / 32 hex chars).
    std::fs::write(
        tmp.path(),
        r#"{"version": 1, "exportedAt": 0, "identity": {"pubkey":"aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899","privkey":"aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899","fingerprint":"aabbccddeeff001122334455667788","createdAt":0}, "peers":[], "incomingSessions":[], "outgoingSessions":[], "channels":[], "autotrust":[]}"#,
    )
    .unwrap();
    let result = crate::e2e::portable::import_from_path(alice.keyring(), tmp.path());
    let msg = format!("{}", result.err().unwrap());
    assert!(
        msg.contains("identity.fingerprint"),
        "error should name field: {msg}"
    );
}

#[test]
fn import_rejects_bad_status_enum() {
    let alice = make_manager();
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let body = r#"{"version":1,"exportedAt":0,"identity":{"pubkey":"aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899","privkey":"aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899","fingerprint":"aabbccddeeff00112233445566778899","createdAt":0},"peers":[{"fingerprint":"aabbccddeeff00112233445566778899","pubkey":"aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899","lastHandle":null,"lastNick":null,"firstSeen":0,"lastSeen":0,"globalStatus":"wibble"}],"incomingSessions":[],"outgoingSessions":[],"channels":[],"autotrust":[]}"#;
    std::fs::write(tmp.path(), body).unwrap();
    let result = crate::e2e::portable::import_from_path(alice.keyring(), tmp.path());
    let err = result.err().unwrap();
    let msg = format!("{err}");
    assert!(msg.contains("wibble"), "error should name bad value: {msg}");
    assert!(
        msg.contains("globalStatus"),
        "error should name field: {msg}"
    );
}

#[test]
fn import_replaces_existing_keyring_state() {
    let alice = make_manager();
    let bob = make_manager();
    let carol = make_manager();

    enable_channel(&alice, "#rust", ChannelMode::AutoAccept);
    enable_channel(&bob, "#rust", ChannelMode::AutoAccept);
    let req = bob.build_keyreq("#rust").unwrap();
    let rsp = alice.handle_keyreq("~bob@b.host", &req).unwrap().unwrap();
    bob.handle_keyrsp("~alice@a.host", &rsp).unwrap();
    alice
        .keyring()
        .add_autotrust("#rust", "~bob@*", 1_000)
        .unwrap();

    enable_channel(&carol, "#stale", ChannelMode::AutoAccept);
    carol
        .keyring()
        .add_autotrust("#stale", "~stale@*", 2_000)
        .unwrap();
    carol
        .keyring()
        .set_outgoing_session("#stale", &[7u8; 32], 2_000)
        .unwrap();
    carol
        .keyring()
        .set_incoming_session(&IncomingSession {
            handle: "~stale@old.host".to_string(),
            channel: "#stale".to_string(),
            fingerprint: [9u8; 16],
            sk: [8u8; 32],
            status: TrustStatus::Trusted,
            created_at: 2_000,
        })
        .unwrap();
    carol
        .keyring()
        .cache_dm_handle("StaleNet", "stale", "~stale@old.host")
        .unwrap();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    crate::e2e::portable::export_to_path(alice.keyring(), tmp.path()).unwrap();
    crate::e2e::portable::import_from_path(carol.keyring(), tmp.path()).unwrap();

    assert_eq!(
        carol.keyring().list_all_peers().unwrap().len(),
        alice.keyring().list_all_peers().unwrap().len()
    );
    assert_eq!(
        carol.keyring().list_all_incoming_sessions().unwrap().len(),
        alice.keyring().list_all_incoming_sessions().unwrap().len()
    );
    assert_eq!(
        carol.keyring().list_all_outgoing_sessions().unwrap().len(),
        alice.keyring().list_all_outgoing_sessions().unwrap().len()
    );
    assert_eq!(
        carol.keyring().list_all_channel_configs().unwrap().len(),
        alice.keyring().list_all_channel_configs().unwrap().len()
    );
    assert_eq!(
        carol.keyring().list_autotrust().unwrap(),
        alice.keyring().list_autotrust().unwrap()
    );
    assert!(
        carol
            .keyring()
            .get_incoming_session("~stale@old.host", "#stale")
            .unwrap()
            .is_none()
    );
    assert!(
        carol
            .keyring()
            .get_outgoing_session("#stale")
            .unwrap()
            .is_none()
    );
    assert!(
        carol
            .keyring()
            .get_channel_config("#stale")
            .unwrap()
            .is_none()
    );
    // The stale DM handle cache from the previous keyring must be cleared, so
    // an imported keyring can't resolve a DM under the old handle.
    assert_eq!(
        carol
            .keyring()
            .last_handle_for_nick("stale", "StaleNet")
            .unwrap(),
        None
    );
}

#[test]
fn handshake_with_changed_fingerprint_is_rejected() {
    // Alice fully handshakes with Bob, then Bob's identity is regenerated
    // (a fresh E2eManager → fresh Ed25519 keypair) and he tries a second
    // KEYREQ from the same `~bob@b.host`. Alice's handle_keyreq must
    // reject the new key with HandleMismatch and surface a
    // FingerprintChanged TrustChange.
    let alice = make_manager();
    let bob_original = make_manager();

    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob_original, "#x", ChannelMode::AutoAccept);

    let bob_handle = "~bob@b.host";
    let alice_handle = "~alice@a.host";

    // First handshake completes normally; Alice records bob's fingerprint.
    let req1 = bob_original.build_keyreq("#x").unwrap();
    let rsp1 = alice.handle_keyreq(bob_handle, &req1).unwrap().unwrap();
    bob_original.handle_keyrsp(alice_handle, &rsp1).unwrap();
    // Pending changes should be empty so far.
    assert!(alice.take_pending_trust_changes().is_empty());

    // Bob's identity is regenerated (simulate: new E2eManager → new
    // Ed25519 keypair and thus new fingerprint).
    let bob_new = make_manager();
    enable_channel(&bob_new, "#x", ChannelMode::AutoAccept);
    // Ensure fingerprints actually differ — if by astronomical luck they
    // collide, the test premise is broken and we should know.
    assert_ne!(bob_original.fingerprint(), bob_new.fingerprint());

    // Second KEYREQ from the same handle with a brand-new key.
    let req2 = bob_new.build_keyreq("#x").unwrap();
    let err = alice
        .handle_keyreq(bob_handle, &req2)
        .expect_err("must reject changed fingerprint");
    match err {
        E2eError::HandleMismatch { .. } => {}
        other => panic!("expected HandleMismatch, got {other:?}"),
    }

    // A FingerprintChanged notice was recorded.
    let notices = alice.take_pending_trust_changes();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].handle, bob_handle);
    assert_eq!(notices[0].channel, "#x");
    match &notices[0].change {
        TrustChange::FingerprintChanged {
            handle,
            old_fp,
            new_fp,
        } => {
            assert_eq!(handle, bob_handle);
            assert_eq!(*old_fp, bob_original.fingerprint());
            assert_eq!(*new_fp, bob_new.fingerprint());
        }
        other => panic!("expected FingerprintChanged, got {other:?}"),
    }
}

#[test]
fn handshake_with_changed_handle_is_warned() {
    // Bob completes a handshake from one handle, then reconnects from a
    // different host (same identity, new handle) and does another KEYREQ.
    // Alice must refuse to auto-accept and surface a HandleChanged notice.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let home_handle = "~bob@home.host";
    let vpn_handle = "~bob@vpn.mullvad.net";

    // First handshake: pins bob's fp under the home handle.
    let req1 = bob.build_keyreq("#x").unwrap();
    alice.handle_keyreq(home_handle, &req1).unwrap().unwrap();
    assert!(alice.take_pending_trust_changes().is_empty());

    // Second KEYREQ from bob (same identity — we reuse the same E2eManager,
    // so the long-term Ed25519 is unchanged) but under a different handle.
    let req2 = bob.build_keyreq("#x").unwrap();
    assert_eq!(req1.pubkey, req2.pubkey, "same bob → same pubkey");
    let err = alice
        .handle_keyreq(vpn_handle, &req2)
        .expect_err("must refuse to auto-rebind handle");
    match err {
        E2eError::HandleMismatch { .. } => {}
        other => panic!("expected HandleMismatch, got {other:?}"),
    }

    let notices = alice.take_pending_trust_changes();
    assert_eq!(notices.len(), 1);
    match &notices[0].change {
        TrustChange::HandleChanged {
            old_handle,
            new_handle,
            fingerprint,
        } => {
            assert_eq!(old_handle, home_handle);
            assert_eq!(new_handle, vpn_handle);
            assert_eq!(*fingerprint, bob.fingerprint());
        }
        other => panic!("expected HandleChanged, got {other:?}"),
    }
}

#[test]
fn revoked_peer_cannot_re_handshake() {
    // Alice handshakes with Bob, then revokes him. A subsequent KEYREQ
    // must be refused and surface a Revoked notice.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let bob_handle = "~bob@b.host";

    // Initial handshake pins bob.
    let req1 = bob.build_keyreq("#x").unwrap();
    alice.handle_keyreq(bob_handle, &req1).unwrap().unwrap();
    assert!(alice.take_pending_trust_changes().is_empty());

    // Revoke bob in alice's keyring.
    alice
        .keyring()
        .upsert_peer(&crate::e2e::keyring::PeerRecord {
            fingerprint: bob.fingerprint(),
            pubkey: bob.identity_pub(),
            last_handle: Some(bob_handle.to_string()),
            last_nick: None,
            first_seen: 0,
            last_seen: 1,
            global_status: TrustStatus::Revoked,
        })
        .unwrap();

    // Bob retries a KEYREQ; alice must refuse and warn.
    let req2 = bob.build_keyreq("#x").unwrap();
    let err = alice
        .handle_keyreq(bob_handle, &req2)
        .expect_err("must refuse revoked peer");
    match err {
        E2eError::HandleMismatch { .. } => {}
        other => panic!("expected HandleMismatch for revoked, got {other:?}"),
    }

    let notices = alice.take_pending_trust_changes();
    assert_eq!(notices.len(), 1);
    match &notices[0].change {
        TrustChange::Revoked {
            handle,
            fingerprint,
        } => {
            assert_eq!(handle, bob_handle);
            assert_eq!(*fingerprint, bob.fingerprint());
        }
        other => panic!("expected Revoked, got {other:?}"),
    }
}

#[test]
fn keyrsp_carries_pubkey_for_self_contained_verification() {
    // Regression-style: proves that the initiator (`bob`) no longer needs
    // to know Alice's Ed25519 pubkey out-of-band. The KEYRSP itself
    // carries `rsp.pubkey`, which `handle_keyrsp` uses to verify the
    // signature and TOFU-pin the peer.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq("~bob@b.host", &req).unwrap().unwrap();
    assert_eq!(rsp.pubkey, alice.identity_pub());
    bob.handle_keyrsp("~alice@a.host", &rsp).unwrap();

    let wires = alice.encrypt_outgoing("#x", "hello").unwrap();
    let out = bob
        .decrypt_incoming("~alice@a.host", "#x", &wires[0])
        .unwrap();
    match out {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "hello"),
        other => panic!("expected Plaintext, got {other:?}"),
    }
}

// === PM pseudochannel (spec §6) =========================================
//
// The pseudochannel is an opaque label both sides agree on for a PM
// conversation. Spec §6 prescribes the shape `@<peer_handle>` so two
// peers who share a nick across different hosts end up under distinct
// keyring rows. At the KEYRING layer the channel is just an opaque
// string — AEAD AAD binds it and `(handle, channel)` is the primary
// lookup key — so the tests below drive the full handshake/encrypt/
// decrypt pipeline with the same concrete pseudochannel label on both
// ends, exactly as the existing channel tests do with `"#x"`.
//
// The asymmetric-label wiring between two real repartee instances
// (Alice's "bob" buffer vs Bob's "alice" buffer) is the responsibility
// of the app layer in `app::input` / `irc::events`, which synthesizes
// the label from the local `Buffer.peer_handle` on each side. The G9
// refactor introduces the helper (`context_key`) and the buffer field;
// reconciling the two sides' labels into a single shared one over the
// wire is the focus of a later gate.

#[test]
fn pm_pseudochannel_round_trip() {
    // A PM round-trips under the `@<peer_handle>` pseudochannel exactly
    // like a real channel — there is nothing special about the key's
    // shape from the keyring's point of view.
    let alice = make_manager();
    let bob = make_manager();

    // Shared pseudochannel label for this PM conversation. Both sides
    // agree on the string; the only thing that makes it a "pseudo"
    // channel is the leading `@` and the fact that it encodes a raw
    // userhost.
    let pm_ctx = "@~bob@b.host";
    enable_channel(&alice, pm_ctx, ChannelMode::AutoAccept);
    enable_channel(&bob, pm_ctx, ChannelMode::AutoAccept);

    let alice_handle = "~alice@a.host";
    let bob_handle = "~bob@b.host";

    // Bob initiates the handshake against the shared pseudochannel.
    let req = bob.build_keyreq(pm_ctx).unwrap();
    let rsp = alice
        .handle_keyreq(bob_handle, &req)
        .unwrap()
        .expect("auto-accept should produce a KEYRSP");
    bob.handle_keyrsp(alice_handle, &rsp).unwrap();

    // Alice encrypts a PM under the pseudochannel — note the key is
    // `@~bob@b.host`, not the bare nick `"bob"`.
    let wires = alice.encrypt_outgoing(pm_ctx, "hi bob").unwrap();
    assert_eq!(wires.len(), 1);

    // Bob decrypts under the same pseudochannel.
    let out = bob
        .decrypt_incoming(alice_handle, pm_ctx, &wires[0])
        .unwrap();
    match out {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "hi bob"),
        other => panic!("expected Plaintext, got {other:?}"),
    }
}

#[test]
fn pm_pseudochannel_distinguishes_same_nick_different_host() {
    // The exact collision spec §6 warns about: two peers whose IRC
    // nicks are both "alice" but whose userhost differs. Under a
    // bare-nick scheme both sessions would overwrite each other in a
    // single `channel = "alice"` row. Under the pseudochannel shape
    // they sit under distinct `@~alice@home.host` and
    // `@~alice@vpn.host` keys — each alice decrypts only her own
    // bob-encrypted traffic, and a cross-attempt must not succeed.
    //
    // Handshake direction note: `build_keyreq` makes the caller the
    // *initiator* and the responder the one whose outgoing session
    // gets installed on both sides (responder's outgoing, initiator's
    // incoming). To test "bob sends one session key per alice", each
    // alice initiates a keyreq (so bob is responder and therefore gets
    // a distinct outgoing session keyed under each alice's
    // pseudochannel), and messages then flow `bob → aliceN`.
    let bob = make_manager();

    let alice1_handle = "~alice@home.host";
    let alice2_handle = "~alice@vpn.host";
    let pm_ctx_alice1 = format!("@{alice1_handle}");
    let pm_ctx_alice2 = format!("@{alice2_handle}");

    enable_channel(&bob, &pm_ctx_alice1, ChannelMode::AutoAccept);
    enable_channel(&bob, &pm_ctx_alice2, ChannelMode::AutoAccept);

    let alice1 = make_manager();
    let alice2 = make_manager();
    enable_channel(&alice1, &pm_ctx_alice1, ChannelMode::AutoAccept);
    enable_channel(&alice2, &pm_ctx_alice2, ChannelMode::AutoAccept);

    let bob_handle = "~bob@b.host";

    // alice1 → bob handshake, then alice2 → bob handshake. After each,
    // bob has a distinct outgoing session keyed on the respective
    // pseudochannel, and the corresponding alice has an incoming
    // session under `(bob_handle, pm_ctx_aliceN)`.
    let r1 = alice1.build_keyreq(&pm_ctx_alice1).unwrap();
    let rsp1 = bob.handle_keyreq(alice1_handle, &r1).unwrap().unwrap();
    alice1.handle_keyrsp(bob_handle, &rsp1).unwrap();

    let r2 = alice2.build_keyreq(&pm_ctx_alice2).unwrap();
    let rsp2 = bob.handle_keyreq(alice2_handle, &r2).unwrap().unwrap();
    alice2.handle_keyrsp(bob_handle, &rsp2).unwrap();

    // Bob encrypts one PM for each alice under the matching
    // pseudochannel. Under the old nick-keyed scheme bob's outgoing
    // session table would only have one row (`"alice"`), and the
    // second handshake would clobber the first; under pseudochannels
    // the rows are distinct.
    let w1 = bob
        .encrypt_outgoing(&pm_ctx_alice1, "msg for alice1")
        .unwrap();
    let w2 = bob
        .encrypt_outgoing(&pm_ctx_alice2, "msg for alice2")
        .unwrap();

    // Each alice decrypts her own message using her incoming session.
    let o1 = alice1
        .decrypt_incoming(bob_handle, &pm_ctx_alice1, &w1[0])
        .unwrap();
    let o2 = alice2
        .decrypt_incoming(bob_handle, &pm_ctx_alice2, &w2[0])
        .unwrap();
    match (&o1, &o2) {
        (DecryptOutcome::Plaintext(s1), DecryptOutcome::Plaintext(s2)) => {
            assert_eq!(s1, "msg for alice1");
            assert_eq!(s2, "msg for alice2");
        }
        _ => panic!("expected two Plaintext outcomes, got {o1:?} / {o2:?}"),
    }

    // Cross-attempt: alice2 tries to decrypt alice1's ciphertext, but
    // her keyring has no `(bob_handle, pm_ctx_alice1)` row — she only
    // has her own pseudochannel. The outcome is MissingKey, which is
    // the collision isolation the pseudochannel scheme exists to
    // provide.
    let cross = alice2
        .decrypt_incoming(bob_handle, &pm_ctx_alice1, &w1[0])
        .unwrap();
    assert!(
        !matches!(cross, DecryptOutcome::Plaintext(_)),
        "cross-decrypt must not succeed, got {cross:?}"
    );
}

// ---------- G10: lazy rotate distribution via REKEY ----------

#[test]
fn lazy_rotate_distributes_new_key_to_remaining_trusted_peers() {
    let alice = make_manager();
    let bob = make_manager();
    let carol = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    enable_channel(&carol, "#x", ChannelMode::AutoAccept);

    // Bob and Carol each handshake with Alice so alice has two trusted
    // incoming sessions on #x — alice's perspective.
    for (peer, ph) in [(&bob, "~bob@b.host"), (&carol, "~carol@c.host")] {
        let req = peer.build_keyreq("#x").unwrap();
        let rsp = alice.handle_keyreq(ph, &req).unwrap().unwrap();
        peer.handle_keyrsp("~alice@a.host", &rsp).unwrap();
    }

    // Alice sends msg 1; both bob and carol decrypt with alice's v1 key.
    let w1 = alice.encrypt_outgoing("#x", "msg-1").unwrap();
    match bob.decrypt_incoming("~alice@a.host", "#x", &w1[0]).unwrap() {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "msg-1"),
        other => panic!("bob decrypt 1: {other:?}"),
    }
    match carol
        .decrypt_incoming("~alice@a.host", "#x", &w1[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "msg-1"),
        other => panic!("carol decrypt 1: {other:?}"),
    }

    // /e2e revoke bob: flip bob's incoming row to Revoked, drop bob from
    // the outgoing-recipients list, and mark outgoing pending_rotation
    // (the three steps e2e_revoke performs).
    alice
        .keyring()
        .update_incoming_status("~bob@b.host", "#x", TrustStatus::Revoked)
        .unwrap();
    alice
        .keyring()
        .remove_outgoing_recipient("#x", "~bob@b.host")
        .unwrap();
    alice
        .keyring()
        .mark_outgoing_pending_rotation("#x")
        .unwrap();

    // Alice sends msg 2 — this triggers lazy rotate and builds a REKEY
    // for every remaining trusted peer (carol only; bob is revoked).
    let w2 = alice.encrypt_outgoing("#x", "msg-2").unwrap();
    let rekeys = alice.take_pending_rekey_sends();
    assert_eq!(
        rekeys.len(),
        1,
        "expected exactly one rekey targeted at carol"
    );
    let (target, ctcp) = (&rekeys[0].target_handle, &rekeys[0].notice_text);
    assert_eq!(target, "~carol@c.host");

    // Carol consumes the REKEY: strip CTCP framing, parse, handle.
    let inner = ctcp
        .strip_prefix('\x01')
        .and_then(|s| s.strip_suffix('\x01'))
        .unwrap();
    let parsed = crate::e2e::handshake::parse(inner).unwrap().unwrap();
    let rk = match parsed {
        crate::e2e::handshake::HandshakeMsg::Rekey(r) => r,
        other => panic!("expected Rekey, got {other:?}"),
    };
    carol.handle_rekey("~alice@a.host", &rk).unwrap();

    // After the rekey, carol can decrypt msg-2 but bob cannot.
    match carol
        .decrypt_incoming("~alice@a.host", "#x", &w2[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "msg-2"),
        other => panic!("carol decrypt 2: {other:?}"),
    }
    let bob_out = bob.decrypt_incoming("~alice@a.host", "#x", &w2[0]).unwrap();
    assert!(
        !matches!(bob_out, DecryptOutcome::Plaintext(_)),
        "bob should not be able to decrypt msg-2 after revoke+rotate"
    );

    // And the rotate queue is empty on the next send (no new revokes).
    let _w3 = alice.encrypt_outgoing("#x", "msg-3").unwrap();
    assert!(
        alice.take_pending_rekey_sends().is_empty(),
        "no further rekey sends should be queued after rotation completes"
    );
}

#[test]
fn rekey_rejects_unknown_peer() {
    let alice = make_manager();
    let stranger = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);

    // Stranger fabricates a REKEY for alice. They've never handshaked,
    // so alice's classify_peer_change returns New → reject.
    let sk = crate::e2e::crypto::aead::generate_session_key().unwrap();
    // Borrow the private builder indirectly: have stranger install a fake
    // peer record referencing alice's pubkey so they can sign+encrypt.
    // The check is on alice's side (TrustChange::New → reject).
    let fake_peer = crate::e2e::keyring::PeerRecord {
        fingerprint: [0u8; 16],
        pubkey: alice.identity_pub(),
        last_handle: Some("~alice@a.host".into()),
        last_nick: None,
        first_seen: 0,
        last_seen: 0,
        global_status: TrustStatus::Trusted,
    };
    let _ = fake_peer; // only used for documentation; stranger's build path below is the real one.

    // We build a real REKEY by having stranger call their own internal
    // build_rekey_for_peer pointed at alice — but that method is private.
    // Instead: round-trip by making stranger handshake with alice first
    // to establish the peer row, then forging a new REKEY on top.
    // Simpler: exercise the unknown-peer path by directly calling
    // handle_rekey with a manually-assembled payload signed by stranger.
    let eph_sk_bytes = [9u8; 32];
    let eph_sk = x25519_dalek::StaticSecret::from(eph_sk_bytes);
    let eph_pub = x25519_dalek::PublicKey::from(&eph_sk).to_bytes();
    let alice_x = crate::e2e::crypto::ecdh::ed25519_pub_to_x25519(&alice.identity_pub()).unwrap();
    let shared = eph_sk.diffie_hellman(&x25519_dalek::PublicKey::from(alice_x));
    let info = "RPE2E01-REKEY:#x";
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(b"RPE2E01-WRAP"), shared.as_bytes());
    let mut wrap_key = [0u8; 32];
    hk.expand(info.as_bytes(), &mut wrap_key).unwrap();
    let (wrap_nonce, wrap_ct) =
        crate::e2e::crypto::aead::encrypt(&wrap_key, info.as_bytes(), &sk).unwrap();
    let nonce = [7u8; 16];
    let pubkey = stranger.identity_pub();
    let sig_payload = crate::e2e::handshake::signed_keyrekey_payload(
        "#x",
        &pubkey,
        &eph_pub,
        &wrap_nonce,
        &wrap_ct,
        &nonce,
    );
    // Sign using stranger's identity — we reach into the internal API via
    // a reciprocal manager build_keyreq + extract signing key? Not exposed.
    // Easier: use stranger to sign via their own build_keyreq path and
    // reuse the manager test helper. But build_rekey_for_peer is private.
    //
    // Instead: assert via the shorter proof that *some* unknown-peer
    // REKEY arriving at alice is rejected — we can simulate this by
    // constructing a KeyRekey with a *valid self-signed* shape for a
    // freshly-generated identity (stranger). Alice's classify_peer_change
    // returns New → E2eError::Handshake.
    //
    // Build signature via stranger's signing_key. The signing_key itself
    // isn't exposed publicly, so use the crypto::sig helper through the
    // public signing_key accessor on Identity. For tests we re-derive an
    // Identity from stranger's secret_bytes... but that accessor is also
    // not public. Instead use the `crypto::identity::Identity::from_secret_bytes`
    // path via the manager's helper. Easiest: round-trip through
    // crate::e2e::crypto::identity::Identity directly.
    let stranger_id = crate::e2e::crypto::identity::Identity::from_secret_bytes(&{
        // We need stranger's secret — since we don't expose it, construct
        // a brand-new identity specifically for this forgery test.
        let mut seed = [0u8; 32];
        rand::fill(&mut seed);
        seed
    });
    let stranger_pub = stranger_id.public_bytes();
    let sig_payload2 = crate::e2e::handshake::signed_keyrekey_payload(
        "#x",
        &stranger_pub,
        &eph_pub,
        &wrap_nonce,
        &wrap_ct,
        &nonce,
    );
    let sig_bytes = crate::e2e::crypto::sig::sign(stranger_id.signing_key(), &sig_payload2);

    let fake_rekey = crate::e2e::handshake::KeyRekey {
        channel: "#x".into(),
        pubkey: stranger_pub,
        eph_pub,
        wrap_nonce,
        wrap_ct,
        nonce,
        sig: sig_bytes,
    };
    let res = alice.handle_rekey("~stranger@s.host", &fake_rekey);
    assert!(
        res.is_err(),
        "REKEY from unknown peer must be rejected, got {res:?}"
    );
    // And no incoming session row should have been created.
    let sess = alice
        .keyring()
        .get_incoming_session("~stranger@s.host", "#x")
        .unwrap();
    assert!(sess.is_none(), "no session installed from unknown REKEY");
    let _ = sig_payload; // silence unused-let-binding when tests compile with warnings-as-errors
}

// ---------- G10: autotrust enforcement ----------

#[test]
fn autotrust_global_accepts_matching_peer_in_normal_mode() {
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::Normal);
    enable_channel(&bob, "#x", ChannelMode::Normal);
    // Alice marks anyone on `*.trusted.org` as autotrust globally.
    alice
        .keyring()
        .add_autotrust("global", "*@*.trusted.org", 100)
        .unwrap();

    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice
        .handle_keyreq("~bob@shell.trusted.org", &req)
        .unwrap()
        .expect("autotrust should have forced AutoAccept semantics");
    // Bob can consume the KEYRSP and decrypt alice's subsequent messages.
    bob.handle_keyrsp("~alice@a.host", &rsp).unwrap();
    let w = alice.encrypt_outgoing("#x", "hi bob").unwrap();
    let out = bob.decrypt_incoming("~alice@a.host", "#x", &w[0]).unwrap();
    assert!(matches!(out, DecryptOutcome::Plaintext(s) if s == "hi bob"));
}

#[test]
fn autotrust_scoped_to_channel_does_not_leak_to_other_channels() {
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::Normal);
    enable_channel(&alice, "#y", ChannelMode::Normal);
    enable_channel(&bob, "#x", ChannelMode::Normal);
    enable_channel(&bob, "#y", ChannelMode::Normal);
    alice.keyring().add_autotrust("#x", "~bob@*", 100).unwrap();

    // #x: autotrust hit → AutoAccept promotion → KEYRSP returned.
    let req_x = bob.build_keyreq("#x").unwrap();
    let rsp_x = alice.handle_keyreq("~bob@b.host", &req_x).unwrap();
    assert!(rsp_x.is_some(), "autotrust should allow #x");

    // #y: no matching rule, normal mode, peer not previously trusted →
    // Normal mode caches the KEYREQ and returns Ok(None).
    let req_y = bob.build_keyreq("#y").unwrap();
    let rsp_y = alice.handle_keyreq("~bob@b.host", &req_y).unwrap();
    assert!(
        rsp_y.is_none(),
        "#y should fall through to Normal-mode pending (not AutoAccept)"
    );
    // A pending-accept prompt should have been queued.
    let accepts = alice.take_pending_accept_requests();
    assert_eq!(accepts.len(), 1);
    assert_eq!(accepts[0].channel, "#y");
    assert_eq!(accepts[0].handle, "~bob@b.host");
}

#[test]
fn autotrust_wildcard_matching_is_case_insensitive() {
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::Normal);
    enable_channel(&bob, "#x", ChannelMode::Normal);
    alice
        .keyring()
        .add_autotrust("global", "*bob*@Example.Org", 100)
        .unwrap();

    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice
        .handle_keyreq("~BoB@example.org", &req)
        .unwrap()
        .expect("case-insensitive glob should match");
    let _ = rsp;
}

// ---------- G10: Normal mode pending prompt + accept completes handshake ----------

#[test]
fn normal_mode_stores_pending_keyreq_and_emits_prompt() {
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::Normal);
    enable_channel(&bob, "#x", ChannelMode::Normal);

    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice
        .handle_keyreq_with_nick("~bob@b.host", Some("bob"), &req)
        .unwrap();
    assert!(
        rsp.is_none(),
        "Normal mode must not auto-respond to unknown peer"
    );
    // Pending prompt was queued.
    let prompts = alice.take_pending_accept_requests();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].nick.as_deref(), Some("bob"));
    assert_eq!(prompts[0].handle, "~bob@b.host");
    assert_eq!(prompts[0].channel, "#x");
    // Incoming session row exists, status = Pending.
    let sess = alice
        .keyring()
        .get_incoming_session("~bob@b.host", "#x")
        .unwrap()
        .expect("pending session row should be installed");
    assert_eq!(sess.status, TrustStatus::Pending);
    let peer = alice
        .keyring()
        .get_peer_by_handle("~bob@b.host")
        .unwrap()
        .expect("peer row should be installed");
    assert_eq!(peer.last_nick.as_deref(), Some("bob"));
}

#[test]
fn accept_completes_pending_keyreq_by_sending_keyrsp() {
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::Normal);
    enable_channel(&bob, "#x", ChannelMode::Normal);

    // Bob sends KEYREQ; alice stashes it.
    let req = bob.build_keyreq("#x").unwrap();
    let none = alice.handle_keyreq("~bob@b.host", &req).unwrap();
    assert!(none.is_none());

    // Alice runs /e2e accept — manager produces the KEYRSP.
    let rsp = alice
        .accept_pending_inbound("~bob@b.host", "#x")
        .unwrap()
        .expect("accept should return Some(KeyRsp)");

    // The cached placeholder must stay Pending until the reciprocal
    // handshake installs a real Bob→Alice session.
    let pending = alice
        .keyring()
        .get_incoming_session("~bob@b.host", "#x")
        .unwrap()
        .expect("placeholder session should still exist");
    assert_eq!(pending.status, TrustStatus::Pending);

    // Bob consumes the KEYRSP and can decrypt Alice's next message.
    bob.handle_keyrsp("~alice@a.host", &rsp).unwrap();
    let w = alice.encrypt_outgoing("#x", "hello after accept").unwrap();
    let out = bob.decrypt_incoming("~alice@a.host", "#x", &w[0]).unwrap();
    assert!(
        matches!(out, DecryptOutcome::Plaintext(ref s) if s == "hello after accept"),
        "expected Plaintext, got {out:?}"
    );

    // Accept must also queue the reciprocal KEYREQ so Bob→Alice
    // converges without a separate manual handshake.
    let recs = alice.take_pending_outbound_keyreqs();
    assert_eq!(recs.len(), 1, "accept should queue one reciprocal KEYREQ");
    assert_eq!(recs[0].peer_handle, "~bob@b.host");
    assert_eq!(recs[0].channel, "#x");

    let rsp2 = bob
        .handle_keyreq("~alice@a.host", &recs[0].req)
        .unwrap()
        .expect("bob should answer the reciprocal KEYREQ");
    alice.handle_keyrsp("~bob@b.host", &rsp2).unwrap();

    let w2 = bob.encrypt_outgoing("#x", "hello back").unwrap();
    let out2 = alice.decrypt_incoming("~bob@b.host", "#x", &w2[0]).unwrap();
    assert!(
        matches!(out2, DecryptOutcome::Plaintext(ref s) if s == "hello back"),
        "expected reciprocal Plaintext, got {out2:?}"
    );

    // A second accept for the same (handle, channel) returns Ok(None)
    // because the pending-inbound cache is drained on the first call.
    let again = alice.accept_pending_inbound("~bob@b.host", "#x").unwrap();
    assert!(
        again.is_none(),
        "pending-inbound cache must be consumed by the first accept"
    );
}

/// Regression test for the echo-message self-decrypt bug: a stale
/// in-flight KEYREQ in `self.pending` (for example, one fired by
/// `try_decrypt_e2e` on an `echo-message` cap echo of our own encrypted
/// PRIVMSG) must NOT block the reciprocal KEYREQ that `/e2e accept`
/// queues for the peer who is waiting on the other side of a
/// Normal-mode handshake.
///
/// Without the fix, `build_keyrsp_for_accepted_request` skips the
/// reciprocal block because `has_pending_keyreq` returns true on the
/// stale entry, and the Bob→Alice direction never gets installed —
/// Alice keeps rejecting Bob's ciphertext with
/// `peer not trusted (status=Pending)` forever.
#[test]
fn stale_self_keyreq_must_not_block_reciprocal_from_accept() {
    // Use AutoAccept on bob so we don't have to chain through a second
    // Normal-mode prompt just to test the alice-side reciprocal path.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::Normal);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    // Alice writes → generates her outgoing key.
    let _w = alice.encrypt_outgoing("#x", "hello").unwrap();

    // Simulate the effect of `try_decrypt_e2e` running on Alice's own
    // echo-message echo: MissingKey → auto-KEYREQ → pending entry in
    // `alice.pending`. The NOTICE itself would have been targeted at
    // Alice's own nick and never produced a real KEYRSP, so this entry
    // is effectively stale forever.
    let _self_req = alice.build_keyreq("#x").unwrap();
    assert!(alice.has_pending_keyreq("#x"));

    // Bob sends a real KEYREQ to Alice; Alice is in Normal mode and
    // caches it as a pending prompt.
    let bob_req = bob.build_keyreq("#x").unwrap();
    let none = alice.handle_keyreq("~bob@b.host", &bob_req).unwrap();
    assert!(none.is_none(), "Normal mode should not auto-respond");

    // Alice runs /e2e accept — the reciprocal KEYREQ MUST be queued so
    // the Bob→Alice direction can complete. Without the fix the
    // `has_pending_keyreq` guard short-circuits and leaves the queue
    // without the bob reciprocal.
    let _rsp = alice
        .accept_pending_inbound("~bob@b.host", "#x")
        .unwrap()
        .expect("accept must produce a KEYRSP");
    let recs = alice.take_pending_outbound_keyreqs();
    let bob_reciprocal = recs
        .iter()
        .find(|r| r.peer_handle == "~bob@b.host" && r.channel == "#x")
        .unwrap_or_else(|| {
            panic!(
                "accept must queue a reciprocal KEYREQ for bob even if a stale \
                 self-targeted pending entry exists; got {} entries",
                recs.len()
            )
        });

    // And the reciprocal must actually complete the round-trip: Bob
    // serves a KEYRSP (AutoAccept), Alice consumes it, and Alice can
    // now decrypt Bob's ciphertext (placeholder row promotes to
    // Trusted).
    let rsp2 = bob
        .handle_keyreq("~alice@a.host", &bob_reciprocal.req)
        .unwrap()
        .expect("AutoAccept bob should always answer a KEYREQ");
    alice.handle_keyrsp("~bob@b.host", &rsp2).unwrap();

    let bob_wire = bob.encrypt_outgoing("#x", "hello back").unwrap();
    let out = alice
        .decrypt_incoming("~bob@b.host", "#x", &bob_wire[0])
        .unwrap();
    assert!(
        matches!(out, DecryptOutcome::Plaintext(ref s) if s == "hello back"),
        "alice must decrypt bob's ciphertext after reciprocal completes, got {out:?}"
    );
}

#[test]
fn quiet_mode_silently_drops_unknown_peer() {
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::Quiet);
    enable_channel(&bob, "#x", ChannelMode::Quiet);

    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq("~bob@b.host", &req).unwrap();
    assert!(rsp.is_none(), "Quiet mode must drop unknown peer silently");
    // No pending prompt, no pending-inbound cache.
    assert!(alice.take_pending_accept_requests().is_empty());
}

#[test]
fn context_key_channels_vs_pms() {
    // Unit-level check that the helper preserves channels and rewrites
    // PMs — intentionally duplicated here as an integration-test-level
    // breadcrumb alongside the keyring-driven tests.
    use crate::e2e::context_key;

    assert_eq!(context_key("#rust", "~bob@b.host"), "#rust");
    assert_eq!(context_key("&local", "~bob@b.host"), "&local");
    assert_eq!(context_key("!ABCDE", "~bob@b.host"), "!ABCDE");
    assert_eq!(context_key("+modeless", "~bob@b.host"), "+modeless");
    assert_eq!(
        context_key("bob", "~bob@b.host"),
        "@~bob@b.host",
        "PM target must be rewritten to @<peer_handle>"
    );
}

// ---------- G11 gap 3: /e2e reverify is a real path (not an alias) ----------

#[test]
fn reverify_applies_pending_fingerprint_change() {
    // Reproduce `handshake_with_changed_fingerprint_is_rejected`:
    //   1. Alice and Bob_orig complete a handshake → alice has a peer row
    //      + trusted incoming session pinned to bob_orig's fingerprint.
    //   2. Bob regenerates his identity (bob_new) and retries a KEYREQ
    //      from the same handle — alice MUST reject with a
    //      FingerprintChanged PendingTrustNotice.
    //   3. User runs `/e2e reverify` — `mgr.reverify_peer("~bob@b.host", None)`
    //      consumes the notice, deletes the old peer + sessions, and
    //      installs bob_new's pubkey.
    //   4. A third handshake attempt from bob_new under the SAME handle
    //      must now succeed (the alice-side TOFU classifier sees the
    //      fingerprint as Known or New, never FingerprintChanged).
    let alice = make_manager();
    let bob_orig = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob_orig, "#x", ChannelMode::AutoAccept);

    let bob_handle = "~bob@b.host";
    let alice_handle = "~alice@a.host";

    // (1) initial handshake pins bob_orig.
    let req1 = bob_orig.build_keyreq("#x").unwrap();
    let rsp1 = alice.handle_keyreq(bob_handle, &req1).unwrap().unwrap();
    bob_orig.handle_keyrsp(alice_handle, &rsp1).unwrap();
    assert!(alice.take_pending_trust_changes().is_empty());

    // (2) bob regenerates identity and retries.
    let bob_new = make_manager();
    enable_channel(&bob_new, "#x", ChannelMode::AutoAccept);
    assert_ne!(bob_orig.fingerprint(), bob_new.fingerprint());

    let req2 = bob_new.build_keyreq("#x").unwrap();
    let err = alice
        .handle_keyreq(bob_handle, &req2)
        .expect_err("must reject changed fingerprint");
    match err {
        E2eError::HandleMismatch { .. } => {}
        other => panic!("expected HandleMismatch, got {other:?}"),
    }
    // One FingerprintChanged notice is queued. The test intentionally
    // does NOT drain it via take_pending_trust_changes — reverify_peer
    // consumes it instead.

    // (3) user runs /e2e reverify — manager consumes the pending notice.
    let outcome = alice.reverify_peer(bob_handle, None).unwrap();
    match outcome {
        ReverifyOutcome::Applied { old_fp, new_fp } => {
            assert_eq!(old_fp, bob_orig.fingerprint());
            assert_eq!(new_fp, bob_new.fingerprint());
        }
        other => panic!("expected Applied, got {other:?}"),
    }
    // The pending queue is now empty (reverify consumed the matching
    // notice and preserved everything else).
    assert!(alice.take_pending_trust_changes().is_empty());

    // Alice's peer table now has bob_new's fingerprint (and NOT
    // bob_orig's) under bob_handle.
    assert!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&bob_orig.fingerprint())
            .unwrap()
            .is_none(),
        "old peer row must be deleted"
    );
    let peer_new = alice
        .keyring()
        .get_peer_by_fingerprint(&bob_new.fingerprint())
        .unwrap()
        .expect("new peer row installed by reverify");
    assert_eq!(peer_new.last_handle.as_deref(), Some(bob_handle));
    // Incoming session for bob_handle on #x was purged; a third
    // handshake is required to re-establish it with bob_new's key.
    assert!(
        alice
            .keyring()
            .get_incoming_session(bob_handle, "#x")
            .unwrap()
            .is_none(),
        "stale incoming session must be purged"
    );

    // (4) Replay the same req2 back to alice. After reverify consumed
    // the FingerprintChanged notice AND upserted bob_new's pubkey as
    // Trusted, alice's TOFU classifier sees (new_fp, bob_handle) as
    // Known rather than FingerprintChanged — so the second attempt
    // succeeds and produces a KEYRSP. We reuse req2 instead of
    // building a fresh req3 because bob_new still has a live pending
    // handshake entry from req2, and the v1 initiator only scans the
    // pending map by channel — a second build_keyreq for the same
    // channel would race the stale entry. In real deployment this is
    // rate-limited away by the 30s outgoing window; in the test we
    // just replay the already-built request.
    let rsp2_accepted = alice
        .handle_keyreq(bob_handle, &req2)
        .unwrap()
        .expect("after reverify the replayed KEYREQ should be accepted");
    bob_new.handle_keyrsp(alice_handle, &rsp2_accepted).unwrap();

    // And the trust change queue stays empty — no more FingerprintChanged
    // warnings fire for this handle.
    assert!(alice.take_pending_trust_changes().is_empty());

    // End-to-end: Alice encrypts for #x and bob_new decrypts using the
    // session his handle_keyrsp just installed.
    let wires = alice
        .encrypt_outgoing("#x", "hello after reverify")
        .unwrap();
    match bob_new
        .decrypt_incoming(alice_handle, "#x", &wires[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "hello after reverify"),
        other => panic!("expected Plaintext, got {other:?}"),
    }
}

#[test]
fn reverify_without_pending_notice_purges_handle() {
    // When `/e2e reverify <nick>` is run without a queued
    // FingerprintChanged notice, the command falls back to a
    // destructive purge of the handle's keyring state. This is the
    // documented recovery path for users who've already compared SAS
    // out-of-band and just need the old identity gone.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let bob_handle = "~bob@b.host";
    let alice_handle = "~alice@a.host";

    // Full handshake populates peer + incoming session + recipient row.
    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq(bob_handle, &req).unwrap().unwrap();
    bob.handle_keyrsp(alice_handle, &rsp).unwrap();
    assert!(alice.take_pending_trust_changes().is_empty());

    // Reverify with no pending notice → Cleared.
    match alice.reverify_peer(bob_handle, None).unwrap() {
        ReverifyOutcome::Cleared { deleted } => {
            assert!(deleted >= 1, "expected at least one row purged");
        }
        other => panic!("expected Cleared, got {other:?}"),
    }

    // Peer row is gone, incoming session is gone.
    assert!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&bob.fingerprint())
            .unwrap()
            .is_none()
    );
    assert!(
        alice
            .keyring()
            .get_incoming_session(bob_handle, "#x")
            .unwrap()
            .is_none()
    );
}

#[test]
fn reverify_unknown_handle_is_not_found() {
    let alice = make_manager();
    match alice.reverify_peer("~ghost@nowhere.test", None).unwrap() {
        ReverifyOutcome::NotFound => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// Drive a HandleChanged into `alice`: bob handshakes from `old_handle`,
/// then re-appears under `new_handle` with the same identity key. Returns
/// the refused KEYREQ so callers can replay it after reverify (bob still
/// has a live pending entry for `#x`, so a second `build_keyreq` would
/// race it — see `reverify_applies_pending_fingerprint_change`).
///
/// The notice is drained exactly as `surface_pending_trust_changes` does
/// in production: the IRC dispatcher takes the queue to render the
/// `[E2E] notice: known key … appeared under new handle` line, long
/// before the user gets a chance to type `/e2e reverify`.
fn stage_handle_change(
    alice: &E2eManager,
    bob: &E2eManager,
    old_handle: &str,
    new_handle: &str,
) -> crate::e2e::handshake::KeyReq {
    let req1 = bob.build_keyreq("#x").unwrap();
    alice.handle_keyreq(old_handle, &req1).unwrap().unwrap();
    assert!(alice.take_pending_trust_changes().is_empty());

    let req2 = bob.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(new_handle, &req2)
        .expect_err("must refuse to auto-rebind the handle");

    let notices = alice.take_pending_trust_changes();
    assert_eq!(notices.len(), 1, "the handle change must be surfaced once");
    assert!(matches!(
        notices[0].change,
        TrustChange::HandleChanged { .. }
    ));
    req2
}

#[test]
fn reverify_accepts_a_known_key_under_its_new_handle() {
    // The reported bug. A peer reconnects with a different ident, so the
    // same already-trusted key arrives under a new handle. The notice
    // tells the user to `/e2e reverify <new handle>` "to accept" — and
    // accepting must re-bind the existing trusted identity to the new
    // handle, not purge it and not report "no keyring state".
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let old_handle = "freakyy85@hosted.by.nextgamers.eu";
    let new_handle = "freaky@hosted.by.nextgamers.eu";
    let req2 = stage_handle_change(&alice, &bob, old_handle, new_handle);

    match alice.reverify_peer(new_handle, None).unwrap() {
        ReverifyOutcome::Rebound {
            fingerprint,
            old_handle: from,
            new_handle: to,
        } => {
            assert_eq!(fingerprint, bob.fingerprint());
            assert_eq!(from, old_handle);
            assert_eq!(to, new_handle);
        }
        other => panic!("expected Rebound, got {other:?}"),
    }

    // The identity survived — same fingerprint, same trust, new handle.
    let peer = alice
        .keyring()
        .get_peer_by_fingerprint(&bob.fingerprint())
        .unwrap()
        .expect("the trusted peer row must survive a handle change");
    assert_eq!(peer.last_handle.as_deref(), Some(new_handle));
    // Carried over, not raised: alice only answered bob's KEYREQ, which
    // leaves the peer row Pending even under AutoAccept. See
    // `a_rebind_carries_the_peers_trust_status_over_unchanged`.
    assert_eq!(peer.global_status, TrustStatus::Pending);

    // The stale per-handle rows are gone so nothing keys off the old ident.
    assert!(
        alice
            .keyring()
            .get_incoming_session(old_handle, "#x")
            .unwrap()
            .is_none(),
        "the old handle's session must be purged"
    );

    // And the handshake that was refused now completes without a warning.
    alice
        .handle_keyreq(new_handle, &req2)
        .unwrap()
        .expect("after reverify the peer handshakes under the new handle");
    assert!(alice.take_pending_trust_changes().is_empty());
}

#[test]
fn reverify_accepts_a_handle_change_named_by_the_old_handle() {
    // The notice prints both handles, so either one is a plausible thing
    // for the user to type. Naming the OLD handle must also re-bind —
    // never fall through to the destructive purge, which would throw away
    // a trusted key whose fingerprint never changed.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let old_handle = "~bob@home.host";
    let new_handle = "~bob@vpn.mullvad.net";
    stage_handle_change(&alice, &bob, old_handle, new_handle);

    match alice.reverify_peer(old_handle, None).unwrap() {
        ReverifyOutcome::Rebound { new_handle: to, .. } => assert_eq!(to, new_handle),
        other => panic!("expected Rebound, got {other:?}"),
    }
    let peer = alice
        .keyring()
        .get_peer_by_fingerprint(&bob.fingerprint())
        .unwrap()
        .expect("the trusted peer row must survive a handle change");
    assert_eq!(peer.last_handle.as_deref(), Some(new_handle));
}

#[test]
fn reverify_picks_the_handle_change_the_user_named() {
    // Bob flips idents twice without a reverify in between, so alice holds
    // two HandleChanged notices that share the same `old_handle`. Naming
    // one destination must re-bind to THAT one, not to whichever arrived
    // first.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let home = "~bob@home.host";
    let vpn = "~bob@vpn.mullvad.net";
    let cafe = "~bob@cafe.wifi";

    stage_handle_change(&alice, &bob, home, vpn);
    let req3 = bob.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(cafe, &req3)
        .expect_err("still pinned to the original handle");
    assert_eq!(alice.take_pending_trust_changes().len(), 1);

    match alice.reverify_peer(cafe, None).unwrap() {
        ReverifyOutcome::Rebound { new_handle, .. } => assert_eq!(new_handle, cafe),
        other => panic!("expected Rebound to {cafe}, got {other:?}"),
    }
    let peer = alice
        .keyring()
        .get_peer_by_fingerprint(&bob.fingerprint())
        .unwrap()
        .unwrap();
    assert_eq!(peer.last_handle.as_deref(), Some(cafe));
}

#[test]
fn reverify_refuses_when_two_keys_are_waiting_at_one_handle() {
    // Bob's key is pinned at H, then two DIFFERENT keys handshake at H
    // before the user gets round to reverifying. The user compares one
    // fingerprint out of band — but `/e2e reverify H` carries no way to
    // say which, so accepting either risks installing the key they did
    // NOT verify. Refuse, and keep both warnings so the choice survives.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    let handle = "~bob@b.host";

    let req = bob.build_keyreq("#x").unwrap();
    alice.handle_keyreq(handle, &req).unwrap().unwrap();

    let claimant_a = make_manager();
    let claimant_b = make_manager();
    for m in [&claimant_a, &claimant_b] {
        enable_channel(m, "#x", ChannelMode::AutoAccept);
        let r = m.build_keyreq("#x").unwrap();
        alice
            .handle_keyreq(handle, &r)
            .expect_err("a changed fingerprint must be refused");
    }
    assert_eq!(
        alice.take_pending_trust_changes().len(),
        2,
        "both key changes must be surfaced — neither may replace the other"
    );

    match alice.reverify_peer(handle, None).unwrap() {
        ReverifyOutcome::Ambiguous { candidates } => assert_eq!(candidates.len(), 2),
        other => panic!("expected Ambiguous, got {other:?}"),
    }

    // Nothing was installed: the originally pinned key is untouched.
    let peer = alice
        .keyring()
        .get_peer_by_fingerprint(&bob.fingerprint())
        .unwrap()
        .expect("the pinned key must survive a refused reverify");
    assert_eq!(peer.last_handle.as_deref(), Some(handle));
    for m in [&claimant_a, &claimant_b] {
        assert!(
            alice
                .keyring()
                .get_peer_by_fingerprint(&m.fingerprint())
                .unwrap()
                .is_none(),
            "no unverified key may be installed by an ambiguous reverify"
        );
    }
    // And nothing was consumed, so the user can still resolve it —
    // which naming the handle again cannot do, since both candidates
    // carry that same handle. The fingerprint they compared out of band
    // is the discriminator. A short, upper-case prefix must work: it is
    // pasted back off the candidate list by hand.
    assert!(matches!(
        alice.reverify_peer(handle, None).unwrap(),
        ReverifyOutcome::Ambiguous { .. }
    ));
    let chosen = fingerprint_hex(&claimant_a.fingerprint());
    match alice
        .reverify_peer(handle, Some(&chosen[..8].to_uppercase()))
        .unwrap()
    {
        ReverifyOutcome::Applied { old_fp, new_fp } => {
            assert_eq!(old_fp, bob.fingerprint());
            assert_eq!(new_fp, claimant_a.fingerprint());
        }
        other => panic!("expected Applied, got {other:?}"),
    }
    assert!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&claimant_a.fingerprint())
            .unwrap()
            .is_some(),
        "the named key must be installed"
    );
    assert!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&claimant_b.fingerprint())
            .unwrap()
            .is_none(),
        "the key the user did not name must stay out"
    );
    // Choosing one settles the contest at that handle: the loser's claim
    // is not left lying around to be applied by a later reverify.
    assert!(matches!(
        alice
            .reverify_peer(handle, Some(&fingerprint_hex(&claimant_b.fingerprint())))
            .unwrap(),
        ReverifyOutcome::NoSuchCandidate { .. }
    ));
}

/// Stage the crossing case both orderings share: bob's trusted key moves
/// `h` → `j`, and a *different* key claims `h`. Two independent pending
/// decisions. Returns the rival manager.
fn stage_crossing_change(alice: &E2eManager, bob: &E2eManager, h: &str, j: &str) -> E2eManager {
    stage_handle_change(alice, bob, h, j);
    let claimant = make_manager();
    enable_channel(&claimant, "#x", ChannelMode::AutoAccept);
    let req = claimant.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(h, &req)
        .expect_err("a changed fingerprint must be refused");
    assert_eq!(alice.take_pending_trust_changes().len(), 1);
    claimant
}

#[test]
fn rebinding_a_moved_key_leaves_a_rival_claim_on_the_old_handle_unresolved() {
    // Accepting the move to J says nothing about who may use H. Retiring
    // the rival's warning here would drop a decision the user never made
    // — and leave the handle free for that same key to be TOFU-pinned
    // without any warning at all.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    let h = "~bob@b.host";
    let j = "~bob@vpn.host";
    let claimant = stage_crossing_change(&alice, &bob, h, j);

    assert!(matches!(
        alice.reverify_peer(j, None).unwrap(),
        ReverifyOutcome::Rebound { .. }
    ));

    // The claim on H is still the user's to make.
    match alice.reverify_peer(h, None).unwrap() {
        ReverifyOutcome::Applied { new_fp, .. } => assert_eq!(new_fp, claimant.fingerprint()),
        other => panic!("the rival claim on {h} must still be applicable, got {other:?}"),
    }
    // And the moved key, now at J, was not disturbed by that decision.
    let peer = alice
        .keyring()
        .get_peer_by_fingerprint(&bob.fingerprint())
        .unwrap()
        .expect("the moved key must survive a decision about the handle it left");
    assert_eq!(peer.last_handle.as_deref(), Some(j));
}

#[test]
fn accepting_a_rival_key_keeps_the_moved_identity_rebindable() {
    // The inverse ordering. Accepting the rival at H must not delete the
    // moved key's row just because that row still names H: its H → J
    // warning is unresolved, so the identity is not finished. Accepting
    // the move afterwards has to RE-BIND it, per the documented promise
    // that a handle change preserves the fingerprint you verified — not
    // discover the row gone and degrade to a purge.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    let h = "~bob@b.host";
    let j = "~bob@vpn.host";
    let claimant = stage_crossing_change(&alice, &bob, h, j);

    match alice.reverify_peer(h, None).unwrap() {
        ReverifyOutcome::Applied { new_fp, .. } => assert_eq!(new_fp, claimant.fingerprint()),
        other => panic!("expected Applied at {h}, got {other:?}"),
    }

    match alice.reverify_peer(j, None).unwrap() {
        ReverifyOutcome::Rebound {
            fingerprint,
            new_handle,
            ..
        } => {
            assert_eq!(fingerprint, bob.fingerprint());
            assert_eq!(new_handle, j);
        }
        other => panic!("expected Rebound at {j}, got {other:?}"),
    }
    let peer = alice
        .keyring()
        .get_peer_by_fingerprint(&bob.fingerprint())
        .unwrap()
        .expect("the moved identity must survive");
    assert_eq!(peer.last_handle.as_deref(), Some(j));
    assert_eq!(peer.global_status, TrustStatus::Pending);
    // ...and the rival still holds H.
    assert_eq!(
        alice
            .keyring()
            .get_peer_by_handle(h)
            .unwrap()
            .expect("the accepted rival must hold H")
            .fingerprint,
        claimant.fingerprint()
    );
}

/// Give `alice` a trusted incoming session for `peer` under `handle`.
///
/// A responder learns nothing from answering a KEYREQ — the session key
/// travels in the KEYRSP — so this drives the direction that installs
/// alice's own side of the session.
fn open_session(alice: &E2eManager, peer: &E2eManager, handle: &str) {
    let req = alice.build_keyreq("#x").unwrap();
    let rsp = peer
        .handle_keyreq("~alice@a.host", &req)
        .unwrap()
        .expect("the peer answers the KEYREQ");
    alice.handle_keyrsp(handle, &rsp).unwrap();
}

#[test]
fn a_stale_handle_change_does_not_purge_the_handles_new_owner() {
    // Forgetting a peer by the handle it is moving *away from* deletes its
    // row but leaves the warning, which is filed under the destination. If
    // somebody else then claims the vacated handle, reverifying that handle
    // selects the stale warning through its old-handle alias — and must not
    // take the new owner's state down with it. The key the warning is about
    // no longer exists, so there is nothing to re-bind and nothing of its
    // to clean up.
    let alice = make_manager();
    let bob = make_manager();
    let newcomer = make_manager();
    for m in [&alice, &bob, &newcomer] {
        enable_channel(m, "#x", ChannelMode::AutoAccept);
    }
    let h = "~bob@b.host";
    let j = "~bob@vpn.host";

    stage_handle_change(&alice, &bob, h, j);
    alice.forget_peer_everywhere(h).unwrap();
    assert!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&bob.fingerprint())
            .unwrap()
            .is_none(),
        "forget removed bob's row but left the warning filed under J"
    );

    // Somebody else takes the vacated handle the ordinary way.
    open_session(&alice, &newcomer, h);

    match alice.reverify_peer(h, None).unwrap() {
        ReverifyOutcome::Stale { fingerprint } => assert_eq!(fingerprint, bob.fingerprint()),
        other => panic!("expected Stale, got {other:?}"),
    }
    assert_eq!(
        alice
            .keyring()
            .get_peer_by_handle(h)
            .unwrap()
            .expect("the new owner's row must survive")
            .fingerprint,
        newcomer.fingerprint()
    );
    assert!(
        alice
            .keyring()
            .get_incoming_session(h, "#x")
            .unwrap()
            .is_some(),
        "the new owner's session must survive"
    );
    // The stale warning is retired — it could never be applied.
    assert!(matches!(
        alice.reverify_peer(j, None).unwrap(),
        ReverifyOutcome::NotFound
    ));
}

#[test]
fn a_rebind_carries_the_peers_trust_status_over_unchanged() {
    // Accepting a handle change is consent about the binding, not the key.
    // A peer whose first exchange is still awaiting `/e2e accept` holds a
    // Pending row, and a handle change is classified without regard to
    // that — so the rebind must not promote it. An already-trusted key
    // must likewise not be demoted.
    let h = "~bob@b.host";
    let j = "~bob@vpn.host";

    // Pending: alice only answered bob's KEYREQ. That leaves the peer row
    // Pending even under AutoAccept — only consuming a KEYRSP, which is
    // our own explicit outbound consent, promotes it.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    stage_handle_change(&alice, &bob, h, j);
    assert_eq!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&bob.fingerprint())
            .unwrap()
            .unwrap()
            .global_status,
        TrustStatus::Pending,
        "answering a KEYREQ must not trust the identity globally"
    );
    assert!(matches!(
        alice.reverify_peer(j, None).unwrap(),
        ReverifyOutcome::Rebound { .. }
    ));
    assert_eq!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&bob.fingerprint())
            .unwrap()
            .unwrap()
            .global_status,
        TrustStatus::Pending,
        "accepting a handle change must not promote an unaccepted peer"
    );

    // Trusted: alice consumed a KEYRSP, so the identity really is trusted.
    let carol = make_manager();
    let dave = make_manager();
    enable_channel(&carol, "#x", ChannelMode::AutoAccept);
    enable_channel(&dave, "#x", ChannelMode::AutoAccept);
    open_session(&carol, &dave, h);
    assert_eq!(
        carol
            .keyring()
            .get_peer_by_fingerprint(&dave.fingerprint())
            .unwrap()
            .unwrap()
            .global_status,
        TrustStatus::Trusted
    );
    let req = dave.build_keyreq("#x").unwrap();
    carol
        .handle_keyreq(j, &req)
        .expect_err("must refuse to auto-rebind");
    carol.take_pending_trust_changes();
    assert!(matches!(
        carol.reverify_peer(j, None).unwrap(),
        ReverifyOutcome::Rebound { .. }
    ));
    assert_eq!(
        carol
            .keyring()
            .get_peer_by_fingerprint(&dave.fingerprint())
            .unwrap()
            .unwrap()
            .global_status,
        TrustStatus::Trusted,
        "a trusted key must not be demoted by a handle change"
    );
}

#[test]
fn accepting_a_detached_peers_move_leaves_its_old_handle_alone() {
    // A rival accepted at H detaches the original key (`last_handle =
    // None`) because its H→J move is still pending. If the rival then
    // handshakes at H, accepting that move must not resurrect H as the
    // original's "current" binding: a detached peer has nothing to clean,
    // and H belongs to the rival now.
    let alice = make_manager();
    let bob = make_manager();
    let rival = make_manager();
    for m in [&alice, &bob, &rival] {
        enable_channel(m, "#x", ChannelMode::AutoAccept);
    }
    let h = "~bob@b.host";
    let j = "~bob@vpn.host";

    stage_handle_change(&alice, &bob, h, j);

    // The rival claims H and is accepted there, detaching bob.
    let rival_req = rival.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(h, &rival_req)
        .expect_err("a changed fingerprint must be refused");
    alice.take_pending_trust_changes();
    assert!(matches!(
        alice.reverify_peer(h, None).unwrap(),
        ReverifyOutcome::Applied { .. }
    ));
    assert_eq!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&bob.fingerprint())
            .unwrap()
            .expect("bob's key is kept, only unbound")
            .last_handle,
        None
    );

    // The rival settles in at H.
    open_session(&alice, &rival, h);
    assert!(
        alice
            .keyring()
            .get_incoming_session(h, "#x")
            .unwrap()
            .is_some()
    );

    // Accepting bob's still-pending move must not reach back into H.
    assert!(matches!(
        alice.reverify_peer(j, None).unwrap(),
        ReverifyOutcome::Rebound { .. }
    ));
    assert!(
        alice
            .keyring()
            .get_incoming_session(h, "#x")
            .unwrap()
            .is_some(),
        "a detached peer has no binding at H to clean — the rival's session must survive"
    );
}

#[test]
fn installing_a_key_cleans_the_binding_it_already_held() {
    // A key change at H is recorded, and before it is accepted the new key
    // is TOFU-pinned at a free handle J. Accepting at H reassigns the key
    // to H, so its session at J is stale — and decryption is keyed by
    // `(handle, channel)` without re-checking the peer row, so leaving it
    // would keep trusting traffic from J.
    let alice = make_manager();
    let bob = make_manager();
    let newkey = make_manager();
    for m in [&alice, &bob, &newkey] {
        enable_channel(m, "#x", ChannelMode::AutoAccept);
    }
    let h = "~bob@b.host";
    let j = "~other@elsewhere.host";

    let bob_req = bob.build_keyreq("#x").unwrap();
    alice.handle_keyreq(h, &bob_req).unwrap().unwrap();

    let nk_req = newkey.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(h, &nk_req)
        .expect_err("a changed fingerprint must be refused");
    assert_eq!(alice.take_pending_trust_changes().len(), 1);

    // Meanwhile it takes the free handle J the ordinary way.
    open_session(&alice, &newkey, j);
    assert!(
        alice
            .keyring()
            .get_incoming_session(j, "#x")
            .unwrap()
            .is_some()
    );

    match alice.reverify_peer(h, None).unwrap() {
        ReverifyOutcome::Applied { new_fp, .. } => assert_eq!(new_fp, newkey.fingerprint()),
        other => panic!("expected Applied at {h}, got {other:?}"),
    }
    assert_eq!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&newkey.fingerprint())
            .unwrap()
            .unwrap()
            .last_handle
            .as_deref(),
        Some(h)
    );
    assert!(
        alice
            .keyring()
            .get_incoming_session(j, "#x")
            .unwrap()
            .is_none(),
        "the session under the handle it left must not stay trusted"
    );
}

#[test]
fn equivalent_rebinds_to_one_destination_are_a_single_decision() {
    // H→J is pending; the user accepts a separate H→K move, so the peer
    // sits at K and its next attempt at J classifies as K→J. Two warnings
    // now describe the same key arriving at the same handle. That is one
    // decision — treating it as two returns Ambiguous listing the same
    // fingerprint twice with identical accept commands, which nothing can
    // resolve.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    let h = "~bob@home.host";
    let j = "~bob@vpn.host";
    let k = "~bob@cafe.wifi";

    let req1 = bob.build_keyreq("#x").unwrap();
    alice.handle_keyreq(h, &req1).unwrap().unwrap();
    for dest in [j, k] {
        let req = bob.build_keyreq("#x").unwrap();
        alice
            .handle_keyreq(dest, &req)
            .expect_err("must refuse to auto-rebind");
    }
    alice.take_pending_trust_changes();

    assert!(matches!(
        alice.reverify_peer(k, None).unwrap(),
        ReverifyOutcome::Rebound { .. }
    ));

    // bob tries J again — classified K→J now, beside the stale H→J.
    let req_j = bob.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(j, &req_j)
        .expect_err("must refuse to auto-rebind");
    assert_eq!(alice.take_pending_trust_changes().len(), 1);

    match alice.reverify_peer(j, None).unwrap() {
        ReverifyOutcome::Rebound {
            fingerprint,
            new_handle,
            ..
        } => {
            assert_eq!(fingerprint, bob.fingerprint());
            assert_eq!(new_handle, j);
        }
        other => panic!("expected Rebound to {j}, got {other:?}"),
    }
}

#[test]
fn a_second_rebind_cleans_up_the_binding_the_peer_actually_holds() {
    // One key, two unresolved moves off H: H→K and H→J. Accepting K binds
    // the peer there and it handshakes, so a trusted session exists at K.
    // Accepting J afterwards must clean up K — the binding the peer really
    // holds — not the H the J warning snapshotted. `decrypt_incoming` keys
    // on `(handle, channel)` and never re-checks the peer row, so a session
    // left at K would go on decrypting traffic from a handle this peer no
    // longer owns.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    let h = "~bob@home.host";
    let k = "~bob@cafe.wifi";
    let j = "~bob@vpn.host";

    // Pin bob at H, then raise both moves before resolving either.
    let req1 = bob.build_keyreq("#x").unwrap();
    alice.handle_keyreq(h, &req1).unwrap().unwrap();
    for dest in [k, j] {
        let req = bob.build_keyreq("#x").unwrap();
        alice
            .handle_keyreq(dest, &req)
            .expect_err("must refuse to auto-rebind");
    }
    assert_eq!(alice.take_pending_trust_changes().len(), 2);

    // Accept K, then let bob handshake there so a trusted session exists.
    match alice.reverify_peer(k, None).unwrap() {
        ReverifyOutcome::Rebound { new_handle, .. } => assert_eq!(new_handle, k),
        other => panic!("expected Rebound to {k}, got {other:?}"),
    }
    // Alice learns bob's key only by consuming a KEYRSP, so drive that
    // direction: her session for bob lands under the handle she names.
    let a_req = alice.build_keyreq("#x").unwrap();
    let b_rsp = bob
        .handle_keyreq("~alice@a.host", &a_req)
        .unwrap()
        .expect("bob answers the KEYREQ");
    alice.handle_keyrsp(k, &b_rsp).unwrap();
    assert!(
        alice
            .keyring()
            .get_incoming_session(k, "#x")
            .unwrap()
            .is_some(),
        "the handshake must leave a session at K"
    );

    // Now accept the still-pending move to J.
    match alice.reverify_peer(j, None).unwrap() {
        ReverifyOutcome::Rebound {
            old_handle,
            new_handle,
            ..
        } => {
            assert_eq!(new_handle, j);
            assert_eq!(
                old_handle, k,
                "it moved from the binding it held, not the warning's snapshot"
            );
        }
        other => panic!("expected Rebound to {j}, got {other:?}"),
    }
    assert_eq!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&bob.fingerprint())
            .unwrap()
            .unwrap()
            .last_handle
            .as_deref(),
        Some(j)
    );
    assert!(
        alice
            .keyring()
            .get_incoming_session(k, "#x")
            .unwrap()
            .is_none(),
        "the session under the handle the peer left must not survive"
    );
}

#[test]
fn a_rebind_does_not_disturb_whoever_took_over_the_old_handle() {
    // The other half: once a key has moved off H, H can be reassigned.
    // A later rebind of that key must not reach back and delete the new
    // occupant's session just because its warning still names H.
    let alice = make_manager();
    let bob = make_manager();
    let newcomer = make_manager();
    for m in [&alice, &bob, &newcomer] {
        enable_channel(m, "#x", ChannelMode::AutoAccept);
    }
    let h = "~bob@home.host";
    let k = "~bob@cafe.wifi";
    let j = "~bob@vpn.host";

    let req1 = bob.build_keyreq("#x").unwrap();
    alice.handle_keyreq(h, &req1).unwrap().unwrap();
    for dest in [k, j] {
        let req = bob.build_keyreq("#x").unwrap();
        alice
            .handle_keyreq(dest, &req)
            .expect_err("must refuse to auto-rebind");
    }
    alice.take_pending_trust_changes();
    assert!(matches!(
        alice.reverify_peer(k, None).unwrap(),
        ReverifyOutcome::Rebound { .. }
    ));

    // H is free now — somebody else takes it and handshakes.
    let a_req = alice.build_keyreq("#x").unwrap();
    let new_rsp = newcomer
        .handle_keyreq("~alice@a.host", &a_req)
        .unwrap()
        .expect("the newcomer answers the KEYREQ");
    alice.handle_keyrsp(h, &new_rsp).unwrap();
    assert!(
        alice
            .keyring()
            .get_incoming_session(h, "#x")
            .unwrap()
            .is_some(),
        "a vacated handle accepts a new peer"
    );

    // Accepting bob's stale H→J warning must leave the newcomer alone.
    assert!(matches!(
        alice.reverify_peer(j, None).unwrap(),
        ReverifyOutcome::Rebound { .. }
    ));
    assert!(
        alice
            .keyring()
            .get_incoming_session(h, "#x")
            .unwrap()
            .is_some(),
        "the new occupant's session must survive an unrelated peer's rebind"
    );
    assert_eq!(
        alice
            .keyring()
            .get_peer_by_handle(h)
            .unwrap()
            .expect("the newcomer still owns H")
            .fingerprint,
        newcomer.fingerprint()
    );
}

#[test]
fn rebinding_onto_an_occupied_handle_evicts_the_incumbent() {
    // A handle change is classified by fingerprint alone, so nothing
    // upstream notices that the destination already belongs to someone.
    // Accepting the move must still leave exactly one trusted key on that
    // `ident@host` — otherwise the displaced key keeps classifying as
    // Known there and can re-handshake as if it were still the owner.
    let alice = make_manager();
    let mover = make_manager();
    let sitting = make_manager();
    for m in [&alice, &mover, &sitting] {
        enable_channel(m, "#x", ChannelMode::AutoAccept);
    }
    let h = "~mover@one.host";
    let j = "~shared@two.host";

    // `sitting` is trusted at J; `mover` is trusted at H.
    let sit_req = sitting.build_keyreq("#x").unwrap();
    alice.handle_keyreq(j, &sit_req).unwrap().unwrap();
    let mov_req = mover.build_keyreq("#x").unwrap();
    alice.handle_keyreq(h, &mov_req).unwrap().unwrap();
    assert!(alice.take_pending_trust_changes().is_empty());

    // `mover` turns up at J.
    let mov_req2 = mover.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(j, &mov_req2)
        .expect_err("must refuse to auto-rebind onto an occupied handle");
    assert_eq!(alice.take_pending_trust_changes().len(), 1);

    match alice.reverify_peer(j, None).unwrap() {
        ReverifyOutcome::Rebound { fingerprint, .. } => {
            assert_eq!(fingerprint, mover.fingerprint());
        }
        other => panic!("expected Rebound, got {other:?}"),
    }

    // Exactly one identity owns J, and it is the one just accepted.
    assert_eq!(
        alice
            .keyring()
            .get_peer_by_handle(j)
            .unwrap()
            .expect("J must have an owner")
            .fingerprint,
        mover.fingerprint()
    );
    let displaced = alice
        .keyring()
        .get_peer_by_fingerprint(&sitting.fingerprint())
        .unwrap()
        .expect("the displaced key is kept, only unbound");
    assert_eq!(
        displaced.last_handle, None,
        "the displaced key must not still claim J"
    );

    // And it can no longer pass as the owner of J.
    let sit_req2 = sitting.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(j, &sit_req2)
        .expect_err("the displaced key must not classify as Known at J");
}

#[test]
fn forgetting_a_peer_counts_the_warning_it_retires() {
    // The dispatcher drains the render queue the moment it prints a
    // warning, so by the time the user forgets the peer the pending
    // consent entry is the only state left. Reporting "0 row(s)" then
    // reads as "nothing happened" when a real decision was removed.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    let h = "~bob@b.host";
    let j = "~bob@vpn.host";
    stage_handle_change(&alice, &bob, h, j);

    let removed = alice.forget_peer_everywhere(j).unwrap();
    assert!(
        removed >= 1,
        "forget retired the pending warning but reported {removed} row(s)"
    );
    // ...and it really is gone.
    assert!(matches!(
        alice.reverify_peer(j, None).unwrap(),
        ReverifyOutcome::NotFound
    ));
}

#[test]
fn reverify_with_an_unknown_fingerprint_changes_nothing() {
    // A mistyped selector must not fall through to the destructive
    // purge — the pinned key has to survive a wrong answer, and the
    // warning has to survive it too.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    let handle = "~bob@b.host";

    let req = bob.build_keyreq("#x").unwrap();
    alice.handle_keyreq(handle, &req).unwrap().unwrap();

    let claimant = make_manager();
    enable_channel(&claimant, "#x", ChannelMode::AutoAccept);
    let req2 = claimant.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(handle, &req2)
        .expect_err("a changed fingerprint must be refused");
    assert_eq!(alice.take_pending_trust_changes().len(), 1);

    match alice.reverify_peer(handle, Some("deadbeef")).unwrap() {
        ReverifyOutcome::NoSuchCandidate { candidates } => assert_eq!(candidates.len(), 1),
        other => panic!("expected NoSuchCandidate, got {other:?}"),
    }
    assert!(
        alice
            .keyring()
            .get_peer_by_fingerprint(&bob.fingerprint())
            .unwrap()
            .is_some(),
        "a wrong selector must not purge the pinned key"
    );
    // The warning survived, so the right answer still works.
    assert!(matches!(
        alice
            .reverify_peer(handle, Some(&fingerprint_hex(&claimant.fingerprint())))
            .unwrap(),
        ReverifyOutcome::Applied { .. }
    ));
}

#[test]
fn reverify_answers_the_warning_filed_under_the_handle_named() {
    // Bob's trusted key moves from H to J — a HandleChanged that names H
    // only as the handle being moved *away from*. A different key then
    // appears at H, raising a FingerprintChanged filed under H.
    //
    // `/e2e reverify H` is the command that second warning tells the user
    // to run, so it must install that key: not re-bind bob to J on the
    // strength of an alias match, and not eat the handle-change warning,
    // which is a separate decision.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let h = "~bob@b.host";
    let j = "~bob@vpn.host";
    stage_handle_change(&alice, &bob, h, j);

    let other = make_manager();
    enable_channel(&other, "#x", ChannelMode::AutoAccept);
    let req = other.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(h, &req)
        .expect_err("a changed fingerprint at H must be refused");
    assert_eq!(alice.take_pending_trust_changes().len(), 1);

    match alice.reverify_peer(h, None).unwrap() {
        ReverifyOutcome::Applied { old_fp, new_fp } => {
            assert_eq!(old_fp, bob.fingerprint());
            assert_eq!(new_fp, other.fingerprint());
        }
        outcome => panic!("expected Applied at {h}, got {outcome:?}"),
    }
    assert_eq!(
        alice
            .keyring()
            .get_peer_by_handle(h)
            .unwrap()
            .expect("the accepted key must be installed at H")
            .fingerprint,
        other.fingerprint()
    );
    assert!(
        !matches!(
            alice.reverify_peer(j, None).unwrap(),
            ReverifyOutcome::NotFound
        ),
        "the handle-change warning must survive a decision about a different handle"
    );
}

#[test]
fn accepting_one_warning_leaves_an_unrelated_one_on_screen() {
    // Same shape as the test above, but nothing is drained: after the key
    // change at H is accepted, the handle-change warning about J must
    // still be queued for rendering. Clearing per-handle state for H must
    // not suppress a warning that merely names H as the handle a
    // different decision is moving a key away from.
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    let h = "~bob@b.host";
    let j = "~bob@vpn.host";

    let req1 = bob.build_keyreq("#x").unwrap();
    alice.handle_keyreq(h, &req1).unwrap().unwrap();
    let req2 = bob.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(j, &req2)
        .expect_err("the handle change must be refused");

    let other = make_manager();
    enable_channel(&other, "#x", ChannelMode::AutoAccept);
    let req3 = other.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(h, &req3)
        .expect_err("the key change must be refused");

    assert!(matches!(
        alice.reverify_peer(h, None).unwrap(),
        ReverifyOutcome::Applied { .. }
    ));

    let still_queued = alice.take_pending_trust_changes();
    assert_eq!(
        still_queued.len(),
        1,
        "only the warning the user answered may be cleared"
    );
    assert!(matches!(
        still_queued[0].change,
        TrustChange::HandleChanged { .. }
    ));
}

#[test]
fn reverify_still_applies_a_fingerprint_change_after_the_ui_rendered_it() {
    // `reverify_applies_pending_fingerprint_change` never drains the
    // notice queue, but production always does — the IRC dispatcher takes
    // it to render the warning the moment the handshake is refused. The
    // consent state must outlive that render, or the documented one-step
    // "install the new key" path is unreachable and every reverify
    // silently degrades to the destructive purge.
    let alice = make_manager();
    let bob_orig = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob_orig, "#x", ChannelMode::AutoAccept);

    let bob_handle = "~bob@b.host";
    let alice_handle = "~alice@a.host";

    let req1 = bob_orig.build_keyreq("#x").unwrap();
    let rsp1 = alice.handle_keyreq(bob_handle, &req1).unwrap().unwrap();
    bob_orig.handle_keyrsp(alice_handle, &rsp1).unwrap();

    let bob_new = make_manager();
    enable_channel(&bob_new, "#x", ChannelMode::AutoAccept);
    let req2 = bob_new.build_keyreq("#x").unwrap();
    alice
        .handle_keyreq(bob_handle, &req2)
        .expect_err("must reject changed fingerprint");

    // The UI renders the warning — and drains the queue doing it.
    assert_eq!(alice.take_pending_trust_changes().len(), 1);

    match alice.reverify_peer(bob_handle, None).unwrap() {
        ReverifyOutcome::Applied { old_fp, new_fp } => {
            assert_eq!(old_fp, bob_orig.fingerprint());
            assert_eq!(new_fp, bob_new.fingerprint());
        }
        other => panic!("expected Applied, got {other:?}"),
    }
}

// ---------- G11 gap 4: config.e2e.ts_tolerance_secs is honoured ----------

#[test]
fn decrypt_respects_configured_ts_tolerance() {
    // Spec §5.4 ts-replay window. Construct a manager with a 10-second
    // tolerance, run a full handshake, and then hand-craft wire chunks
    // under alice's outgoing session key (which bob, as the initiator,
    // installed as his incoming session via the KEYRSP unwrap). We
    // feed the chunks to `bob.decrypt_incoming` with synthesised
    // timestamps: 15s stale must be rejected, 5s stale must succeed.
    use crate::e2e::crypto::aead;
    use crate::e2e::wire::{WireChunk, build_aad, fresh_msgid};
    use std::time::{SystemTime, UNIX_EPOCH};

    let alice = make_manager_with_tolerance(10);
    let bob = make_manager_with_tolerance(10);

    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let alice_handle = "~alice@a.host";
    let bob_handle = "~bob@b.host";

    // Bob → Alice handshake: bob initiates, alice responds. Alice
    // creates (or already had) an outgoing session for #x, wraps it in
    // the KEYRSP, and bob installs it as his incoming session for
    // alice on #x.
    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq(bob_handle, &req).unwrap().unwrap();
    bob.handle_keyrsp(alice_handle, &rsp).unwrap();

    // Alice's outgoing session is what we'll encrypt with. Bob's
    // incoming session for (alice_handle, #x) must share the same key.
    let alice_outgoing_sk = alice
        .keyring()
        .get_outgoing_session("#x")
        .unwrap()
        .expect("handshake installs alice's outgoing session")
        .sk;
    let bob_incoming = bob
        .keyring()
        .get_incoming_session(alice_handle, "#x")
        .unwrap()
        .expect("bob has an incoming session for alice");
    assert_eq!(bob_incoming.sk, alice_outgoing_sk);

    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();

    // Helper: build a wire chunk with a given ts under alice's outgoing sk.
    let craft = |ts: i64, plaintext: &str| -> String {
        let msgid = fresh_msgid();
        let aad = build_aad("#x", msgid, ts, 1, 1);
        let (nonce, ct) = aead::encrypt(&alice_outgoing_sk, &aad, plaintext.as_bytes()).unwrap();
        WireChunk {
            msgid,
            ts,
            part: 1,
            total: 1,
            nonce,
            ciphertext: ct,
        }
        .encode()
        .unwrap()
    };

    // 15 seconds in the past → outside the 10s tolerance → Rejected.
    let stale = craft(now - 15, "too-old");
    let out_stale = bob.decrypt_incoming(alice_handle, "#x", &stale).unwrap();
    match out_stale {
        DecryptOutcome::Rejected(reason) => {
            assert!(
                reason.contains("ts outside tolerance window"),
                "expected replay-window rejection, got: {reason}"
            );
        }
        other => panic!("expected Rejected for 15s skew, got {other:?}"),
    }

    // 5 seconds in the past → inside the 10s tolerance → Plaintext.
    let fresh = craft(now - 5, "within window");
    let out_fresh = bob.decrypt_incoming(alice_handle, "#x", &fresh).unwrap();
    match out_fresh {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "within window"),
        other => panic!("expected Plaintext for 5s skew, got {other:?}"),
    }
}

// ─── G13: symmetric handshake + identity self-check ─────────────────────────

/// After Alice serves a KEYRSP in AutoAccept mode she must queue a
/// reciprocal KEYREQ so the us→peer direction gets established in the
/// same round-trip. The reciprocal must also close the loop — once Bob
/// holds a trusted incoming session from Alice his own handle_keyreq
/// must NOT queue a second reciprocal (otherwise the two sides
/// perpetually ping-pong KEYREQs).
#[test]
fn autoaccept_keyreq_queues_reciprocal_and_converges() {
    let alice = make_manager();
    let bob = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);

    let alice_handle = "~alice@a.host";
    let bob_handle = "~bob@b.host";

    // Bob initiates.
    let req = bob.build_keyreq("#x").unwrap();
    let rsp1 = alice
        .handle_keyreq(bob_handle, &req)
        .unwrap()
        .expect("auto-accept should produce a KEYRSP");
    bob.handle_keyrsp(alice_handle, &rsp1).unwrap();

    // Alice's symmetric-handshake path must have queued exactly one
    // reciprocal KEYREQ back to Bob on the same channel.
    let recs = alice.take_pending_outbound_keyreqs();
    assert_eq!(
        recs.len(),
        1,
        "expected one reciprocal from AutoAccept path"
    );
    assert_eq!(recs[0].peer_handle, bob_handle);
    assert_eq!(recs[0].channel, "#x");
    let reciprocal_req = recs.into_iter().next().unwrap().req;

    // Bob handles the reciprocal. Because Bob already holds a trusted
    // incoming session from Alice (installed by `handle_keyrsp` above),
    // Bob must skip its OWN reciprocal — otherwise the two clients would
    // loop KEYREQs forever.
    let rsp2 = bob
        .handle_keyreq(alice_handle, &reciprocal_req)
        .unwrap()
        .expect("bob should still serve the reciprocal KEYRSP");
    assert!(
        bob.take_pending_outbound_keyreqs().is_empty(),
        "bob must skip its reciprocal when it already holds a trusted incoming session"
    );
    alice.handle_keyrsp(bob_handle, &rsp2).unwrap();

    // One KEYREQ in, three messages out — full bidirectional traffic now works.
    let wire_a = alice.encrypt_outgoing("#x", "hello bob").unwrap();
    match bob
        .decrypt_incoming(alice_handle, "#x", &wire_a[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "hello bob"),
        other => panic!("alice→bob decrypt failed: {other:?}"),
    }
    let wire_b = bob.encrypt_outgoing("#x", "hi alice").unwrap();
    match alice
        .decrypt_incoming(bob_handle, "#x", &wire_b[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "hi alice"),
        other => panic!("bob→alice decrypt failed: {other:?}"),
    }
}

/// G13 item 7: `load_or_init` must verify the stored pubkey matches the
/// recomputed one. Corrupt the pubkey column in place and the next
/// load must return an error instead of silently running on a key
/// triple where the public column lies about what the secret encodes.
#[test]
fn load_identity_detects_corrupted_pubkey() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    let conn_arc = Arc::new(Mutex::new(conn));

    // Save a fresh valid identity first.
    let kr = Keyring::new(conn_arc.clone());
    let _ = E2eManager::load_or_init(kr).unwrap();

    // Corrupt the pubkey column in place.
    conn_arc
        .lock()
        .unwrap()
        .execute(
            "UPDATE e2e_identity SET pubkey = ?1 WHERE id = 1",
            rusqlite::params![vec![0xffu8; 32]],
        )
        .unwrap();

    // Reloading must now fail with a clear diagnostic.
    let kr2 = Keyring::new(conn_arc);
    match E2eManager::load_or_init(kr2) {
        Err(E2eError::Crypto(msg)) => assert!(
            msg.contains("public key does not match"),
            "expected pk mismatch error, got: {msg}"
        ),
        Err(e) => panic!("expected Err(Crypto), got different Err: {e:?}"),
        Ok(_) => panic!("expected Err(Crypto), got Ok"),
    }
}

/// G13 item 7: same as above but corrupt only the fingerprint column —
/// the recomputed fingerprint from the valid pubkey must still catch it.
#[test]
fn load_identity_detects_corrupted_fingerprint() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    let conn_arc = Arc::new(Mutex::new(conn));

    let kr = Keyring::new(conn_arc.clone());
    let _ = E2eManager::load_or_init(kr).unwrap();

    conn_arc
        .lock()
        .unwrap()
        .execute(
            "UPDATE e2e_identity SET fingerprint = ?1 WHERE id = 1",
            rusqlite::params![vec![0xeeu8; 16]],
        )
        .unwrap();

    let kr2 = Keyring::new(conn_arc);
    match E2eManager::load_or_init(kr2) {
        Err(E2eError::Crypto(msg)) => assert!(
            msg.contains("fingerprint does not match"),
            "expected fp mismatch error, got: {msg}"
        ),
        Err(e) => panic!("expected Err(Crypto), got different Err: {e:?}"),
        Ok(_) => panic!("expected Err(Crypto), got Ok"),
    }
}

// === Phase C: REKEY replay protection + previous-key retention ===

/// Handshake carol→alice on `#x` and return the parsed REKEY produced by
/// alice's next lazy rotation, plus the pre-rotation ciphertext `w1`.
fn setup_rotation(alice: &E2eManager, carol: &E2eManager) -> (String, crate::e2e::handshake::KeyRekey, Vec<String>) {
    enable_channel(alice, "#x", ChannelMode::AutoAccept);
    enable_channel(carol, "#x", ChannelMode::AutoAccept);
    let req = carol.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq("~carol@c.host", &req).unwrap().unwrap();
    carol.handle_keyrsp("~alice@a.host", &rsp).unwrap();

    // Message sent under the CURRENT (soon to be previous) key.
    let w1 = alice.encrypt_outgoing("#x", "in-flight under old key").unwrap();

    // Rotate: the next send regenerates the key and queues a REKEY to carol.
    alice.keyring().mark_outgoing_pending_rotation("#x").unwrap();
    let _w2 = alice.encrypt_outgoing("#x", "first under new key").unwrap();
    let rekeys = alice.take_pending_rekey_sends();
    assert_eq!(rekeys.len(), 1);
    let inner = rekeys[0]
        .notice_text
        .strip_prefix('\x01')
        .and_then(|s| s.strip_suffix('\x01'))
        .unwrap()
        .to_string();
    let parsed = crate::e2e::handshake::parse(&inner).unwrap().unwrap();
    let rk = match parsed {
        crate::e2e::handshake::HandshakeMsg::Rekey(r) => r,
        other => panic!("expected Rekey, got {other:?}"),
    };
    (inner, rk, w1)
}

#[test]
fn rekey_replay_is_rejected() {
    // A captured REKEY replayed later must not overwrite the current
    // incoming session (rollback/DoS): the signed nonce is single-use.
    let alice = make_manager();
    let carol = make_manager();
    let (_inner, rk, _w1) = setup_rotation(&alice, &carol);

    carol.handle_rekey("~alice@a.host", &rk).unwrap();
    let err = carol
        .handle_rekey("~alice@a.host", &rk)
        .expect_err("replayed REKEY must be rejected");
    assert!(
        err.to_string().to_lowercase().contains("replay"),
        "error should name the replay: {err}"
    );
}

#[test]
fn in_flight_ciphertext_under_previous_key_decrypts_after_rekey() {
    // NOTICE (REKEY) can overtake PRIVMSG ciphertext already sent under the
    // superseded key. The receiver must keep the previous key for a grace
    // window so the in-flight message still decrypts instead of failing AEAD
    // until a manual re-handshake.
    let alice = make_manager();
    let carol = make_manager();
    let (_inner, rk, w1) = setup_rotation(&alice, &carol);

    // REKEY arrives FIRST (reorder), replacing carol's incoming session…
    carol.handle_rekey("~alice@a.host", &rk).unwrap();

    // …then the older ciphertext lands. It must still decrypt.
    match carol.decrypt_incoming("~alice@a.host", "#x", &w1[0]).unwrap() {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "in-flight under old key"),
        other => panic!("in-flight message must decrypt under the previous key, got {other:?}"),
    }
}

#[test]
fn previous_key_grace_window_expires() {
    // The previous key is a short reorder tolerance, not a second long-lived
    // key: once the grace window has passed, old-key ciphertext is rejected.
    let alice = make_manager();
    let carol_conn = Connection::open_in_memory().unwrap();
    carol_conn.execute_batch(SCHEMA).unwrap();
    let carol_db = Arc::new(Mutex::new(carol_conn));
    let carol = E2eManager::load_or_init(Keyring::new(Arc::clone(&carol_db))).unwrap();
    let (_inner, rk, w1) = setup_rotation(&alice, &carol);

    carol.handle_rekey("~alice@a.host", &rk).unwrap();

    // Age the retained previous key far past the grace window.
    carol_db
        .lock()
        .unwrap()
        .execute(
            "UPDATE e2e_incoming_sessions SET prev_created_at = 1 WHERE channel = '#x'",
            [],
        )
        .unwrap();

    match carol.decrypt_incoming("~alice@a.host", "#x", &w1[0]).unwrap() {
        DecryptOutcome::Rejected(_) => {}
        other => panic!("expired previous key must not decrypt, got {other:?}"),
    }
}

// === Phase D: pending-handshake TTL ===

#[test]
fn stale_pending_handshakes_are_evicted() {
    // The initiator's in-memory pending map (ephemeral secrets awaiting a
    // KEYRSP) must not grow without bound across a long session: entries
    // past the TTL are pruned on the next handshake build, and a KEYRSP for
    // an evicted entry fails like any unknown handshake.
    let bob = make_manager();
    let alice = make_manager();
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);

    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq("~bob@b.host", &req).unwrap().unwrap();

    // Age bob's pending entry far past the TTL; the next build prunes.
    bob.age_pending_entries_for_test(1_000_000);
    let _ = bob.build_keyreq("#other").unwrap();

    let err = bob
        .handle_keyrsp("~alice@a.host", &rsp)
        .expect_err("KEYRSP for an evicted pending entry must fail");
    assert!(
        err.to_string().contains("no pending handshake"),
        "unexpected error: {err}"
    );
}

#[test]
fn stale_pending_inbound_keyreqs_are_evicted() {
    // Normal-mode inbound KEYREQs cached for /e2e accept get the same TTL
    // treatment (a much longer window — the user may be away). After
    // eviction, accept finds nothing; the peer simply re-handshakes.
    let alice = make_manager();
    enable_channel(&alice, "#x", ChannelMode::Normal);
    let bob = make_manager();
    let req = bob.build_keyreq("#x").unwrap();
    assert!(
        alice.handle_keyreq("~bob@b.host", &req).unwrap().is_none(),
        "normal mode caches the KEYREQ instead of answering"
    );

    alice.age_pending_entries_for_test(1_000_000);
    // Any new inbound handshake triggers the prune.
    let carol = make_manager();
    let req2 = carol.build_keyreq("#x").unwrap();
    let _ = alice.handle_keyreq("~carol@c.host", &req2).unwrap();

    assert!(
        alice
            .accept_pending_inbound("~bob@b.host", "#x")
            .unwrap()
            .is_none(),
        "the evicted inbound KEYREQ must be gone"
    );
}

#[test]
fn stale_pending_handshake_rejected_by_keyrsp_consumer_without_insert() {
    // Consumer-side TTL enforcement: a KEYRSP arriving after the TTL with NO
    // intervening handshake build (so nothing pruned on insert) must still
    // be rejected, and the stale ephemeral secret must not complete the
    // handshake. This is the gap the insert-only prune leaves open.
    let bob = make_manager();
    let alice = make_manager();
    enable_channel(&bob, "#x", ChannelMode::AutoAccept);
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);

    let req = bob.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq("~bob@b.host", &req).unwrap().unwrap();

    // Age bob's pending entry past the TTL — and do NOT build another
    // handshake, so the only thing that can evict it is the KEYRSP consumer.
    bob.age_pending_entries_for_test(1_000_000);

    let err = bob
        .handle_keyrsp("~alice@a.host", &rsp)
        .expect_err("a KEYRSP past the TTL must not complete a stale handshake");
    assert!(
        err.to_string().contains("no pending handshake"),
        "unexpected error: {err}"
    );
}

#[test]
fn stale_pending_inbound_rejected_by_accept_without_insert() {
    // Consumer-side TTL enforcement for the inbound cache: /e2e accept on a
    // KEYREQ that has sat past the inbound TTL, with NO intervening inbound
    // handshake to trigger pruning, must find nothing to accept.
    let alice = make_manager();
    enable_channel(&alice, "#x", ChannelMode::Normal);
    let bob = make_manager();
    let req = bob.build_keyreq("#x").unwrap();
    assert!(
        alice.handle_keyreq("~bob@b.host", &req).unwrap().is_none(),
        "normal mode caches the KEYREQ instead of answering"
    );

    // Age it past the inbound TTL; accept alone must evict + refuse.
    alice.age_pending_entries_for_test(1_000_000);

    assert!(
        alice
            .accept_pending_inbound("~bob@b.host", "#x")
            .unwrap()
            .is_none(),
        "a stale inbound KEYREQ must not be accepted"
    );
}

// === Phase E: network-scoped keyring contexts ===

#[test]
fn scoped_context_keeps_wire_fields_unscoped() {
    // Everything that leaves the process must carry the WIRE context —
    // the network label is a local storage detail. A scoped KEYREQ must
    // stamp c= with the bare channel and sign over it (interop with
    // clients that never scope).
    let alice = make_manager();
    let ctx = crate::e2e::scoped_context("NetA", "#x");
    let req = alice.build_keyreq(&ctx).unwrap();
    assert_eq!(req.channel, "#x", "c= must carry the wire context");

    // The signature must verify against the wire channel, exactly as an
    // unscoped peer would compute it.
    let payload = crate::e2e::handshake::signed_keyreq_payload(
        "#x",
        &req.pubkey,
        &req.eph_x25519,
        &req.nonce,
    );
    crate::e2e::crypto::sig::verify(&req.pubkey, &payload, &req.sig)
        .expect("signature must be over the wire channel");
}

#[test]
fn scoped_handshake_round_trip_and_network_isolation() {
    // Both sides key their storage under their own network label while the
    // wire carries only "#x". After the handshake, the session exists under
    // NetA's scoped context — and NOT under NetB's, even though the channel
    // name is identical.
    let alice = make_manager();
    let carol = make_manager();
    let a_ctx = crate::e2e::scoped_context("NetA", "#x");
    enable_channel(&alice, &a_ctx, ChannelMode::AutoAccept);
    enable_channel(&carol, &a_ctx, ChannelMode::AutoAccept);

    let req = carol.build_keyreq(&a_ctx).unwrap();
    assert_eq!(req.channel, "#x");
    // The receiving side scopes the wire channel with ITS network before
    // handing the message to the manager (what events.rs does).
    let mut scoped_req = req.clone();
    scoped_req.channel = crate::e2e::scoped_context("NetA", &req.channel);
    let rsp = alice
        .handle_keyreq("~carol@c.host", &scoped_req)
        .unwrap()
        .expect("AutoAccept must answer");
    assert_eq!(rsp.channel, "#x", "KEYRSP echoes the wire c= verbatim");

    let mut scoped_rsp = rsp.clone();
    scoped_rsp.channel = crate::e2e::scoped_context("NetA", &rsp.channel);
    carol.handle_keyrsp("~alice@a.host", &scoped_rsp).unwrap();

    // Encrypt/decrypt round trip under the scoped context.
    let wires = alice.encrypt_outgoing(&a_ctx, "scoped secret").unwrap();
    match carol
        .decrypt_incoming("~alice@a.host", &a_ctx, &wires[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "scoped secret"),
        other => panic!("scoped decrypt failed: {other:?}"),
    }

    // Isolation: the same channel name on another network has NO session
    // and NO config.
    let b_ctx = crate::e2e::scoped_context("NetB", "#x");
    assert!(
        carol
            .keyring()
            .get_incoming_session("~alice@a.host", &b_ctx)
            .unwrap()
            .is_none(),
        "NetB must not inherit NetA's session"
    );
    assert!(
        alice.keyring().get_channel_config(&b_ctx).unwrap().is_none(),
        "NetB must not inherit NetA's config"
    );
}

#[test]
fn legacy_unscoped_rows_still_resolve_after_upgrade() {
    // Pre-upgrade databases hold unscoped rows ("#x"). Scoped reads must
    // fall back to them so an existing E2E setup keeps decrypting and
    // keeps its config after the upgrade; scoped rows win when present.
    let alice = make_manager();
    let carol = make_manager();
    // Legacy handshake — everything stored unscoped.
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&carol, "#x", ChannelMode::AutoAccept);
    let req = carol.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq("~carol@c.host", &req).unwrap().unwrap();
    carol.handle_keyrsp("~alice@a.host", &rsp).unwrap();
    let wires = alice.encrypt_outgoing("#x", "legacy msg").unwrap();

    // Post-upgrade the caller passes scoped contexts.
    let scoped = crate::e2e::scoped_context("NetA", "#x");
    assert!(
        carol
            .keyring()
            .get_channel_config(&scoped)
            .unwrap()
            .is_some_and(|c| c.enabled),
        "scoped config read must fall back to the legacy row"
    );
    match carol
        .decrypt_incoming("~alice@a.host", &scoped, &wires[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "legacy msg"),
        other => panic!("legacy session must decrypt under a scoped read: {other:?}"),
    }
    // Our own outgoing key must also fall back — otherwise the first
    // post-upgrade send generates a fresh key without a REKEY and every
    // legacy peer fails AEAD.
    let post = alice.encrypt_outgoing(&scoped, "post-upgrade msg").unwrap();
    match carol
        .decrypt_incoming("~alice@a.host", "#x", &post[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "post-upgrade msg"),
        other => panic!("post-upgrade send must reuse the legacy outgoing key: {other:?}"),
    }
}

#[test]
fn scoped_dm_keyreq_still_skips_reciprocal() {
    // The DM test inside handle_keyreq must look at the WIRE part of a
    // scoped context — 'Net\x1F@handle' does not start with '@', and
    // treating it as a channel would fire a reciprocal KEYREQ keyed under
    // the wrong DM direction (the G13 bug all over again).
    let alice = make_manager();
    let dm_ctx = crate::e2e::scoped_context("NetA", "@~me@host");
    enable_channel(&alice, &dm_ctx, ChannelMode::AutoAccept);

    let bob = make_manager();
    let mut req = bob.build_keyreq(&dm_ctx).unwrap();
    assert_eq!(req.channel, "@~me@host");
    req.channel = dm_ctx;
    alice
        .handle_keyreq("~bob@b.host", &req)
        .unwrap()
        .expect("AutoAccept answers the DM KEYREQ");
    assert!(
        alice.take_pending_outbound_keyreqs().is_empty(),
        "a DM KEYREQ must not queue a reciprocal (per-direction handshakes)"
    );
}

#[test]
fn rotation_recipients_union_scoped_and_legacy_rows() {
    // Upgraded keyrings hold pre-scoping recipients under the legacy
    // unscoped channel while new handshakes record under the scoped one.
    // A rotate must REKEY the UNION of both — dropping the legacy list
    // would leave every pre-upgrade peer on the old key.
    let alice = make_manager();
    let scoped = crate::e2e::scoped_context("NetA", "#x");
    alice
        .keyring()
        .record_outgoing_recipient("#x", "~legacy@old.host", &[0xaa; 16], 100)
        .unwrap();
    alice
        .keyring()
        .record_outgoing_recipient(&scoped, "~new@new.host", &[0xbb; 16], 200)
        .unwrap();
    // The same peer present in both generations must appear once, with the
    // scoped row winning.
    alice
        .keyring()
        .record_outgoing_recipient("#x", "~both@dual.host", &[0xcc; 16], 100)
        .unwrap();
    alice
        .keyring()
        .record_outgoing_recipient(&scoped, "~both@dual.host", &[0xdd; 16], 200)
        .unwrap();

    let recipients = alice.keyring().list_outgoing_recipients(&scoped).unwrap();
    let handles: Vec<&str> = recipients.iter().map(|(h, _)| h.as_str()).collect();
    assert!(handles.contains(&"~legacy@old.host"), "legacy peer dropped: {handles:?}");
    assert!(handles.contains(&"~new@new.host"));
    assert_eq!(
        handles.iter().filter(|h| **h == "~both@dual.host").count(),
        1,
        "same peer in both generations must be deduped"
    );
    let both_fp = recipients
        .iter()
        .find(|(h, _)| h == "~both@dual.host")
        .map(|(_, fp)| *fp)
        .unwrap();
    assert_eq!(both_fp, [0xdd; 16], "the scoped row wins the dedup");
}

#[test]
fn trusted_peer_listing_unions_scoped_and_legacy_rows() {
    // /e2e list and the /e2e status peer count read
    // list_trusted_peers_for_channel — after the upgrade it must show BOTH
    // generations (dedup by handle, scoped wins), like the REKEY
    // recipient list.
    let mgr = make_manager();
    let scoped = crate::e2e::scoped_context("NetA", "#x");
    for (channel, handle, fpb) in [
        ("#x", "~legacy@old.host", 0xaau8),
        (scoped.as_str(), "~new@new.host", 0xbb),
        ("#x", "~both@dual.host", 0xcc),
        (scoped.as_str(), "~both@dual.host", 0xdd),
    ] {
        mgr.keyring()
            .set_incoming_session(&IncomingSession {
                handle: handle.to_string(),
                channel: channel.to_string(),
                fingerprint: [fpb; 16],
                sk: [1u8; 32],
                status: TrustStatus::Trusted,
                created_at: 100,
            })
            .unwrap();
    }

    let peers = mgr
        .keyring()
        .list_trusted_peers_for_channel(&scoped)
        .unwrap();
    let handles: Vec<&str> = peers.iter().map(|p| p.handle.as_str()).collect();
    assert!(handles.contains(&"~legacy@old.host"), "legacy peer hidden: {handles:?}");
    assert!(handles.contains(&"~new@new.host"));
    assert_eq!(
        handles.iter().filter(|h| **h == "~both@dual.host").count(),
        1
    );
    let both = peers.iter().find(|p| p.handle == "~both@dual.host").unwrap();
    assert_eq!(both.fingerprint, [0xdd; 16], "scoped row wins the dedup");
}

#[test]
fn first_scoped_rekey_retains_legacy_key_and_fingerprint_check() {
    // Upgraded keyring: the established Trusted session lives under the
    // legacy unscoped row. The FIRST scoped install (post-upgrade REKEY)
    // must consult that row — both for the fingerprint-continuity check
    // and as the prev-key source, or the reorder grace window is lost
    // exactly once per conversation after the upgrade.
    let kr_conn = Connection::open_in_memory().unwrap();
    kr_conn.execute_batch(SCHEMA).unwrap();
    let kr = Keyring::new(Arc::new(Mutex::new(kr_conn)));
    kr.set_incoming_session(&IncomingSession {
        handle: "~alice@a.host".into(),
        channel: "#x".into(),
        fingerprint: [0xaa; 16],
        sk: [1u8; 32],
        status: TrustStatus::Trusted,
        created_at: 100,
    })
    .unwrap();

    let scoped = crate::e2e::scoped_context("NetA", "#x");
    // Same fingerprint, new key → allowed, legacy key retained as prev.
    kr.install_incoming_session_strict(&IncomingSession {
        handle: "~alice@a.host".into(),
        channel: scoped.clone(),
        fingerprint: [0xaa; 16],
        sk: [2u8; 32],
        status: TrustStatus::Trusted,
        created_at: 200,
    })
    .unwrap();
    let (prev_sk, _) = kr
        .get_incoming_prev_key("~alice@a.host", &scoped)
        .unwrap()
        .expect("legacy key must be retained as prev on the first scoped install");
    assert_eq!(prev_sk, [1u8; 32]);

    // Different fingerprint under another scoped context with only a legacy
    // row → must be rejected like any TOFU fingerprint change.
    let fresh_conn = Connection::open_in_memory().unwrap();
    fresh_conn.execute_batch(SCHEMA).unwrap();
    let kr2 = Keyring::new(Arc::new(Mutex::new(fresh_conn)));
    kr2.set_incoming_session(&IncomingSession {
        handle: "~alice@a.host".into(),
        channel: "#x".into(),
        fingerprint: [0xaa; 16],
        sk: [1u8; 32],
        status: TrustStatus::Trusted,
        created_at: 100,
    })
    .unwrap();
    let err = kr2
        .install_incoming_session_strict(&IncomingSession {
            handle: "~alice@a.host".into(),
            channel: crate::e2e::scoped_context("NetA", "#x"),
            fingerprint: [0xbb; 16],
            sk: [2u8; 32],
            status: TrustStatus::Trusted,
            created_at: 200,
        })
        .expect_err("fingerprint change vs the legacy row must be rejected");
    assert!(err.to_string().contains("fp="), "unexpected error: {err}");
}

#[test]
fn renamed_network_label_heals_unambiguous_config_read() {
    // config.toml label rename ("Libera" → "LiberaChat") orphans every
    // scoped row. The enabled check is the fail-open point: it must find
    // the config when EXACTLY ONE other network's row shares the wire part
    // (unambiguous rename) — never when two networks both have one (real
    // cross-network isolation).
    let mgr = make_manager();
    let old_ctx = crate::e2e::scoped_context("Libera", "@~bob@b.host");
    enable_channel(&mgr, &old_ctx, ChannelMode::Normal);

    // The heal fires only when the sibling's label is no longer configured
    // (true rename signal) — declare the post-rename server set.
    mgr.keyring()
        .set_configured_networks(["LiberaChat".to_string(), "Rizon".to_string()]);

    let renamed = crate::e2e::scoped_context("LiberaChat", "@~bob@b.host");
    assert!(
        mgr.keyring()
            .get_channel_config(&renamed)
            .unwrap()
            .is_some_and(|c| c.enabled),
        "unambiguous rename must heal the enabled read (fail-open otherwise)"
    );

    // Ambiguous: a second network has its own row for the same wire part →
    // NO heal (isolation wins).
    enable_channel(
        &mgr,
        &crate::e2e::scoped_context("OFTC", "@~bob@b.host"),
        ChannelMode::Normal,
    );
    assert!(
        mgr.keyring()
            .get_channel_config(&crate::e2e::scoped_context("Rizon", "@~bob@b.host"))
            .unwrap()
            .is_none(),
        "two candidate networks → ambiguous → no heal"
    );
}

#[test]
fn multi_network_config_denies_legacy_fallback() {
    // With two networks configured, a legacy unscoped row has no
    // determinable owner. Serving it to every scoped miss would hand one
    // network's keys to ANY network sharing the wire name (`#x` on NetA
    // and NetB) — the reads must fail closed instead.
    let alice = make_manager();
    let carol = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&carol, "#x", ChannelMode::AutoAccept);
    let req = carol.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq("~carol@c.host", &req).unwrap().unwrap();
    carol.handle_keyrsp("~alice@a.host", &rsp).unwrap();
    carol
        .keyring()
        .record_outgoing_recipient("#x", "~legacy@old.host", &[0xaa; 16], 100)
        .unwrap();
    carol.keyring().add_autotrust("#x", "*", 100).unwrap();

    carol
        .keyring()
        .set_configured_networks(["NetA".to_string(), "NetB".to_string()]);

    let net_b = crate::e2e::scoped_context("NetB", "#x");
    assert!(
        carol.keyring().get_channel_config(&net_b).unwrap().is_none(),
        "NetB must not inherit NetA's legacy config"
    );
    assert!(
        carol
            .keyring()
            .get_incoming_session("~alice@a.host", &net_b)
            .unwrap()
            .is_none(),
        "NetB must not decrypt with the legacy session"
    );
    assert!(
        carol.keyring().get_outgoing_session(&net_b).unwrap().is_none(),
        "NetB must not encrypt with the legacy outgoing key"
    );
    assert!(
        carol
            .keyring()
            .list_outgoing_recipients(&net_b)
            .unwrap()
            .is_empty(),
        "NetB must not REKEY the legacy recipient list"
    );
    assert!(
        carol
            .keyring()
            .list_trusted_peers_for_channel(&net_b)
            .unwrap()
            .is_empty(),
        "NetB must not list the legacy trusted peers"
    );
    assert!(
        !carol
            .keyring()
            .autotrust_matches("~mallory@m.host", &net_b)
            .unwrap(),
        "a legacy autotrust rule must not auto-trust peers on another network"
    );
}

#[test]
fn single_network_startup_migrates_legacy_rows() {
    // Exactly one configured network is the unambiguous owner: startup
    // migration renames every context-keyed legacy row to its scope, so
    // scoped reads resolve exactly and nothing is left for the fallback.
    let alice = make_manager();
    let carol = make_manager();
    enable_channel(&alice, "#x", ChannelMode::AutoAccept);
    enable_channel(&carol, "#x", ChannelMode::AutoAccept);
    let req = carol.build_keyreq("#x").unwrap();
    let rsp = alice.handle_keyreq("~carol@c.host", &req).unwrap().unwrap();
    carol.handle_keyrsp("~alice@a.host", &rsp).unwrap();
    let wires = alice.encrypt_outgoing("#x", "pre-upgrade msg").unwrap();
    carol
        .keyring()
        .record_outgoing_recipient("#x", "~alice@a.host", &[0xaa; 16], 100)
        .unwrap();
    carol.keyring().add_autotrust("#x", "~alice*", 100).unwrap();

    carol.keyring().set_configured_networks(["NetA".to_string()]);
    let unattributed = carol.keyring().adopt_legacy_contexts().unwrap();
    assert!(unattributed.is_empty(), "single network is never ambiguous");
    assert!(
        carol.keyring().list_legacy_contexts().unwrap().is_empty(),
        "migration must leave no unscoped rows behind"
    );

    let scoped = crate::e2e::scoped_context("NetA", "#x");
    assert!(
        carol
            .keyring()
            .get_channel_config(&scoped)
            .unwrap()
            .is_some_and(|c| c.enabled),
        "config must resolve under the scoped key after migration"
    );
    match carol
        .decrypt_incoming("~alice@a.host", &scoped, &wires[0])
        .unwrap()
    {
        DecryptOutcome::Plaintext(s) => assert_eq!(s, "pre-upgrade msg"),
        other => panic!("migrated session must keep decrypting: {other:?}"),
    }
    assert_eq!(
        carol
            .keyring()
            .list_outgoing_recipients(&scoped)
            .unwrap()
            .len(),
        1,
        "recipient rows must migrate with the context"
    );
    assert!(
        carol
            .keyring()
            .autotrust_matches("~alice@a.host", &scoped)
            .unwrap(),
        "autotrust rules must migrate with the context"
    );
    // A different network's scoped read finds nothing — the migrated rows
    // belong to NetA now.
    assert!(
        carol
            .keyring()
            .get_channel_config(&crate::e2e::scoped_context("NetB", "#x"))
            .unwrap()
            .is_none(),
        "migrated rows must not leak to another network"
    );
}

#[test]
fn adoption_attributes_dm_context_via_handle_cache() {
    // Multi-network config, but the DM handle cache (network-keyed) has
    // seen the peer's handle on exactly one configured network — that
    // attribution is unambiguous, so the DM context migrates to it.
    let mgr = make_manager();
    let dm_wire = "@~bob@b.host";
    enable_channel(&mgr, dm_wire, ChannelMode::AutoAccept);
    mgr.keyring()
        .cache_dm_handle("NetA", "bob", "~bob@b.host")
        .unwrap();

    mgr.keyring()
        .set_configured_networks(["NetA".to_string(), "NetB".to_string()]);
    let unattributed = mgr.keyring().adopt_legacy_contexts().unwrap();
    assert!(
        unattributed.is_empty(),
        "cache-attributed DM context must migrate: {unattributed:?}"
    );
    assert!(
        mgr.keyring()
            .get_channel_config(&crate::e2e::scoped_context("NetA", dm_wire))
            .unwrap()
            .is_some_and(|c| c.enabled),
        "the DM config must now live under the cache's network"
    );
    assert!(
        mgr.keyring()
            .get_channel_config(&crate::e2e::scoped_context("NetB", dm_wire))
            .unwrap()
            .is_none(),
        "the other network must not see the migrated DM config"
    );
}

#[test]
fn adoption_never_attributes_channel_contexts_from_chat_logs() {
    // Multi-network config; the shared database's message log shows '#only'
    // active solely on NetA — but chat logs prove activity, not key
    // ownership: the pre-upgrade E2E rows could belong to a network whose
    // history is empty, excluded, or purged, and migrating on that evidence
    // would reuse the keys cross-network. Channel contexts therefore always
    // stay legacy on multi-network configs and are reported for the startup
    // warning; the read gate keeps them inert (fresh handshakes
    // re-establish sessions fail-closed).
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn.execute_batch(
        "CREATE TABLE messages (id INTEGER PRIMARY KEY, network TEXT NOT NULL,
                                buffer TEXT NOT NULL, timestamp INTEGER NOT NULL DEFAULT 0)",
    )
    .unwrap();
    conn.execute_batch("INSERT INTO messages (network, buffer) VALUES ('NetA', '#only');")
        .unwrap();
    let kr = Keyring::new(Arc::new(Mutex::new(conn)));
    let mgr = E2eManager::load_or_init(kr).unwrap();
    enable_channel(&mgr, "#only", ChannelMode::AutoAccept);

    mgr.keyring()
        .set_configured_networks(["NetA".to_string(), "NetB".to_string()]);
    let unattributed = mgr.keyring().adopt_legacy_contexts().unwrap();
    assert_eq!(
        unattributed,
        vec!["#only".to_string()],
        "channel contexts must never be attributed from the message log"
    );
    for network in ["NetA", "NetB"] {
        assert!(
            mgr.keyring()
                .get_channel_config(&crate::e2e::scoped_context(network, "#only"))
                .unwrap()
                .is_none(),
            "the legacy channel config must not migrate to '{network}'"
        );
    }
}

#[test]
fn nick_rename_carries_dm_handle_cache() {
    // An IRC NICK must re-key the (network, nick) handle cache row, or a
    // `/msg <new_nick>` with no live query buffer resolves no handle and
    // downgrades an E2E-enabled DM to plaintext. Scoped per network: the
    // same nick on another network is untouched, and a stale row already
    // holding the new nick is replaced (NICK is authoritative). The
    // network-agnostic e2e_peers.last_nick hint must NOT be rewritten: a
    // rename on NetA says nothing about a same-nick peer on NetB, and
    // moving their hint off the nick they still hold would break the
    // legacy_handle_for_nick fallback there (plaintext passthrough).
    let mgr = make_manager();
    let kr = mgr.keyring();
    kr.cache_dm_handle("NetA", "bob", "~bob@b.host").unwrap();
    kr.cache_dm_handle("NetA", "bobby", "~stale@old.host").unwrap();
    kr.cache_dm_handle("NetB", "bob", "~otherbob@x.host").unwrap();
    // A same-nick peer on ANOTHER network, resolvable only via the
    // legacy last_nick hint (no cache row for them on NetB).
    kr.upsert_peer(&crate::e2e::keyring::PeerRecord {
        fingerprint: [7u8; 16],
        pubkey: [9u8; 32],
        last_handle: Some("~carol@b.net".to_string()),
        last_nick: Some("carol".to_string()),
        first_seen: 0,
        last_seen: 1,
        global_status: crate::e2e::keyring::TrustStatus::Trusted,
    })
    .unwrap();

    kr.rename_dm_nick("NetA", "bob", "bobby").unwrap();
    kr.rename_dm_nick("NetA", "carol", "dave").unwrap();

    assert_eq!(
        kr.last_handle_for_nick("bobby", "NetA").unwrap().as_deref(),
        Some("~bob@b.host"),
        "the renamed peer must resolve under the new nick"
    );
    assert!(
        kr.last_handle_for_nick("bob", "NetA").unwrap().is_none(),
        "the old nick no longer belongs to the peer"
    );
    assert_eq!(
        kr.last_handle_for_nick("bob", "NetB").unwrap().as_deref(),
        Some("~otherbob@x.host"),
        "another network's row for the same nick must be untouched"
    );
    assert_eq!(
        kr.legacy_handle_for_nick("carol").unwrap().as_deref(),
        Some("~carol@b.net"),
        "a rename on one network must not clobber the last_nick hint of a \
         same-nick peer elsewhere — that hint is their only resolution path"
    );
}

#[test]
fn adoption_keeps_existing_scoped_row_on_conflict() {
    // A legacy row whose scoped twin already exists loses: every scoped
    // write postdates any pre-upgrade row, so the scoped one is kept and
    // the legacy one dropped (never the reverse, and never an error).
    let mgr = make_manager();
    enable_channel(&mgr, "#x", ChannelMode::AutoAccept); // legacy
    let scoped = crate::e2e::scoped_context("NetA", "#x");
    enable_channel(&mgr, &scoped, ChannelMode::Normal); // newer scoped twin

    mgr.keyring().set_configured_networks(["NetA".to_string()]);
    mgr.keyring().adopt_legacy_contexts().unwrap();

    let cfg = mgr.keyring().get_channel_config(&scoped).unwrap().unwrap();
    assert_eq!(
        cfg.mode,
        ChannelMode::Normal,
        "the existing scoped row must win the collision"
    );
    assert!(
        mgr.keyring().list_legacy_contexts().unwrap().is_empty(),
        "the colliding legacy row must be dropped, not kept"
    );
}

#[test]
fn adoption_skips_bare_nick_dm_rows() {
    // Pre-handle bare-nick DM rows (neither `#…` nor `@handle`) must stay
    // unscoped: the send gate consults them UNSCOPED for its fail-closed
    // NoPeerHandle refusal, and scoping them would move the row out of the
    // gate's reach — turning the refusal into a plaintext send. They are
    // also not reported as unattributed leftovers: the unscoped read path
    // still serves them, so warning "ignored" would be false.
    let mgr = make_manager();
    enable_channel(&mgr, "bob", ChannelMode::Normal);

    mgr.keyring().set_configured_networks(["NetA".to_string()]);
    let unattributed = mgr.keyring().adopt_legacy_contexts().unwrap();

    assert!(
        unattributed.is_empty(),
        "bare-nick rows are skipped, not reported as unattributed"
    );
    assert!(
        mgr.keyring()
            .get_channel_config("bob")
            .unwrap()
            .is_some_and(|c| c.enabled),
        "the bare-nick row must remain readable under its unscoped key"
    );
    assert!(
        mgr.keyring().list_legacy_contexts().unwrap().is_empty(),
        "bare-nick rows must not appear in the startup-warning listing"
    );
}
