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
    // as his incoming session for alice.
    bob.handle_keyrsp(alice_handle, &alice.identity_pub(), &rsp)
        .unwrap();

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
    bob.handle_keyrsp("~alice@a.host", &alice.identity_pub(), &rsp)
        .unwrap();

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
        peer.handle_keyrsp("~alice@a.host", &alice.identity_pub(), &rsp)
            .unwrap();
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
