//! Pairing state: transactional `pair`, bare relative paths, bounded lock waits,
//! the authentication audit trail and HOME handling. Everything uses temp dirs.
use riwork_remote::{
    config::{Storage, private_read},
    relay::Routes,
};
use serde_json::Value;
use std::{path::Path, process::Command, time::Duration};

const RELAY: &str = "wss://example.com/v1/ws";

fn routes_len(path: &Path) -> usize {
    let r: Routes = private_read(path, 1024 * 1024).unwrap();
    r.validate().unwrap();
    r.routes.len()
}
fn remote(home: &Path, cwd: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_riwork-remote"));
    c.env("RIWORK_HOME", home).current_dir(cwd);
    c
}

#[test]
fn pair_with_bare_relative_routes_file_succeeds_and_retries_do_not_accumulate() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for (name, expected) in [("a", 1), ("b", 2)] {
        let out = remote(&home, tmp.path())
            .args(["pair", "--relay", RELAY, "--name", name])
            .args([
                "--out",
                &format!("{name}.json"),
                "--relay-routes",
                "routes.json",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(routes_len(&tmp.path().join("routes.json")), expected);
    }
    // A retry that reuses an existing export fails before touching anything.
    let again = remote(&home, tmp.path())
        .args(["pair", "--relay", RELAY, "--name", "c", "--out", "a.json"])
        .args(["--relay-routes", "routes.json"])
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert_eq!(routes_len(&tmp.path().join("routes.json")), 2);
}

#[test]
fn devices_lists_pairing_and_authentication_times() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let paired = remote(&home, tmp.path())
        .args([
            "pair", "--relay", RELAY, "--name", "phone", "--out", "p.json",
        ])
        .output()
        .unwrap();
    assert!(paired.status.success());
    let list = |home: &Path| -> Value {
        let out = remote(home, tmp.path()).arg("devices").output().unwrap();
        assert!(out.status.success());
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let before = list(&home);
    assert!(before[0]["paired_at_unix"].as_u64().unwrap() > 1_700_000_000);
    assert_eq!(before[0]["first_authenticated_unix"], Value::Null);
    assert_eq!(before[0]["last_authenticated_unix"], Value::Null);
    let storage = Storage::at(home).unwrap();
    let id = before[0]["device_id"].as_str().unwrap().to_owned();
    assert!(
        storage
            .record_authentication(&id, Duration::from_secs(1))
            .unwrap(),
        "first authentication is reported as such"
    );
    assert!(
        !storage
            .record_authentication(&id, Duration::from_secs(1))
            .unwrap()
    );
    let after = list(tmp.path().join("home").as_path());
    assert!(after[0]["first_authenticated_unix"].as_u64().is_some());
    assert!(after[0]["last_authenticated_unix"].as_u64().is_some());
    // A revoked or unknown device leaves no record and cannot be resurrected.
    storage.revoke(&id).unwrap();
    assert!(
        storage
            .record_authentication(&id, Duration::from_secs(1))
            .is_err()
    );
    assert!(
        storage
            .record_authentication(&uuid::Uuid::new_v4().to_string(), Duration::from_secs(1))
            .is_err()
    );
    assert!(storage.config().unwrap().devices[0].revoked);
}

#[test]
fn configs_written_before_the_audit_fields_still_load() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::at(tmp.path().into()).unwrap();
    storage
        .pair(
            RELAY.into(),
            "old".into(),
            false,
            &tmp.path().join("o.json"),
            None,
        )
        .unwrap();
    let path = storage.dir.join("devices.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["devices"][0]
        .as_object_mut()
        .unwrap()
        .remove("paired_at_unix")
        .unwrap();
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    let c = storage.config().unwrap();
    assert_eq!(c.devices[0].paired_at_unix, None);
}

#[test]
fn failed_pair_leaves_no_partial_state() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::at(tmp.path().into()).unwrap();
    let routes = tmp.path().join("routes.json");
    // Export cannot be created: nothing else may change, not even the routes file.
    let missing = tmp.path().join("no-such-dir/out.json");
    assert!(
        storage
            .pair(RELAY.into(), "x".into(), false, &missing, Some(&routes))
            .is_err()
    );
    assert!(!routes.exists());
    assert!(storage.config().unwrap().devices.is_empty());

    storage
        .pair(
            RELAY.into(),
            "first".into(),
            false,
            &tmp.path().join("1.json"),
            Some(&routes),
        )
        .unwrap();
    let kept = std::fs::read(&routes).unwrap();

    // The config cannot be saved (read-only storage directory): the export and the
    // route added a moment earlier are both taken back.
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::fs::PermissionsExt;
        let set = |mode| std::fs::set_permissions(&storage.dir, PermissionsExt::from_mode(mode));
        set(0o500).unwrap();
        let export = tmp.path().join("2.json");
        for _ in 0..3 {
            let failed = storage.pair(RELAY.into(), "second".into(), false, &export, Some(&routes));
            assert!(failed.is_err());
            assert!(
                !export.exists(),
                "secret export must not survive a failed pair"
            );
            assert_eq!(std::fs::read(&routes).unwrap(), kept, "no orphan route");
        }
        set(0o700).unwrap();
        storage
            .pair(RELAY.into(), "second".into(), false, &export, Some(&routes))
            .unwrap();
        assert_eq!(routes_len(&routes), 2);
        assert_eq!(storage.config().unwrap().devices.len(), 2);
    }
}

