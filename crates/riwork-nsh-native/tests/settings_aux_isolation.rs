//! Exercise the ordinary debug dependency (not cfg(test)) in a private process.
//! The engine dev-dependency also refuses Keychain as a final test safety net.
#![cfg(all(debug_assertions, not(feature = "test-support")))]
use riwork_nsh_native::settings_aux::{ApiKey, OrcaImport};

#[test]
fn plain_debug_settings_actions_keep_temporary_homes_off_live_services() {
    const CHILD: &str = "RIWORK_NSH_AUX_FIXTURE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert_eq!(ApiKey::status().await.unwrap().saved, Some(false));
            assert_eq!(
                ApiKey::save("fixture-memory-value".into())
                    .await
                    .unwrap()
                    .saved,
                Some(true)
            );
            assert!(
                ApiKey::save("invalid paste with spaces".into())
                    .await
                    .is_err()
            );
            assert_eq!(ApiKey::refresh().await.unwrap().saved, Some(true));
            assert_eq!(ApiKey::remove().await.unwrap().saved, Some(false));
            let preview = OrcaImport::preview().await.unwrap();
            assert!(!preview.recordable);
            assert!(preview.source.contains("Fixture"));
            assert!(
                OrcaImport::run(preview.id)
                    .await
                    .unwrap_err()
                    .starts_with("refused:")
            );
        });
        return;
    }
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let os_home = root.join("os-home");
    std::fs::create_dir(&os_home).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "plain_debug_settings_actions_keep_temporary_homes_off_live_services",
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD, "1")
        .env("HOME", &os_home)
        .env("RIWORK_HOME", root.join("state"))
        .env("TMPDIR", &root)
        .env("ORCA_CLI_COMMAND", root.join("nonexistent-fixture-orca"))
        .env("PATH", root.join("empty-path"))
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!root.join("state/settings.json").exists());
    assert!(!root.join("state/state.json").exists());
    assert!(!os_home.join(".codex/auth.json").exists());
}
