use anyhow::Result;
use riwork_core::{
    settings::{Settings, SettingsStore},
    theme::ThemeChoice,
    ui_text::{SizeChange, TextPoints},
};
use riwork_nsh_native::{
    ProcessService,
    backend::{CoreStoreBackend, SessionsBackend},
    dto::SessionsSnapshot,
    settings::{CoreSettingsBackend, PreferenceService, SettingsBackend, SettingsPatch},
};
use std::{
    path::Path,
    sync::{Arc, Barrier, Mutex},
    time::Duration,
};

#[derive(Default)]
struct EmptySessions;
impl SessionsBackend for EmptySessions {
    fn list(&self) -> Result<SessionsSnapshot> {
        Ok(SessionsSnapshot::default())
    }
}

async fn service(home: &Path) -> Arc<ProcessService> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        ProcessService::with_backends(
            home.clone(),
            Arc::new(CoreStoreBackend::new(home)),
            Arc::new(EmptySessions),
        )
    })
    .await
    .unwrap()
    .unwrap()
}

#[test]
fn external_settings_writer_fixture() {
    // A genuine second process writes only the path explicitly supplied by the
    // parent test. No default-home resolution or process env mutation occurs.
    let Some(home) = std::env::var_os("RIWORK_NSH_SETTINGS_CHILD_FIXTURE") else {
        return;
    };
    SettingsStore::open(std::path::PathBuf::from(home))
        .unwrap()
        .set_value("native_terminal_colors", serde_json::json!(false))
        .unwrap();
}

#[tokio::test]
async fn a_write_by_another_process_reaches_every_subscriber_once() {
    let isolated = tempfile::tempdir().unwrap();
    let settings_path = isolated.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"theme":"ri_work","future_key":{"kept":true}}"#,
    )
    .unwrap();
    let process = service(isolated.path()).await;
    let a = process.preferences().subscribe_settings();
    let b = process.preferences().subscribe_settings();
    let appearances = process.preferences().subscribe_appearance();
    assert_eq!(a.recv().await.unwrap().rev, 1);
    assert_eq!(b.recv().await.unwrap().rev, 1);
    assert_eq!(appearances.recv().await.unwrap().rev, 1);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "external_settings_writer_fixture", "--nocapture"])
        .env("RIWORK_NSH_SETTINGS_CHILD_FIXTURE", isolated.path())
        .status()
        .unwrap();
    assert!(status.success());
    let changed = tokio::time::timeout(Duration::from_secs(5), a.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(changed.rev, 2);
    assert_eq!(changed.data.theme, "native");
    assert!(!changed.data.native_terminal_colors);
    assert_eq!(b.recv().await.unwrap(), changed);
    let appearance = appearances.recv().await.unwrap();
    assert_eq!(appearance.rev, 2);
    assert_eq!(appearance.data.selected, "native");
    assert!(appearance.data.terminal_override.is_none());
    // The removed theme stays in the file as written.
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
    assert_eq!(file["theme"], "ri_work");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), a.recv())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), b.recv())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), appearances.recv())
            .await
            .is_err()
    );
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(settings_path).unwrap()).unwrap();
    assert_eq!(stored["future_key"]["kept"], true);
    assert!(!isolated.path().join("appearance.json").exists());
}