#[test]
fn revoke_waits_a_bounded_time_for_the_config_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::at(tmp.path().into()).unwrap();
    let p = storage
        .pair(
            RELAY.into(),
            "phone".into(),
            false,
            &tmp.path().join("p.json"),
            None,
        )
        .unwrap();
    let held = storage.lock("config.lock").unwrap();
    // Bounded: gives up with an error rather than blocking forever.
    let started = std::time::Instant::now();
    assert!(
        storage
            .lock_wait("config.lock", Duration::from_millis(150))
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    // A concurrent pair finishing within the bound no longer fails the revoke.
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
    });
    storage.revoke(&p.device_id).unwrap();
    release.join().unwrap();
    assert!(!storage.authorized(&p.device_id).unwrap());
}

#[test]
fn unset_home_is_an_error_not_a_path_relative_to_the_working_directory() {
    let tmp = tempfile::tempdir().unwrap();
    for home in [None, Some("")] {
        let mut c = Command::new(env!("CARGO_BIN_EXE_riwork-remote"));
        c.arg("devices")
            .current_dir(tmp.path())
            .env_remove("RIWORK_HOME")
            .env_remove("HOME");
        if let Some(h) = home {
            c.env("HOME", h);
        }
        let out = c.output().unwrap();
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("HOME is not set"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn failed_route_write_removes_the_export_and_adds_no_device() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // root ignores directory permissions
    }
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::at(tmp.path().into()).unwrap();
    let dir = tmp.path().join("relay");
    std::fs::create_dir(&dir).unwrap();
    let routes = dir.join("routes.json");
    storage
        .pair(
            RELAY.into(),
            "first".into(),
            false,
            &tmp.path().join("1.json"),
            Some(&routes),
        )
        .unwrap();
    // Readable manifest in a directory that cannot be written: the rename step fails.
    std::fs::set_permissions(&dir, PermissionsExt::from_mode(0o500)).unwrap();
    let export = tmp.path().join("2.json");
    let failed = storage.pair(RELAY.into(), "second".into(), false, &export, Some(&routes));
    std::fs::set_permissions(&dir, PermissionsExt::from_mode(0o700)).unwrap();
    assert!(failed.is_err());
    assert!(
        !export.exists(),
        "secret export must not survive a failed pair"
    );
    assert_eq!(routes_len(&routes), 1);
    assert_eq!(storage.config().unwrap().devices.len(), 1);
}
