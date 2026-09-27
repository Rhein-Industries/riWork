use riwork_remote::{
    config::{Storage, private_read, validate_url},
    crypto::decode,
    relay::{Relay, Routes},
};
#[test]
fn pairing_is_random_private_separate_from_relay_secrets_and_revocable() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::at(dir.path().into()).unwrap();
    let routes = dir.path().join("routes.json");
    let a = storage
        .pair(
            "wss://example.com/v1/ws".into(),
            "phone A".into(),
            false,
            &dir.path().join("a.json"),
            Some(&routes),
        )
        .unwrap();
    let b = storage
        .pair(
            "wss://example.com/v1/ws".into(),
            "phone B".into(),
            false,
            &dir.path().join("b.json"),
            Some(&routes),
        )
        .unwrap();
    assert_ne!(a.pairing_secret, b.pairing_secret);
    assert_ne!(a.relay_token, b.relay_token);
    assert_ne!(a.route_id, b.route_id);
    assert_eq!(a.desktop_id, b.desktop_id);
    decode::<32>(&a.pairing_secret).unwrap();
    let raw = std::fs::read_to_string(&routes).unwrap();
    assert!(!raw.contains(&a.pairing_secret) && !raw.contains(&a.relay_token));
    let r: Routes = private_read(&routes, 1024 * 1024).unwrap();
    r.validate().unwrap();
    assert_eq!(r.routes.len(), 2);
    let link = a.deep_link().unwrap();
    assert!(link.starts_with("riwork://pair?v=1&data="));
    assert!(
        !link
            .strip_prefix("riwork://pair?v=1&data=")
            .unwrap()
            .contains('=')
    );
}
#[test]
fn transport_urls_and_config_fail_closed() {
    assert!(validate_url("ws://example.com/v1/ws", true).is_err());
    assert!(validate_url("ws://127.0.0.1:8787/v1/ws", false).is_err());
    assert!(validate_url("ws://127.0.0.1:8787/v1/ws", true).is_ok());
    assert!(validate_url("ws://[::1]:8787/v1/ws", true).is_ok());
    assert!(validate_url("wss://user:pass@example.com/v1/ws", false).is_err());
    assert!(validate_url("wss://example.com/v1/ws?token=bad", false).is_err());
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::at(dir.path().into()).unwrap();
    let p = storage
        .pair(
            "wss://example.com/v1/ws".into(),
            "phone".into(),
            false,
            &dir.path().join("p.json"),
            None,
        )
        .unwrap();
    assert!(storage.authorized(&p.device_id).unwrap());
    storage.revoke(&p.device_id).unwrap();
    assert!(!storage.authorized(&p.device_id).unwrap());
    assert!(storage.revoke(&uuid::Uuid::new_v4().to_string()).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let cfg = storage.dir.join("devices.json");
        assert_eq!(
            std::fs::metadata(&storage.dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&cfg).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(storage.config().is_err());
    }
}
#[test]
fn invalid_route_and_connection_limits_are_rejected() {
    assert!(
        Relay::new(
            Routes {
                v: 1,
                routes: vec![]
            },
            0
        )
        .is_err()
    );
    assert!(
        Relay::new(
            Routes {
                v: 1,
                routes: vec![]
            },
            257
        )
        .is_err()
    );
    assert!(
        Relay::new(
            Routes {
                v: 2,
                routes: vec![]
            },
            16
        )
        .is_err()
    );
}
