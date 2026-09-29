//! v2 invite state machine and the shared Python/Rust/Swift vectors.
use riwork_remote::{config::Storage, crypto::*};
use serde_json::Value;
use std::sync::{Arc, Barrier};

fn fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/v2.json")).unwrap()
}
fn identity(f: &Value) -> Identity {
    Identity {
        desktop_id: f["desktop_id"].as_str().unwrap().into(),
        device_id: f["device_id"].as_str().unwrap().into(),
        route_id: f["route_id"].as_str().unwrap().into(),
    }
}
fn bytes(f: &Value, key: &str) -> [u8; 32] {
    let raw = hex::decode(f[key].as_str().unwrap()).unwrap();
    raw.try_into().unwrap()
}
fn sessions_from_fixture() -> (Session, Session, [u8; 32]) {
    let f = fixture();
    let id = identity(&f);
    let root = bytes(&f, "root_key_hex");
    let client_private = bytes(&f, "client_private_hex");
    let desktop_private = bytes(&f, "desktop_private_hex");
    let (hello, _) = client_hello_v2(&id, &root, client_private).unwrap();
    let server: ServerHelloV2 = serde_json::from_value(f["server_hello"].clone()).unwrap();
    let (finish, mobile) = accept_server_hello_v2(&id, &root, client_private, &server).unwrap();
    let (_, pending) = accept_client_hello_v2(&id, &root, &hello, desktop_private).unwrap();
    (mobile, pending.finish(&finish).unwrap(), root)
}

