//! Integration tests for the RPE2E handshake + encrypt/decrypt pipeline.
//!
//! These live inside the crate (not in `tests/`) because `repartee` is a
//! binary crate with no `lib.rs`, so external integration tests cannot
//! reach private modules. The file is `#[cfg(test)]`-gated by
//! `src/e2e/mod.rs`.

#![allow(clippy::unwrap_used, reason = "test code")]

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::e2e::error::E2eError;
use crate::e2e::keyring::{ChannelConfig, ChannelMode, IncomingSession, Keyring, TrustStatus};
use crate::e2e::manager::{DecryptOutcome, E2eManager, ReverifyOutcome, TrustChange};

const SCHEMA: &str = "
CREATE TABLE e2e_identity (id INTEGER PRIMARY KEY CHECK (id = 1), pubkey BLOB NOT NULL, privkey BLOB NOT NULL, fingerprint BLOB NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE e2e_peers (fingerprint BLOB PRIMARY KEY, pubkey BLOB NOT NULL, last_handle TEXT, last_nick TEXT, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, global_status TEXT NOT NULL DEFAULT 'pending');
CREATE TABLE e2e_outgoing_sessions (channel TEXT PRIMARY KEY, sk BLOB NOT NULL, created_at INTEGER NOT NULL, pending_rotation INTEGER NOT NULL DEFAULT 0);
CREATE TABLE e2e_incoming_sessions (handle TEXT NOT NULL, channel TEXT NOT NULL, fingerprint BLOB NOT NULL, sk BLOB NOT NULL, status TEXT NOT NULL DEFAULT 'pending', created_at INTEGER NOT NULL, PRIMARY KEY (handle, channel));
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
    //   3. User runs `/e2e reverify` — `mgr.reverify_peer("~bob@b.host")`
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
    let outcome = alice.reverify_peer(bob_handle).unwrap();
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
    match alice.reverify_peer(bob_handle).unwrap() {
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
    match alice.reverify_peer("~ghost@nowhere.test").unwrap() {
        ReverifyOutcome::NotFound => {}
        other => panic!("expected NotFound, got {other:?}"),
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
