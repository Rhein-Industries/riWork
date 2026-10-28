use super::*;

#[test]
fn remote_prepare_and_restore_keep_key_without_local_registration() {
    let home = std::env::temp_dir().join(format!("riwork-remote-host-{}", Uuid::new_v4()));
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("state.json"), b"unreadable local state").unwrap();
    let key = riwork_engine::remote_tree::project_key("paired-mac", "host-project");
    let mut options = OpenOptions::project(None);
    options.project_id = Some(key.clone());
    let prepared = prepare(&home, options).unwrap();
    assert_eq!(prepared.runtime.project_id.as_deref(), Some(key.as_str()));
    assert_eq!(prepared.runtime.path, home);
    let restored = restored_window(&home, &prepared.runtime);
    assert_eq!(restored.options.project_id.as_deref(), Some(key.as_str()));
    assert!(restored.options.path.is_none());
    assert_eq!(restored.params("remote-window", 0)["projectId"], key);
    assert_eq!(
        std::fs::read(home.join("state.json")).unwrap(),
        b"unreadable local state"
    );
    assert!(!home.join("sessions.json").exists());
    assert!(!home.join("project-tabs").exists());
    let mut invalid = OpenOptions::project(None);
    invalid.project_id = Some("remote:host:".into());
    assert_eq!(
        prepare(&home, invalid).err().unwrap(),
        "invalid: remote project key"
    );
    std::fs::remove_dir_all(home).unwrap();
}
