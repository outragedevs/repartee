//! Fixed vectors for the cryptography behind stored data and wire messages.
//!
//! Values marked "recorded" were produced by the crate versions in use before
//! the 2026-10 dependency migration (aes-gcm 0.10, chacha20poly1305 0.10,
//! ed25519-dalek 2, x25519-dalek 2, hkdf 0.12, hmac 0.12, pbkdf2 0.12,
//! sha2 0.10, p256 0.13, bip39 2). They prove that data written and messages
//! sent by older builds still read back, which a round-trip through the
//! current crates cannot show. The remaining values are published vectors.

#![allow(clippy::unwrap_used, reason = "test code")]

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::e2e::crypto::{aead, ecdh, fingerprint, identity::Identity, sig};
use crate::e2e::handshake::{self, HandshakeMsg};
use crate::e2e::keyring::{Keyring, PeerRecord, TrustStatus};
use crate::e2e::manager::{DecryptOutcome, E2eManager};

fn h<const N: usize>(s: &str) -> [u8; N] {
    hex::decode(s).unwrap().try_into().unwrap()
}

const STORAGE_KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// Recorded: an encrypted log line, AES-256-GCM with IV `24` repeated.
#[test]
fn recorded_log_ciphertext_still_decrypts() {
    let key = crate::storage::crypto::import_key(STORAGE_KEY_HEX).unwrap();
    let ct = "dYXWvYIbgm2iZmNN/8EfUMNx59VYjLCPCwjjsivpc9yOpXt2tbSLeA==";
    let iv = [0x24; 12];
    assert_eq!(
        crate::storage::crypto::decrypt(ct, &iv, &key).unwrap(),
        "stored message from 0.10"
    );

    let mut tampered = STANDARD.decode(ct).unwrap();
    tampered[0] ^= 1;
    assert!(crate::storage::crypto::decrypt(&STANDARD.encode(tampered), &iv, &key).is_err());
    assert!(crate::storage::crypto::decrypt(ct, &[0x25; 12], &key).is_err());
}

/// Recorded: a keyring secret blob (`version || iv || ciphertext`).
#[test]
fn recorded_keyring_secret_blob_still_decrypts() {
    let key = crate::storage::crypto::import_key(STORAGE_KEY_HEX).unwrap();
    let mut blob = hex::decode(
        "01353535353535353535353535f51c30cea3dc091bd809c1a34822439aff348a\
         0116369b25194f47029f0c2666ac524b884b824c7da050510a5d59154b",
    )
    .unwrap();
    assert_eq!(
        crate::storage::crypto::decrypt_bytes(&blob, &key).unwrap(),
        [0x77; 32]
    );

    let last = blob.len() - 1;
    blob[last] ^= 1;
    assert!(crate::storage::crypto::decrypt_bytes(&blob, &key).is_err());
}

/// RFC 8032 §7.1, tests 1 and 2.
#[test]
fn ed25519_rfc8032_vectors() {
    let cases = [
        (
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            &b""[..],
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ),
        (
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            &[0x72][..],
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ),
    ];
    for (seed, public, message, signature) in cases {
        let id = Identity::from_secret_bytes(&h(seed));
        assert_eq!(id.public_bytes(), h::<32>(public));
        assert_eq!(sig::sign(id.signing_key(), message), h::<64>(signature));
        sig::verify(&h(public), message, &h(signature)).unwrap();
        let mut forged = h::<64>(signature);
        forged[0] ^= 1;
        assert!(sig::verify(&h(public), message, &forged).is_err());
    }
}

/// Recorded: everything derived from a stored identity seed, including the
/// fingerprint and the six SAS words users compare out of band.
#[test]
fn recorded_identity_derivations() {
    let seed = [0x51; 32];
    let id = Identity::from_secret_bytes(&seed);
    let public = id.public_bytes();
    assert_eq!(
        public,
        h::<32>("c050c5637a44fa8629fff3cccce2300cb362a63d99d95fc54145266f4332445a")
    );
    let fp = fingerprint::fingerprint(&public);
    assert_eq!(
        fingerprint::fingerprint_hex(&fp),
        "e476a82d7224d774214834d8ce3bb4a0"
    );
    assert_eq!(
        fingerprint::fingerprint_bip39(&fp).unwrap(),
        "tongue relief area tone estate ring"
    );
    assert_eq!(
        ecdh::ed25519_pub_to_x25519(&public).unwrap(),
        h::<32>("f03e928f87e9a0773cea5b00b22d23e0566296a00e05e68598b9e1b4f4025a45")
    );
    assert_eq!(
        *ecdh::ed25519_seed_to_x25519(&seed),
        h::<32>("e31307cc6ef451125d04803268525ce085ed67b1d7c7886e9bdcbfbfa505020a")
    );
}

