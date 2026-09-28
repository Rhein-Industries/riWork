use super::*;
use serde_json::json;
use std::os::unix::fs::{PermissionsExt, symlink};

struct Fixture {
    root: PathBuf,
    cli: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let root = env::temp_dir().join(format!("riwork-orca-import-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let cli = root.join("orca fixture");
        fs::write(
            &cli,
            format!(
                r#"#!/usr/bin/env python3
import json, pathlib, sys
root = pathlib.Path({root:?})
if (root / 'fail').exists():
    print('private CLI diagnostics', file=sys.stderr)
    sys.exit(5)
args = sys.argv[1:]
if args == ['project', 'list', '--json']: name = 'projects'
elif args == ['project', 'setups', '--host', 'local', '--json']: name = 'setups'
elif args == ['worktree', 'list', '--limit', '10000', '--json']: name = 'worktrees'
else: sys.exit(9)
sys.stdout.write((root / (name + '.json')).read_text())
"#,
                root = root.to_string_lossy()
            ),
        )
        .unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        let store = Store::open(root.join("state")).unwrap();
        let fixture = Self { root, cli, store };
        fixture.write("projects", json!({"projects": []}));
        fixture.write("setups", json!({"setups": []}));
        fixture.write(
            "worktrees",
            json!({"worktrees": [], "totalCount": 0, "truncated": false}),
        );
        fixture
    }

    fn manager(&self) -> ImportManager {
        ImportManager {
            store: self.store.clone(),
            cli: Some(self.cli.clone()),
        }
    }

    fn directory(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write(&self, name: &str, result: Value) {
        fs::write(
            self.root.join(format!("{name}.json")),
            serde_json::to_vec(&json!({"ok": true, "result": result})).unwrap(),
        )
        .unwrap();
    }

    fn source(&self, root: &Path, worktree: &Path) {
        self.write("projects", json!({"projects": [{"id": "modern-project", "displayName": "Imported name", "kind": "git", "sourceRepoIds": ["repo"]}]}));
        self.write("setups", json!({"setups": [{"id": "setup", "projectId": "modern-project", "hostId": "local", "repoId": "repo", "path": root, "kind": "git", "setupState": "ready"}]}));
        self.write("worktrees", json!({"worktrees": [
            {"projectId": "modern-project", "projectHostSetupId": "setup", "repoId": "repo", "hostId": "local", "path": root, "branch": "refs/heads/main", "isMainWorktree": true},
            {"projectId": "modern-project", "projectHostSetupId": "setup", "repoId": "repo", "hostId": "local", "path": worktree, "branch": "refs/heads/feature", "isMainWorktree": false}
        ], "totalCount": 2, "truncated": false, "hostScope": {"hostIds": ["local"], "omittedHostIds": ["remote"]}}));
    }

    fn add_existing(&self, root: PathBuf, worktree: Option<PathBuf>) {
        self.store
            .transaction(|state| {
                state.active_project_id = Some("existing".into());
                state.project_folders.push(crate::store::ProjectFolder {
                    id: "folder".into(),
                    name: "My grouping".into(),
                    parent_id: None,
                    created_at: 7,
                });
                state.projects.push(Project {
                    id: "existing".into(),
                    name: "My edited name".into(),
                    root,
                    repository_roots: vec![],
                    folder_id: Some("folder".into()),
                    notify_on_agent_done: false,
                    codex_account: crate::store::ProjectCodexAccount::default(),
                    created_at: 8,
                });
                if let Some(path) = worktree {
                    state.worktrees.push(Worktree {
                        id: "existing-worktree".into(),
                        project_id: "existing".into(),
                        path,
                        branch: "edited branch".into(),
                        is_primary: true,
                        repository_root: None,
                        created_at: 9,
                    });
                }
                Ok(())
            })
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn modern_local_mapping_imports_atomically_and_preserves_selection() {
    let fixture = Fixture::new();
    fixture.add_existing(fixture.directory("existing"), None);
    let root = fixture.directory("imported");
    let feature = fixture.directory("feature");
    fixture.source(&root, &feature);
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    assert_eq!(
        (
            preview.project_count,
            preview.folder_count,
            preview.worktree_count
        ),
        (1, 0, 2)
    );
    let json = serde_json::to_value(&preview).unwrap();
    assert!(json.get("plan").is_none());
    assert!(json.get("source_snapshot").is_none());
    let receipt = manager.import(&preview).unwrap();
    assert_eq!((receipt.project_count, receipt.worktree_count), (1, 2));
    let state = fixture.store.snapshot().unwrap();
    assert_eq!(state.active_project_id.as_deref(), Some("existing"));
    assert_eq!(state.projects.len(), 2);
    assert_eq!(state.project_folders.len(), 1);
    let imported = state
        .projects
        .iter()
        .find(|project| project.root == root)
        .unwrap();
    assert_eq!(imported.name, "Imported name");
    assert_eq!(imported.repository_roots, [root.clone()]);
    assert!(
        state
            .worktrees
            .iter()
            .all(|tree| tree.project_id == imported.id)
    );
    assert_eq!(state.worktrees[0].branch, "feature");
    assert_eq!(state.worktrees[1].branch, "main");
    assert!(state.worktrees[1].is_primary);
    assert_eq!(
        state.orca_import.unwrap().completed_at,
        receipt.completed_at
    );
}

#[test]
fn canonical_aliases_deduplicate_without_replacing_existing_metadata() {
    let fixture = Fixture::new();
    let root = fixture.directory("repo");
    let feature = fixture.directory("feature");
    let alias = fixture.root.join("repo-alias");
    let feature_alias = fixture.root.join("feature-alias");
    symlink(&root, &alias).unwrap();
    symlink(&feature, &feature_alias).unwrap();
    fixture.add_existing(alias.clone(), Some(feature_alias));
    fixture.source(&root, &feature);
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    assert_eq!((preview.project_count, preview.worktree_count), (0, 1));
    manager.import(&preview).unwrap();
    let state = fixture.store.snapshot().unwrap();
    assert_eq!(state.projects.len(), 1);
    assert_eq!(state.projects[0].name, "My edited name");
    assert_eq!(state.projects[0].root, alias);
    assert_eq!(state.projects[0].folder_id.as_deref(), Some("folder"));
    assert_eq!(state.worktrees[0].branch, "edited branch");
    assert_eq!(state.worktrees[1].project_id, "existing");
}

#[test]
fn remote_missing_and_mismatched_ownership_are_skipped() {
    let fixture = Fixture::new();
    let root = fixture.directory("repo");
    let feature = fixture.directory("feature");
    fixture.source(&root, &feature);
    fixture.write(
        "projects",
        json!({"projects": [
            {"id": "modern-project", "displayName": "Local"},
            {"id": "missing", "displayName": "Missing"},
            {"id": "remote", "displayName": "Remote"}
        ]}),
    );
    fixture.write("setups", json!({"setups": [
        {"id": "setup", "projectId": "modern-project", "hostId": "local", "repoId": "repo", "path": root, "kind": "git", "setupState": "ready"},
        {"id": "missing", "projectId": "missing", "hostId": "local", "path": fixture.root.join("absent"), "kind": "folder", "setupState": "ready"},
        {"id": "remote", "projectId": "remote", "hostId": "runtime:remote", "repoId": "repo", "path": root, "kind": "git", "setupState": "ready"}
    ]}));
    fixture.write("worktrees", json!({"worktrees": [
        {"projectId": "modern-project", "projectHostSetupId": "setup", "repoId": "repo", "hostId": "local", "path": root, "branch": "refs/heads/main"},
        {"projectId": "remote", "projectHostSetupId": "setup", "repoId": "repo", "hostId": "local", "path": feature},
        {"projectId": "modern-project", "projectHostSetupId": "setup", "repoId": "repo", "hostId": "runtime:remote", "path": feature}
    ], "totalCount": 3, "truncated": false}));
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    assert_eq!((preview.project_count, preview.worktree_count), (1, 1));
    assert!(
        preview
            .warnings
            .iter()
            .any(|warning| warning.contains("missing local project"))
    );
    assert!(
        preview
            .warnings
            .iter()
            .any(|warning| warning.contains("remote"))
    );
    assert!(
        preview
            .warnings
            .iter()
            .any(|warning| warning.contains("mismatched"))
    );
    manager.import(&preview).unwrap();
    assert_eq!(fixture.store.snapshot().unwrap().projects.len(), 1);
    assert!(!fixture.root.join("absent").exists());
}

#[test]
fn invalid_cli_replies_and_failure_do_not_write_state() {
    for mode in ["malformed", "truncated", "incomplete", "failed", "envelope"] {
        let fixture = Fixture::new();
        let root = fixture.directory("repo");
        fixture.source(&root, &fixture.directory("feature"));
        match mode {
            "malformed" => fs::write(fixture.root.join("projects.json"), "not json").unwrap(),
            "truncated" => fixture.write(
                "worktrees",
                json!({"worktrees": [], "totalCount": 0, "truncated": true}),
            ),
            "incomplete" => fixture.write(
                "worktrees",
                json!({"worktrees": [], "totalCount": 4, "truncated": false}),
            ),
            "failed" => fs::write(fixture.root.join("fail"), "").unwrap(),
            "envelope" => fs::write(
                fixture.root.join("projects.json"),
                "{\"ok\":false,\"result\":{\"projects\":[]}}",
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert!(fixture.manager().inspect().is_err(), "{mode}");
        assert!(!fixture.root.join("state/state.json").exists(), "{mode}");
    }
}

#[test]
fn changed_source_requires_new_preview() {
    let fixture = Fixture::new();
    let root = fixture.directory("repo");
    fixture.source(&root, &fixture.directory("feature"));
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    fixture.write(
        "projects",
        json!({"projects": [{"id": "modern-project", "displayName": "Renamed"}]}),
    );
    assert!(manager.import(&preview).unwrap_err().contains("Refresh"));
    assert!(!fixture.root.join("state/state.json").exists());
}

#[test]
fn changed_destination_requires_new_preview_without_partial_additions() {
    let fixture = Fixture::new();
    let root = fixture.directory("repo");
    fixture.source(&root, &fixture.directory("feature"));
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    fixture.add_existing(fixture.directory("new-existing"), None);
    let before = fs::read(fixture.root.join("state/state.json")).unwrap();
    assert!(manager.import(&preview).unwrap_err().contains("Refresh"));
    assert_eq!(
        before,
        fs::read(fixture.root.join("state/state.json")).unwrap()
    );
    assert!(fixture.store.snapshot().unwrap().orca_import.is_none());
}

#[test]
fn completion_is_offline_and_repeated_import_does_not_rewrite_state() {
    let fixture = Fixture::new();
    let root = fixture.directory("repo");
    fixture.source(&root, &fixture.directory("feature"));
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    let first = manager.import(&preview).unwrap();
    let path = fixture.root.join("state/state.json");
    let before = fs::read(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    fs::remove_file(&fixture.cli).unwrap();
    let repeated = manager.import(&preview).unwrap();
    assert_eq!(first.completed_at, repeated.completed_at);
    assert_eq!(before, fs::read(&path).unwrap());
    assert_eq!(modified, fs::metadata(&path).unwrap().modified().unwrap());
    let completed = manager.inspect().unwrap();
    assert!(completed.already_imported.is_some());
    assert_eq!((completed.project_count, completed.worktree_count), (0, 0));
}

#[test]
fn unrelated_selection_change_is_preserved_without_invalidating_preview() {
    let fixture = Fixture::new();
    fixture.add_existing(fixture.directory("existing"), None);
    let root = fixture.directory("repo");
    fixture.source(&root, &fixture.directory("feature"));
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    fixture
        .store
        .transaction(|state| {
            state.active_project_id = None;
            Ok(())
        })
        .unwrap();
    manager.import(&preview).unwrap();
    assert_eq!(fixture.store.snapshot().unwrap().active_project_id, None);
}

#[test]
fn multiple_local_setups_keep_each_clone_and_its_primary_worktree() {
    let fixture = Fixture::new();
    let first = fixture.directory("clone-one");
    let second = fixture.directory("clone-two");
    fixture.write(
        "projects",
        json!({"projects": [{"id": "provider", "displayName": "Repo"}]}),
    );
    fixture.write("setups", json!({"setups": [
        {"id": "first", "projectId": "provider", "hostId": "local", "repoId": "repo", "path": first, "kind": "git", "setupState": "ready"},
        {"id": "second", "projectId": "provider", "hostId": "local", "repoId": "repo", "path": second, "kind": "git", "setupState": "ready"}
    ]}));
    fixture.write("worktrees", json!({"worktrees": [
        {"projectId": "provider", "projectHostSetupId": "first", "repoId": "repo", "hostId": "local", "path": first, "branch": "refs/heads/main", "isMainWorktree": true},
        {"projectId": "provider", "projectHostSetupId": "second", "repoId": "repo", "hostId": "local", "path": second, "branch": "refs/heads/main", "isMainWorktree": true}
    ], "totalCount": 2, "truncated": false}));
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    assert_eq!((preview.project_count, preview.worktree_count), (2, 2));
    manager.import(&preview).unwrap();
    let state = fixture.store.snapshot().unwrap();
    for tree in &state.worktrees {
        assert!(tree.is_primary);
        let project = state
            .projects
            .iter()
            .find(|project| project.id == tree.project_id)
            .unwrap();
        assert_eq!(tree.path, project.root);
    }
}

#[test]
fn explicit_nested_project_root_wins_and_worktree_dedup_is_per_project() {
    let fixture = Fixture::new();
    let wrapper = fixture.directory("wrapper");
    let child = fixture.directory("wrapper/child");
    let feature = fixture.directory("feature");
    fixture.add_existing(wrapper, Some(child.clone()));
    fixture
        .store
        .transaction(|state| {
            state.projects[0].repository_roots = vec![child.clone()];
            state.projects.push(Project {
                id: "child-project".into(),
                name: "Child custom name".into(),
                root: child.clone(),
                repository_roots: vec![child.clone()],
                folder_id: None,
                notify_on_agent_done: false,
                codex_account: crate::store::ProjectCodexAccount::default(),
                created_at: 11,
            });
            Ok(())
        })
        .unwrap();
    fixture.source(&child, &feature);
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    assert_eq!((preview.project_count, preview.worktree_count), (0, 2));
    manager.import(&preview).unwrap();
    let state = fixture.store.snapshot().unwrap();
    assert_eq!(state.projects[1].name, "Child custom name");
    let imported = state
        .worktrees
        .iter()
        .filter(|tree| tree.id != "existing-worktree")
        .collect::<Vec<_>>();
    assert!(
        imported
            .iter()
            .all(|tree| tree.project_id == "child-project")
    );
    assert!(
        imported
            .iter()
            .find(|tree| tree.path == child)
            .unwrap()
            .is_primary
    );
}

#[test]
fn nested_repository_without_registered_child_imports_as_its_own_project() {
    let fixture = Fixture::new();
    let wrapper = fixture.directory("wrapper");
    let child = fixture.directory("wrapper/child");
    fixture.add_existing(wrapper, Some(child.clone()));
    fixture
        .store
        .transaction(|state| {
            state.projects[0].repository_roots = vec![child.clone()];
            Ok(())
        })
        .unwrap();
    fixture.source(&child, &fixture.directory("feature"));
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    assert_eq!((preview.project_count, preview.worktree_count), (1, 2));
    manager.import(&preview).unwrap();
    assert_eq!(fixture.store.snapshot().unwrap().projects.len(), 2);
}

#[test]
fn child_holding_pipes_after_cli_exit_is_bounded() {
    let fixture = Fixture::new();
    fs::write(
        &fixture.cli,
        r#"#!/usr/bin/env python3
import os, sys, time
if os.fork() == 0:
    time.sleep(20)
    os._exit(0)
print('{"ok":true,"result":{"projects":[]}}', flush=True)
os._exit(0)
"#,
    )
    .unwrap();
    let started = Instant::now();
    assert!(
        run_json(&fixture.cli, &["project", "list", "--json"])
            .unwrap_err()
            .contains("close its output")
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn destination_symlink_retarget_requires_new_preview() {
    let fixture = Fixture::new();
    let root = fixture.directory("repo");
    let other = fixture.directory("other");
    let alias = fixture.root.join("alias");
    symlink(&root, &alias).unwrap();
    fixture.add_existing(alias.clone(), None);
    fixture.source(&root, &fixture.directory("feature"));
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    assert_eq!(preview.project_count, 0);
    fs::remove_file(&alias).unwrap();
    symlink(&other, &alias).unwrap();
    let before = fs::read(fixture.root.join("state/state.json")).unwrap();
    assert!(manager.import(&preview).unwrap_err().contains("Refresh"));
    assert_eq!(
        before,
        fs::read(fixture.root.join("state/state.json")).unwrap()
    );
}

#[test]
fn empty_warning_free_plan_never_records_a_receipt() {
    let fixture = Fixture::new();
    let manager = fixture.manager();
    // Orca may still be starting, or may simply have no projects yet.
    let preview = manager.inspect().unwrap();
    assert_eq!((preview.project_count, preview.worktree_count), (0, 0));
    // The Settings panel keys its finish button off this.
    assert!(!preview.recordable());
    assert!(
        manager
            .import(&preview)
            .unwrap_err()
            .contains("Nothing was recorded")
    );
    assert!(!fixture.root.join("state/state.json").exists());
    assert!(fixture.store.snapshot().unwrap().orca_import.is_none());
    assert!(manager.inspect().unwrap().already_imported.is_none());

    // Once Orca reports projects, the same manager can still import them.
    let root = fixture.directory("repo");
    fixture.source(&root, &fixture.directory("feature"));
    let preview = manager.inspect().unwrap();
    assert_eq!((preview.project_count, preview.worktree_count), (1, 2));
    manager.import(&preview).unwrap();
    assert!(manager.inspect().unwrap().already_imported.is_some());
}

#[test]
fn everything_already_in_riwork_does_not_record_a_receipt() {
    let fixture = Fixture::new();
    let root = fixture.directory("repo");
    let feature = fixture.directory("feature");
    fixture.add_existing(root.clone(), Some(feature.clone()));
    fixture
        .store
        .transaction(|state| {
            state.worktrees.push(Worktree {
                id: "existing-root-worktree".into(),
                project_id: "existing".into(),
                path: root.clone(),
                branch: "main".into(),
                is_primary: true,
                repository_root: None,
                created_at: 9,
            });
            Ok(())
        })
        .unwrap();
    fixture.source(&root, &feature);
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    assert_eq!((preview.project_count, preview.worktree_count), (0, 0));
    assert!(manager.import(&preview).is_err());
    assert!(fixture.store.snapshot().unwrap().orca_import.is_none());
}

#[test]
fn empty_plan_with_skipped_items_still_records_completion() {
    let fixture = Fixture::new();
    fixture.write(
        "projects",
        json!({"projects": [{"id": "gone", "displayName": "Gone"}]}),
    );
    fixture.write("setups", json!({"setups": [
        {"id": "gone", "projectId": "gone", "hostId": "local", "path": fixture.root.join("absent"), "kind": "folder", "setupState": "ready"}
    ]}));
    let manager = fixture.manager();
    let preview = manager.inspect().unwrap();
    assert_eq!((preview.project_count, preview.worktree_count), (0, 0));
    assert!(
        preview
            .warnings
            .iter()
            .any(|warning| warning.contains("missing local project"))
    );
    assert!(preview.recordable());
    let receipt = manager.import(&preview).unwrap();
    assert_eq!((receipt.project_count, receipt.worktree_count), (0, 0));
    assert!(manager.inspect().unwrap().already_imported.is_some());
}

struct CliLayout {
    root: PathBuf,
    bundle: PathBuf,
    path_dir: PathBuf,
    fallback: PathBuf,
}

impl CliLayout {
    fn new(fixture: &Fixture) -> Self {
        Self {
            root: fixture.root.clone(),
            bundle: fixture.root.join("Orca.app/bin/orca"),
            path_dir: fixture.root.join("path-bin"),
            fallback: fixture.root.join("fallback-bin"),
        }
    }

    fn install(&self, path: &Path) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        path.canonicalize().unwrap()
    }

    fn find(&self, override_command: Option<&OsStr>, development: bool) -> Result<PathBuf, String> {
        find_cli(
            override_command.map(OsStr::to_os_string),
            development,
            Some(&self.bundle),
            Some(env::join_paths([&self.path_dir]).unwrap()),
            std::slice::from_ref(&self.fallback),
        )
    }
}

#[test]
fn cli_lookup_prefers_the_app_bundle_and_keeps_the_same_fallbacks() {
    let fixture = Fixture::new();
    let layout = CliLayout::new(&fixture);
    assert!(layout.find(None, false).unwrap_err().contains("not found"));

    let fallback = layout.install(&layout.fallback.join("orca"));
    assert_eq!(layout.find(None, false).unwrap(), fallback);
    let on_path = layout.install(&layout.path_dir.join("orca"));
    assert_eq!(layout.find(None, false).unwrap(), on_path);
    // Import and account discovery agree on Orca.app even without a symlink.
    let bundle = layout.install(&layout.bundle);
    assert_eq!(layout.find(None, false).unwrap(), bundle);

    // An explicit override still wins, and an empty one is rejected.
    assert_eq!(
        layout.find(Some(on_path.as_os_str()), false).unwrap(),
        on_path
    );
    assert!(
        layout
            .find(Some(OsStr::new("")), false)
            .unwrap_err()
            .contains("empty")
    );
    assert!(
        layout
            .find(Some(layout.root.join("absent").as_os_str()), false)
            .is_err()
    );
}

#[test]
fn development_checkouts_use_orca_dev_instead_of_the_installed_bundle() {
    let fixture = Fixture::new();
    let layout = CliLayout::new(&fixture);
    layout.install(&layout.bundle);
    assert!(layout.find(None, true).unwrap_err().contains("orca-dev"));
    let dev = layout.install(&layout.path_dir.join("orca-dev"));
    assert_eq!(layout.find(None, true).unwrap(), dev);
}

#[test]
fn cli_lookup_skips_files_that_are_not_executable() {
    let fixture = Fixture::new();
    let layout = CliLayout::new(&fixture);
    let plain = layout.install(&layout.bundle);
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(layout.find(None, false).is_err());
}
