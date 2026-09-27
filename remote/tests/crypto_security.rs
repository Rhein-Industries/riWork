use riwork_remote::crypto::*;
use serde_json::Value;
fn fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/v1.json")).unwrap()
}
fn setup() -> (Identity, [u8; 32], [u8; 32], [u8; 32]) {
    let f = fixture();
    (
        Identity {
            desktop_id: f["desktop_id"].as_str().unwrap().into(),
            device_id: f["device_id"].as_str().unwrap().into(),
            route_id: f["route_id"].as_str().unwrap().into(),
        },
        decode(f["pairing_secret"].as_str().unwrap()).unwrap(),
        decode(f["client_nonce"].as_str().unwrap()).unwrap(),
        decode(f["desktop_nonce"].as_str().unwrap()).unwrap(),
    )
}
fn sessions() -> (Session, Session) {
    let (id, key, c, d) = setup();
    let h = client_hello(&id, &key, c).unwrap();
    let (s, p) = accept_hello(&id, &key, &h, d).unwrap();
    let (f, m) = accept_server(&id, &key, &c, &s).unwrap();
    (m, p.finish(&f).unwrap())
}
#[test]
fn independent_fixture_proofs_keys_and_ciphertexts_match() {
    let f = fixture();
    let (id, key, c, d) = setup();
    assert_eq!(hex::encode(id.bytes().unwrap()), f["identity_hex"]);
    let hello = client_hello(&id, &key, c).unwrap();
    assert_eq!(serde_json::to_value(&hello).unwrap(), f["client_hello"]);
    let (server, pending) = accept_hello(&id, &key, &hello, d).unwrap();
    assert_eq!(serde_json::to_value(&server).unwrap(), f["server_hello"]);
    let (finish, mut mobile) = accept_server(&id, &key, &c, &server).unwrap();
    assert_eq!(serde_json::to_value(&finish).unwrap(), f["client_finish"]);
    let mut desktop = pending.finish(&finish).unwrap();
    assert_eq!(hex::encode(mobile.c2d), f["c2d_key_hex"]);
    assert_eq!(hex::encode(mobile.d2c), f["d2c_key_hex"]);
    assert_eq!(b64(&mobile.id), f["session_id"]);
    for (i, dir) in ["c2d", "d2c"].iter().enumerate() {
        let expected: Envelope =
            serde_json::from_value(f["frames"][i]["envelope"].clone()).unwrap();
        let plain = f["frames"][i]["plaintext_utf8"]
            .as_str()
            .unwrap()
            .as_bytes();
        let sealed = mobile.seal(dir, plain).unwrap();
        assert_eq!(
            serde_json::to_value(&sealed).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(desktop.open(dir, &expected).unwrap(), plain);
    }
}
#[test]
fn tamper_replay_gap_direction_and_old_session_are_rejected() {
    let (mut sender, mut receiver) = sessions();
    let e = sender.seal("c2d", b"hello").unwrap();
    let mut bad = e.clone();
    bad.ciphertext.replace_range(
        0..1,
        if e.ciphertext.starts_with('A') {
            "B"
        } else {
            "A"
        },
    );
    assert!(receiver.open("c2d", &bad).is_err());
    let mut gap = e.clone();
    gap.counter = "1".into();
    assert!(receiver.open("c2d", &gap).is_err());
    let mut leading = e.clone();
    leading.counter = "00".into();
    assert!(receiver.open("c2d", &leading).is_err());
    assert!(receiver.open("d2c", &e).is_err());
    assert_eq!(receiver.open("c2d", &e).unwrap(), b"hello");
    assert!(receiver.open("c2d", &e).is_err());
    let (id, key, c, _) = setup();
    let hello = client_hello(&id, &key, c).unwrap();
    let (server, pending) = accept_hello(&id, &key, &hello, random32()).unwrap();
    let (finish, m) = accept_server(&id, &key, &c, &server).unwrap();
    let mut fresh = pending.finish(&finish).unwrap();
    assert_ne!(m.id, receiver.id);
    assert_ne!(m.c2d, receiver.c2d);
    assert!(fresh.open("c2d", &e).is_err());
}
#[test]
fn wrong_device_secret_and_forged_finish_are_rejected() {
    let (id, key, c, d) = setup();
    let hello = client_hello(&id, &key, c).unwrap();
    let mut other = id.clone();
    other.device_id = uuid::Uuid::new_v4().to_string();
    assert!(accept_hello(&other, &key, &hello, d).is_err());
    assert!(accept_hello(&id, &random32(), &hello, d).is_err());
    let (server, pending) = accept_hello(&id, &key, &hello, d).unwrap();
    assert!(accept_server(&id, &random32(), &c, &server).is_err());
    assert!(
        pending
            .finish(&ClientFinish {
                v: 1,
                kind: "client_finish".into(),
                mac: b64(&[0; 32])
            })
            .is_err()
    );
}
#[test]
fn plaintext_and_encoding_limits_are_enforced() {
    let (mut sender, mut receiver) = sessions();
    assert!(
        sender
            .seal("c2d", &vec![0; riwork_remote::MAX_PLAINTEXT + 1])
            .is_err()
    );
    let e = sender.seal("c2d", b"ok").unwrap();
    let mut bad = e.clone();
    bad.ciphertext.push('=');
    assert!(receiver.open("c2d", &bad).is_err());
    assert_eq!(receiver.open("c2d", &e).unwrap(), b"ok");
}