#[test]
fn published_vectors_match_rust_for_invite_and_session() {
    let f = fixture();
    let id = identity(&f);
    let invite = decode::<32>(f["invite_secret"].as_str().unwrap()).unwrap();
    let pair_c = decode::<32>(f["pair_client_nonce"].as_str().unwrap()).unwrap();
    let pair_d = decode::<32>(f["pair_desktop_nonce"].as_str().unwrap()).unwrap();
    let expires = f["expires_at"].as_u64().unwrap();
    let relay = f["relay_url"].as_str().unwrap();
    let invite_id = f["invite_id"].as_str().unwrap();
    let hello = pair_hello(&id, relay, invite_id, expires, &invite, pair_c).unwrap();
    assert_eq!(serde_json::to_value(&hello).unwrap(), f["pair_hello"]);
    assert!(
        !serde_json::to_string(&hello)
            .unwrap()
            .contains(f["invite_secret"].as_str().unwrap())
    );
    let (accept, root, transcript) =
        complete_pair(&id, relay, invite_id, expires, &invite, &hello, pair_d).unwrap();
    assert_eq!(hex::encode(root), f["root_key_hex"]);
    assert_eq!(hex::encode(&transcript), f["pair_transcript_hex"]);
    assert_eq!(serde_json::to_value(&accept).unwrap(), f["pair_accept"]);
    let (finish, derived, _) =
        accept_pair(&id, relay, invite_id, expires, &invite, &pair_c, &accept).unwrap();
    assert_eq!(derived, root);
    assert_eq!(serde_json::to_value(&finish).unwrap(), f["pair_finish"]);
    verify_pair_finish(&root, &transcript, &finish).unwrap();

    let client_private = bytes(&f, "client_private_hex");
    let desktop_private = bytes(&f, "desktop_private_hex");
    let (client_hello, public) = client_hello_v2(&id, &root, client_private).unwrap();
    assert_eq!(hex::encode(public), f["client_public_hex"]);
    assert_eq!(
        serde_json::to_value(&client_hello).unwrap(),
        f["client_hello"]
    );
    let (server, pending) =
        accept_client_hello_v2(&id, &root, &client_hello, desktop_private).unwrap();
    assert_eq!(serde_json::to_value(&server).unwrap(), f["server_hello"]);
    let (mobile_finish, mut mobile) =
        accept_server_hello_v2(&id, &root, client_private, &server).unwrap();
    assert_eq!(
        serde_json::to_value(&mobile_finish).unwrap(),
        f["client_finish"]
    );
    let mut desktop = pending.finish(&mobile_finish).unwrap();
    assert_eq!(hex::encode(mobile.c2d), f["c2d_key_hex"]);
    assert_eq!(hex::encode(mobile.d2c), f["d2c_key_hex"]);
    assert_eq!(b64(&mobile.id), f["session_id"]);
    for (index, direction) in ["c2d", "d2c"].iter().enumerate() {
        let expected: Envelope =
            serde_json::from_value(f["frames"][index]["envelope"].clone()).unwrap();
        let plain = f["frames"][index]["plaintext_utf8"]
            .as_str()
            .unwrap()
            .as_bytes();
        assert_eq!(
            serde_json::to_value(mobile.seal(direction, plain).unwrap()).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(desktop.open(direction, &expected).unwrap(), plain);
    }
}

#[test]
fn different_ephemeral_keys_cannot_open_a_recorded_frame() {
    let (mut recorder, _, root) = sessions_from_fixture();
    let frame = recorder.seal("c2d", b"recorded session").unwrap();
    // A second derivation of the same transcript is still at counter 0, so it can open the frame.
    let (mut first, _, _) = sessions_from_fixture();
    let f = fixture();
    let id = identity(&f);
    let mut other_private = bytes(&f, "desktop_private_hex");
    other_private[0] ^= 0x5a;
    let (hello, _) = client_hello_v2(&id, &root, bytes(&f, "client_private_hex")).unwrap();
    let (server, pending) = accept_client_hello_v2(&id, &root, &hello, other_private).unwrap();
    let (finish, _) =
        accept_server_hello_v2(&id, &root, bytes(&f, "client_private_hex"), &server).unwrap();
    let mut second = pending.finish(&finish).unwrap();
    assert_ne!(first.c2d, second.c2d);
    assert!(second.open("c2d", &frame).is_err());
    let mut tampered = frame.clone();
    tampered.ciphertext.replace_range(
        0..1,
        if frame.ciphertext.starts_with('A') {
            "B"
        } else {
            "A"
        },
    );
    assert!(first.open("c2d", &tampered).is_err());
    assert_eq!(first.open("c2d", &frame).unwrap(), b"recorded session");
}

fn paired_v2(ttl: u64) -> (Storage, riwork_remote::config::Pairing, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::at(dir.path().into()).unwrap();
    let pairing = storage
        .pair_with(
            "wss://example.com/v1/ws".into(),
            "phone".into(),
            false,
            &dir.path().join("phone.json"),
            Some(&dir.path().join("routes.json")),
            2,
            ttl,
        )
        .unwrap();
    (storage, pairing, dir)
}

#[test]
fn bad_proof_does_not_consume_expiry_wipes_and_replay_loses() {
    let (storage, pairing, dir) = paired_v2(30);
    let secret = decode::<32>(pairing.invite_secret.as_deref().unwrap()).unwrap();
    let expires = pairing.expires_at.unwrap();
    let hello = pair_hello(
        &pairing.identity(),
        &pairing.relay_url,
        pairing.invite_id.as_deref().unwrap(),
        expires,
        &secret,
        [9u8; 32],
    )
    .unwrap();
    let mut bad = hello.clone();
    bad.mac = b64(&[7u8; 32]);
    let rejected = storage
        .redeem_invite(&bad, expires - 1, [1u8; 32])
        .unwrap_err();
    assert_eq!(rejected.to_string(), "invite_rejected");
    let still = storage.config().unwrap().devices.remove(0);
    assert_eq!(still.pairing.invite_state.as_deref(), Some("pending"));
    assert!(still.pairing.invite_secret.is_some());

    let expired = storage
        .redeem_invite(&hello, expires, [2u8; 32])
        .unwrap_err();
    assert_eq!(expired.to_string(), "invite_expired");
    let saved = std::fs::read_to_string(storage_dir(&storage)).unwrap();
    assert!(!saved.contains(pairing.invite_secret.as_deref().unwrap()));
    let after = storage.config().unwrap().devices.remove(0);
    assert_eq!(after.pairing.invite_state.as_deref(), Some("expired"));
    assert!(after.pairing.invite_secret.is_none());
    assert!(after.pairing.root_key.is_none());
    let again = storage
        .redeem_invite(&hello, expires - 1, [3u8; 32])
        .unwrap_err();
    assert_eq!(again.to_string(), "invite_expired");
    let routes = std::fs::read_to_string(dir.path().join("routes.json")).unwrap();
    assert!(!routes.contains(pairing.invite_secret.as_deref().unwrap()));
    assert!(!routes.contains(&b64(&secret)));
}

fn storage_dir(storage: &Storage) -> std::path::PathBuf {
    storage.dir.join("devices.json")
}

#[test]
fn one_winner_replay_and_overlapping_race() {
    let (storage, pairing, dir) = paired_v2(600);
    let secret = decode::<32>(pairing.invite_secret.as_deref().unwrap()).unwrap();
    let expires = pairing.expires_at.unwrap();
    let now = expires - 10;
    let hello = pair_hello(
        &pairing.identity(),
        &pairing.relay_url,
        pairing.invite_id.as_deref().unwrap(),
        expires,
        &secret,
        [4u8; 32],
    )
    .unwrap();
    let hold = storage.testing_hold_invite(pairing.invite_id.as_deref().unwrap());
    let raced = storage.redeem_invite(&hello, now, [5u8; 32]).unwrap_err();
    assert_eq!(raced.to_string(), "invite_race");
    assert_eq!(
        storage.config().unwrap().devices[0]
            .pairing
            .invite_state
            .as_deref(),
        Some("pending")
    );
    drop(hold);
    let won = storage.redeem_invite(&hello, now, [6u8; 32]).unwrap();
    assert_eq!(won.accept.kind, "pair_accept");
    let replay = storage.redeem_invite(&hello, now, [7u8; 32]).unwrap_err();
    assert_eq!(replay.to_string(), "invite_replay");
    let saved = std::fs::read_to_string(storage_dir(&storage)).unwrap();
    assert!(!saved.contains(pairing.invite_secret.as_deref().unwrap()));
    assert!(saved.contains(&b64(&won.root_key)));
    let export = std::fs::read_to_string(dir.path().join("phone.json")).unwrap();
    assert!(export.contains(pairing.invite_secret.as_deref().unwrap()));

    let (storage, pairing, _dir) = paired_v2(600);
    let secret = decode::<32>(pairing.invite_secret.as_deref().unwrap()).unwrap();
    let expires = pairing.expires_at.unwrap();
    let hello = Arc::new(
        pair_hello(
            &pairing.identity(),
            &pairing.relay_url,
            pairing.invite_id.as_deref().unwrap(),
            expires,
            &secret,
            [8u8; 32],
        )
        .unwrap(),
    );
    let storage = Arc::new(storage);
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for n in 0..4u8 {
        let (storage, hello, barrier) = (storage.clone(), hello.clone(), barrier.clone());
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            storage.redeem_invite(&hello, expires - 1, [n; 32])
        }));
    }
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    for result in results {
        if let Err(error) = result {
            let code = error.to_string();
            assert!(code == "invite_race" || code == "invite_replay", "{code}");
        }
    }
}

