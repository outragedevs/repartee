//! Integration tests for the RPE2E handshake + encrypt/decrypt pipeline.
//!
//! These live inside the crate (not in `tests/`) because `repartee` is a
//! binary crate with no `lib.rs`, so external integration tests cannot
//! reach private modules. The file is `#[cfg(test)]`-gated by
//! `src/e2e/mod.rs`.

#![allow(clippy::unwrap_used, reason = "test code")]

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::e2e::keyring::{ChannelConfig, ChannelMode, Keyring};
use crate::e2e::manager::{DecryptOutcome, E2eManager};

const SCHEMA: &str = "
CREATE TABLE e2e_identity (id INTEGER PRIMARY KEY CHECK (id = 1), pubkey BLOB NOT NULL, privkey BLOB NOT NULL, fingerprint BLOB NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE e2e_peers (fingerprint BLOB PRIMARY KEY, pubkey BLOB NOT NULL, last_handle TEXT, last_nick TEXT, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, global_status TEXT NOT NULL DEFAULT 'pending');
CREATE TABLE e2e_outgoing_sessions (channel TEXT PRIMARY KEY, sk BLOB NOT NULL, created_at INTEGER NOT NULL, pending_rotation INTEGER NOT NULL DEFAULT 0);
CREATE TABLE e2e_incoming_sessions (handle TEXT NOT NULL, channel TEXT NOT NULL, fingerprint BLOB NOT NULL, sk BLOB NOT NULL, status TEXT NOT NULL DEFAULT 'pending', created_at INTEGER NOT NULL, PRIMARY KEY (handle, channel));
CREATE TABLE e2e_channel_config (channel TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 0, mode TEXT NOT NULL DEFAULT 'normal');
CREATE TABLE e2e_autotrust (id INTEGER PRIMARY KEY AUTOINCREMENT, scope TEXT NOT NULL, handle_pattern TEXT NOT NULL, created_at INTEGER NOT NULL, UNIQUE(scope, handle_pattern));
";

fn make_manager() -> E2eManager {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    let kr = Keyring::new(Arc::new(Mutex::new(conn)));
    E2eManager::load_or_init(kr).unwrap()
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
    alice
        .handle_keyrsp("~bob@b.host", &rsp_to_alice)
        .unwrap();

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
    let summary =
        crate::e2e::portable::export_to_path(alice.keyring(), tmp.path()).unwrap();
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
    let imported =
        crate::e2e::portable::import_from_path(carol.keyring(), tmp.path()).unwrap();
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
    assert_eq!(
        carol_incoming[0].status,
        alice_incoming_before[0].status
    );
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