#[test]
fn change_text_size_is_a_read_modify_write_under_the_lock() {
    let isolated = tempfile::tempdir().unwrap();
    let initial = SettingsStore::open(isolated.path()).unwrap();
    initial
        .update(|settings| settings.ui_text_size = TextPoints::new(11.0))
        .unwrap();
    let a = PreferenceService::new(isolated.path().to_owned()).unwrap();
    let b = PreferenceService::new(isolated.path().to_owned()).unwrap();
    let ready = Arc::new(Barrier::new(3));
    let handles = [a.clone(), b.clone()]
        .into_iter()
        .map(|service| {
            let ready = ready.clone();
            std::thread::spawn(move || {
                ready.wait();
                for _ in 0..5 {
                    service.change_text_size("bigger").unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    ready.wait();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(initial.load().unwrap().ui_text_size.points(), 21.0);
    // The second handle still has an old cached size; the store lock chooses
    // the persisted size, and both fractional snapping and matching are core.
    initial
        .update(|settings| settings.ui_text_size = TextPoints::new(15.5))
        .unwrap();
    assert_eq!(a.change_text_size("smaller").unwrap().ui_text_size, 15.0);
    initial
        .update(|settings| settings.ui_text_matches_terminal = true)
        .unwrap();
    let saved = b.change_text_size("bigger").unwrap();
    assert_eq!(saved.ui_text_size, 14.0);
    assert!(!saved.ui_text_matches_terminal);
    assert_eq!(a.change_text_size("reset").unwrap().ui_text_size, 11.0);
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(isolated.path().join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(saved["ui_text_size"], 11);
    assert!(
        a.change_text_size("grow")
            .unwrap_err()
            .starts_with("invalid:")
    );
    std::fs::write(
        isolated.path().join("settings.json"),
        r#"{"ui_text_size":"future-size","ui_text_matches_terminal":"future-mode"}"#,
    )
    .unwrap();
    assert_eq!(a.change_text_size("reset").unwrap().ui_text_size, 11.0);
    let reset: serde_json::Value =
        serde_json::from_slice(&std::fs::read(isolated.path().join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(reset["ui_text_size"], 11);
    assert_eq!(reset["ui_text_matches_terminal"], false);
}

#[test]
fn typed_patch_preserves_unknown_settings_and_validates_before_writing() {
    let isolated = tempfile::tempdir().unwrap();
    let path = isolated.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"theme":"future_theme","future_key":{"x":1},"native_terminal_colors":false}"#,
    )
    .unwrap();
    let backend = CoreSettingsBackend::new(isolated.path().to_owned()).unwrap();
    backend
        .set(SettingsPatch {
            ui_text_size: Some(12.34),
            ..SettingsPatch::default()
        })
        .unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["theme"], "future_theme");
    assert_eq!(saved["future_key"]["x"], 1);
    assert_eq!(saved["ui_text_size"], 12.3);
    let before = std::fs::read(&path).unwrap();
    assert!(
        backend
            .set(SettingsPatch {
                theme: Some("unknown".into()),
                ..SettingsPatch::default()
            })
            .unwrap_err()
            .starts_with("invalid:")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        backend
            .set(SettingsPatch {
                interface_font: Some("unknown".into()),
                ..SettingsPatch::default()
            })
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        serde_json::from_value::<SettingsPatch>(serde_json::json!({"futureSetting":true})).is_err()
    );
    // Explicitly choosing the default replaces a future value even though the
    // lenient reader currently exposes the same default to this build.
    backend
        .set(SettingsPatch {
            theme: Some("native".into()),
            ..SettingsPatch::default()
        })
        .unwrap();
    let selected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(selected["theme"], "native");
    assert_eq!(selected["future_key"]["x"], 1);
    let service = PreferenceService::new(isolated.path().to_owned()).unwrap();
    let saved = service
        .set(SettingsPatch {
            theme: Some("native".into()),
            ..SettingsPatch::default()
        })
        .unwrap();
    assert_eq!(saved.text.face, "system_mono");
    assert_eq!(saved.text.points, 14.3);
    assert_eq!(
        service.appearance(Some("native"), true).unwrap().palette.bg,
        "#000000"
    );
    // Resolving for a window does not publish that window's dark mode globally.
    assert_eq!(
        service.appearance(None, false).unwrap().palette.bg,
        "#ffffff"
    );
}

struct FakeSettings {
    value: Mutex<Result<Settings, String>>,
}
impl SettingsBackend for FakeSettings {
    fn get(&self) -> Result<Settings, String> {
        self.value.lock().unwrap().clone()
    }
    fn set(&self, _: SettingsPatch) -> Result<Settings, String> {
        self.get()
    }
    fn change_text_size(&self, _: SizeChange) -> Result<Settings, String> {
        self.get()
    }
}

#[tokio::test]
async fn load_errors_keep_a_diagnostic_and_recover_from_a_subscription_invalidation() {
    let fake = Arc::new(FakeSettings {
        value: Mutex::new(Err("io: isolated fixture denied".into())),
    });
    let service = PreferenceService::with_backend(fake.clone());
    assert_eq!(service.get().theme, "ghostty");
    assert!(
        service
            .get()
            .error
            .unwrap()
            .contains("isolated fixture denied")
    );
    let listener = service.subscribe_settings();
    assert_eq!(listener.recv().await.unwrap().rev, 1);
    *fake.value.lock().unwrap() = Ok(Settings {
        theme: ThemeChoice::Native,
        ..Settings::default()
    });
    service.refresh();
    let recovered = listener.recv().await.unwrap();
    assert_eq!(recovered.rev, 2);
    assert_eq!(recovered.data.theme, "native");
    assert!(recovered.data.error.is_none());
    service.refresh();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.recv())
            .await
            .is_err()
    );
}

#[test]
fn sentence_case_keeps_proper_nouns_and_acronyms() {
    use riwork_nsh_native::settings::Settings as NativeSettings;
    assert_eq!(
        NativeSettings::sentence_case("OPEN CODEX / CPU AND RAM".into()),
        "Open Codex / CPU and RAM"
    );
}

#[test]
fn status_edits_read_under_the_lock_and_keep_other_windows_choices() {
    use riwork_core::status_bar::{StatusItemKind as Kind, StatusSide};
    let isolated = tempfile::tempdir().unwrap();
    let store = SettingsStore::open(isolated.path()).unwrap();
    let a = PreferenceService::new(isolated.path().to_owned()).unwrap();
    let b = PreferenceService::new(isolated.path().to_owned()).unwrap();
    // Both handles still hold the original bar. Independent simultaneous edits
    // must merge into the current saved bar rather than overwriting that cache.
    let ready = Arc::new(Barrier::new(3));
    let first = {
        let (a, ready) = (a.clone(), ready.clone());
        std::thread::spawn(move || {
            ready.wait();
            a.set_side("resources", "left").unwrap();
        })
    };
    let second = {
        let (b, ready) = (b.clone(), ready.clone());
        std::thread::spawn(move || {
            ready.wait();
            b.set_visible("project", false).unwrap();
        })
    };
    ready.wait();
    first.join().unwrap();
    second.join().unwrap();
    let bar = store.load().unwrap().status_bar;
    assert_eq!(
        bar.items
            .iter()
            .find(|item| item.kind == Kind::Resources)
            .unwrap()
            .side,
        StatusSide::Left
    );
    assert!(
        !bar.items
            .iter()
            .find(|item| item.kind == Kind::Project)
            .unwrap()
            .enabled
    );
    // Reordering uses the core's same-side neighbor rule and preserves the
    // opposite side exactly, even though this service has a stale copy.
    let right_before = bar
        .items
        .iter()
        .filter(|item| item.side == StatusSide::Right)
        .map(|item| item.kind)
        .collect::<Vec<_>>();
    a.move_item("resources", true).unwrap();
    let moved = store.load().unwrap().status_bar;
    let right_after = moved
        .items
        .iter()
        .filter(|item| item.side == StatusSide::Right)
        .map(|item| item.kind)
        .collect::<Vec<_>>();
    assert_eq!(right_before, right_after);
    b.change_status(riwork_nsh_native::settings::StatusChange::Enabled(false))
        .unwrap();
    let disabled = store.load().unwrap().status_bar;
    assert!(!disabled.enabled);
    assert_eq!(disabled.items, moved.items);
    let before = std::fs::read(isolated.path().join("settings.json")).unwrap();
    assert!(
        a.set_side("resources", "middle")
            .unwrap_err()
            .starts_with("invalid:")
    );
    assert!(
        a.set_visible("unknown", true)
            .unwrap_err()
            .starts_with("invalid:")
    );
    assert_eq!(
        std::fs::read(isolated.path().join("settings.json")).unwrap(),
        before
    );
}