#[test]
fn ttl_bounds_and_v1_records_stay_free_of_invite_fields() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::at(dir.path().into()).unwrap();
    assert!(
        storage
            .pair_with(
                "wss://example.com/v1/ws".into(),
                "x".into(),
                false,
                &dir.path().join("a.json"),
                None,
                2,
                29
            )
            .is_err()
    );
    assert!(
        storage
            .pair_with(
                "wss://example.com/v1/ws".into(),
                "x".into(),
                false,
                &dir.path().join("b.json"),
                None,
                2,
                3601
            )
            .is_err()
    );
    let v1 = storage
        .pair_with(
            "wss://example.com/v1/ws".into(),
            "x".into(),
            false,
            &dir.path().join("c.json"),
            None,
            1,
            600,
        )
        .unwrap();
    assert_eq!(v1.v, 1);
    assert!(
        v1.deep_link()
            .unwrap()
            .starts_with("riwork://pair?v=1&data=")
    );
    assert!(v1.invite_id.is_none());
    let v2 = storage
        .pair_with(
            "wss://example.com/v1/ws".into(),
            "y".into(),
            false,
            &dir.path().join("d.json"),
            None,
            2,
            30,
        )
        .unwrap();
    assert!(
        v2.deep_link()
            .unwrap()
            .starts_with("riwork://pair?v=2&data=")
    );
    assert_eq!(v2.invite_state.as_deref(), Some("pending"));
    assert!(v2.pairing_secret.is_empty());
}