/// draft-irtf-cfrg-xchacha-03 §A.3.1.
#[test]
fn xchacha20poly1305_draft_vector() {
    let key = h::<32>("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
    let nonce = h::<24>("404142434445464748494a4b4c4d4e4f5051525354555657");
    let aad = hex::decode("50515253c0c1c2c3c4c5c6c7").unwrap();
    let ciphertext = hex::decode(
        "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb\
         731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452\
         2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9\
         21f9664c97637da9768812f615c68b13b52e\
         c0875924c1c7987947deafd8780acf49",
    )
    .unwrap();
    assert_eq!(
        aead::decrypt(&key, &nonce, &aad, &ciphertext).unwrap().as_slice(),
        b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it."
    );
}

/// RFC 5869 §A.1 (HKDF-SHA256) and RFC 4231 §4.3 (HMAC-SHA256).
#[test]
fn hkdf_and_hmac_published_vectors() {
    use hmac::Mac as _;

    let hk = hkdf::Hkdf::<sha2::Sha256>::new(
        Some(&hex::decode("000102030405060708090a0b0c").unwrap()),
        &[0x0b; 22],
    );
    let mut okm = [0; 42];
    hk.expand(&hex::decode("f0f1f2f3f4f5f6f7f8f9").unwrap(), &mut okm)
        .unwrap();
    assert_eq!(
        hex::encode(okm),
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
    );

    let mut mac = <hmac::Hmac<sha2::Sha256> as hmac::KeyInit>::new_from_slice(b"Jefe").unwrap();
    mac.update(b"what do ya want for nothing?");
    assert_eq!(
        hex::encode(mac.finalize().into_bytes()),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

/// Recorded: the key wrap and signature a responder sends in KEYRSP, as an
/// old initiator would receive them.
#[test]
fn recorded_keyrsp_wrap_and_signature() {
    let responder = Identity::from_secret_bytes(&[0x51; 32]);
    let responder_eph = h::<32>("4049502db92ca2342c3f92dac5d6de7c85db5df5407a5b4996ce39f2efb7e827");
    let initiator_eph = h::<32>("ab4f197998fcc56cc6ed68c1d931af9bb522ec00743e181f7330915df4aa3176");
    let info = b"RPE2E01-WRAP:#rust";

    // Both ends of the ECDH must reach the recorded wrap key.
    let wrap = h::<32>("f5ed7b3a40cb6e38c8d173665fb5049241fcad0de3dbfcd6f98d28ec6226cea1");
    assert_eq!(
        *ecdh::static_derive_wrap_key(&[0x61; 32], &initiator_eph, info),
        wrap
    );
    assert_eq!(
        *ecdh::static_derive_wrap_key(&[0x71; 32], &responder_eph, info),
        wrap
    );

    let wrap_nonce = [0x81; 24];
    let wrap_ct = hex::decode(
        "f2ca2a6a06bf4cdd96d1a77b2c199e67bfa7b760e344f59ec85f4dc686ce1b5f\
         193638a73ed6ae1382634ef1c0530c9d",
    )
    .unwrap();
    assert_eq!(
        aead::decrypt(&wrap, &wrap_nonce, info, &wrap_ct)
            .unwrap()
            .as_slice(),
        [0x42; 32]
    );

    let payload = handshake::signed_keyrsp_payload(
        "#rust",
        &responder.public_bytes(),
        &responder_eph,
        &wrap_nonce,
        &wrap_ct,
        &[0x91; 16],
    );
    let signature = h::<64>(
        "af6b1ffe190c1c5c8155701ae1da7f379966d6fa0a3cba524d99c27db767acd5\
         c12ed5552d3a908c7498bb4da4fb29acdc2c0f1cec63ebd7909bbd06d3ccc508",
    );
    assert_eq!(sig::sign(responder.signing_key(), &payload), signature);
    sig::verify(&responder.public_bytes(), &payload, &signature).unwrap();
}

/// Recorded: a REKEY CTCP and a channel message from a peer on the old
/// crates, received by the current client through the production path.
#[test]
fn recorded_peer_rekey_and_message_are_accepted() {
    const REKEY: &str = "RPEE2E REKEY v=1 c=#rust p=wFDFY3pE-oYp__PMzOIwDLNipj2Z2V_FQUUmb0MyRFo \
        e=QElQLbksojQsP5LaxdbefIXbXfVAeltJls458u-36Cc wn=gYGBgYGBgYGBgYGBgYGBgYGBgYGBgYGB \
        w=3jD9zrVAIetvDAXFUz8DxzgJtAgmQxA0GOPWvT8GGwZj-dn_cDfh0IAxD15JW_f5 n=kZGRkZGRkZGRkZGRkZGRkQ \
        s=G9SpILar1iNOddduCwT7YUrLruNTDZMpPCTLimmG1HgY2bqGS4pD3D86CEVVYOkaiS2jvm4I2MC9LeosZZxvCQ";
    const MESSAGE: &str = "+RPE2E01 0102030405060708 1760000000 1/1 \
        JCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk:zTu7CQEEKtTNQh9ZZuluolPDvsoTcw2sElHTbVs+6v2EuKYGbzZIPcAZlHpQ4CLp";
    let bob_handle = "~bob@b.host";

    let conn = crate::storage::db::open_database_at(":memory:", false).unwrap();
    let keyring = Keyring::new(Arc::new(Mutex::new(conn)));
    let alice = Identity::from_secret_bytes(&[0x52; 32]);
    let alice_pub = alice.public_bytes();
    assert_eq!(
        alice_pub,
        h::<32>("2012cb90ca60e8e5d8daf66e2272d2233e0486d557e8c66141ed8920177d7eb7")
    );
    keyring
        .save_identity(
            &alice_pub,
            &alice.secret_bytes(),
            &fingerprint::fingerprint(&alice_pub),
            1,
        )
        .unwrap();
    let bob_pub = Identity::from_secret_bytes(&[0x51; 32]).public_bytes();
    keyring
        .upsert_peer(&PeerRecord {
            fingerprint: fingerprint::fingerprint(&bob_pub),
            pubkey: bob_pub,
            last_handle: Some(bob_handle.into()),
            last_nick: Some("bob".into()),
            first_seen: 1,
            last_seen: 1,
            global_status: TrustStatus::Trusted,
        })
        .unwrap();
    // The message timestamp is fixed, so widen the replay window to cover it.
    let config = crate::config::E2eConfig {
        ts_tolerance_secs: i64::MAX,
        ..Default::default()
    };
    let manager = E2eManager::load_or_init_with_config(keyring, &config).unwrap();
    assert_eq!(manager.identity_pub(), alice_pub);

    let decrypt = |line: &str| manager.decrypt_incoming(bob_handle, "#rust", line).unwrap();
    assert!(matches!(
        decrypt(MESSAGE),
        DecryptOutcome::MissingKey { .. }
    ));

    let Some(HandshakeMsg::Rekey(rekey)) = handshake::parse(REKEY).unwrap() else {
        panic!("recorded REKEY must parse");
    };
    manager.handle_rekey(bob_handle, &rekey).unwrap();
    assert!(
        manager.handle_rekey(bob_handle, &rekey).is_err(),
        "a replayed REKEY is refused"
    );

    match decrypt(MESSAGE) {
        DecryptOutcome::Plaintext(text) => assert_eq!(text, "hello from chacha20poly1305 0.10"),
        other => panic!("recorded message must decrypt, got {other:?}"),
    }
    let tampered = MESSAGE.replace(":zTu7", ":zTu8");
    assert!(matches!(decrypt(&tampered), DecryptOutcome::Rejected(_)));
}

/// Recorded: SCRAM-SHA-512 has no published vector, so pin the old output.
/// SHA-1 and SHA-256 are covered by the RFC vectors in `sasl_scram`.
#[test]
fn recorded_scram_sha512_exchange() {
    let (client_final, server_signature) = crate::irc::sasl_scram::client_final(
        crate::irc::sasl_scram::ScramHash::Sha512,
        "r=nonce123server,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
        "n=user,r=nonce123",
        "nonce123",
        "pencil",
    )
    .unwrap();
    assert_eq!(
        client_final,
        "c=biws,r=nonce123server,\
         p=OzBhu4dG30WbBlUWhz4nSOJ+V4NT7J91G0O7YJB0e36RRUpAkM+RarTXt5+Xq1X72qTwkuHf8p5mVSlgMketwA=="
    );
    assert!(crate::irc::sasl_scram::verify_server(
        "v=ksVZGPncam4woe3dv6XYCaA6o5/J7k8ykx9Eo1+R9/zomRZBZgQcxDmFC0vqrs47V3Vk0ao0aRjZTyNVALWZEg==",
        &server_signature
    ));
}

/// Recorded: the deterministic (RFC 6979) DER signature over a challenge,
/// which `openssl pkeyutl -verify` also accepted for this key.
#[test]
fn recorded_ecdsa_challenge_signature() {
    use p256::ecdsa::signature::hazmat::PrehashVerifier as _;

    let pem = "-----BEGIN EC PRIVATE KEY-----\n\
        MHcCAQEEIJPs4/mrOjo5IUswCWuzw6AXrr8XkSqvO4Y0EeDNsOByoAoGCCqGSM49\n\
        AwEHoUQDQgAEBznG72VV2v/dEYvJevrBAnhW+uoQUtVWuFXXSGxZWMl6xR8VgzoI\n\
        p72n2RTnVCJ1aKv3iRxTWAC1sYSuxS448A==\n\
        -----END EC PRIVATE KEY-----\n";
    let key = crate::irc::sasl_ecdsa::load_key(pem).unwrap();
    assert_eq!(
        hex::encode(key.verifying_key().to_sec1_point(true).as_bytes()),
        "020739c6ef6555daffdd118bc97afac1027856faea1052d556b855d7486c5958c9"
    );
    let challenge: Vec<u8> = (0..32).collect();
    let recorded = hex::decode(
        "3045022100f120d2b05ee2dd3f0470d3dc22481a36a894f9b5d70daac43d644fb633a2a537\
         0220382010e7e9d4a993b11dec5f21048ef5b03f4bf070b6ac3c6cc848297937cfa8",
    )
    .unwrap();
    assert_eq!(
        crate::irc::sasl_ecdsa::sign_challenge(&key, &challenge).unwrap(),
        recorded
    );
    let signature = p256::ecdsa::Signature::from_der(&recorded).unwrap();
    key.verifying_key()
        .verify_prehash(&challenge, &signature)
        .unwrap();
}
