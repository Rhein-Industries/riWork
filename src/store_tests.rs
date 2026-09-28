use super::*;
use std::{ffi::OsStr, process::Output};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = env::temp_dir().join(format!("riwork-store-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self {
            root: root.canonicalize().unwrap(),
        }
    }

    fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    fn directory(&self, relative: impl AsRef<Path>) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn store(&self) -> Store {
        Store::open(self.path("state")).unwrap()
    }

    fn repository(&self, relative: impl AsRef<Path>, committed: bool) -> PathBuf {
        let root = self.directory(relative);
        git(&root, ["init", "--initial-branch=main", "--template="]);
        // Store launches Git independently, so disable hooks in this fixture's
        // local repository config as well as in the helper's command options.
        git(&root, ["config", "--local", "core.hooksPath", "/dev/null"]);
        if committed {
            git(
                &root,
                ["commit", "--allow-empty", "--no-gpg-sign", "-m", "fixture"],
            );
        }
        root
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The directory was created for this fixture and contains no user files.
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn git<I, S>(root: &Path, arguments: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = git_command(root);
    command
        .args(["-c", "user.name=RiWork Tests"])
        .args(["-c", "user.email=riwork-tests@example.invalid"])
        .args(["-c", "commit.gpgsign=false"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(arguments);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "Git fixture command failed: {command:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn common_directory(root: &Path) -> PathBuf {
    let output = git(
        root,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    output_path(&output.stdout).canonicalize().unwrap()
}

fn project_worktree(store: &Store, project_id: &str, path: &Path) -> Worktree {
    store
        .snapshot()
        .unwrap()
        .worktrees
        .into_iter()
        .find(|worktree| worktree.project_id == project_id && worktree.path == path)
        .unwrap_or_else(|| panic!("Missing project worktree: {}", path.display()))
}

#[test]
fn virtual_folders_extend_schema_one_without_requiring_a_migration() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let root = fixture.directory("legacy-project");
    let project_id = Uuid::new_v4().to_string();
    let legacy = serde_json::json!({
        "schema_version": 1,
        "active_project_id": project_id,
        "projects": [{"id": project_id, "name": "Legacy", "root": root, "created_at": 7}],
        "worktrees": [],
        "tasks": [],
    });
    fs::write(
        store.dir.join("state.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();

    let state = store.snapshot().unwrap();
    assert!(state.project_folders.is_empty());
    assert_eq!(state.projects[0].folder_id, None);
    assert!(!state.projects[0].notify_on_agent_done);
    assert_eq!(
        state.projects[0].codex_account,
        ProjectCodexAccount::Inherit
    );
    assert!(state.projects[0].repository_roots.is_empty());
    let folder = store.create_project_folder("  Personal  ").unwrap();
    store
        .update_project_metadata(&project_id, "Legacy", Some(&folder.id))
        .unwrap();

    let reopened = Store::open(fixture.path("state"))
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(reopened.schema_version, 1);
    assert_eq!(
        reopened.active_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(reopened.project_folders[0].name, "Personal");
    assert_eq!(
        reopened.projects[0].folder_id.as_deref(),
        Some(folder.id.as_str())
    );
    assert_eq!(reopened.projects[0].root, root);
    assert!(!reopened.projects[0].notify_on_agent_done);
    assert_eq!(
        reopened.projects[0].codex_account,
        ProjectCodexAccount::Inherit
    );
}

#[test]
fn project_account_choice_round_trips_without_changing_other_projects_or_metadata() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let alpha = store
        .add_project(fixture.directory("alpha"), Some("Alpha"))
        .unwrap();
    let beta = store
        .add_project(fixture.directory("beta"), Some("Beta"))
        .unwrap();
    let folder = store.create_project_folder("Work").unwrap();
    store
        .update_project_metadata(&alpha.id, "Renamed", Some(&folder.id))
        .unwrap();
    let changed = store
        .set_project_codex_account(&alpha.id, ProjectCodexAccount::SystemDefault)
        .unwrap();
    assert_eq!(changed.name, "Renamed");
    assert_eq!(changed.folder_id.as_deref(), Some(folder.id.as_str()));
    let state = Store::open(fixture.path("state"))
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(
        state.project(&alpha.id).unwrap().codex_account,
        ProjectCodexAccount::SystemDefault
    );
    assert_eq!(
        state.project(&beta.id).unwrap().codex_account,
        ProjectCodexAccount::Inherit
    );
    assert!(
        store
            .set_project_codex_account(&alpha.id, ProjectCodexAccount::Saved("missing".into()))
            .is_err()
    );
    assert_eq!(
        store
            .snapshot()
            .unwrap()
            .project(&alpha.id)
            .unwrap()
            .codex_account,
        ProjectCodexAccount::SystemDefault
    );
    store
        .set_project_codex_account(&alpha.id, ProjectCodexAccount::Inherit)
        .unwrap();
    assert_eq!(
        store
            .snapshot()
            .unwrap()
            .project(&alpha.id)
            .unwrap()
            .codex_account,
        ProjectCodexAccount::Inherit
    );
}

#[test]
fn completion_notifications_are_project_scoped_and_preserve_workspace_metadata() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let main = store
        .add_project(fixture.directory("main"), Some("Main"))
        .unwrap();
    let other = store
        .add_project(fixture.directory("other"), Some("Other"))
        .unwrap();
    assert!(!main.notify_on_agent_done);
    assert!(!other.notify_on_agent_done);
    let folder = store.create_project_folder("Work").unwrap();
    let main = store
        .update_project_metadata(&main.id, "Main", Some(&folder.id))
        .unwrap();
    store
        .add_task(&main.id, "Keep this task", "Details")
        .unwrap();
    let before = store.snapshot().unwrap();
    let enabled = store.set_project_notifications("Main", true).unwrap();
    assert!(enabled.notify_on_agent_done);
    assert_eq!(enabled.id, main.id);
    assert_eq!(enabled.root, main.root);
    assert_eq!(enabled.folder_id, main.folder_id);
    assert_eq!(enabled.created_at, main.created_at);
    let reopened = fixture.store().snapshot().unwrap();
    assert!(reopened.project(&main.id).unwrap().notify_on_agent_done);
    assert!(!reopened.project(&other.id).unwrap().notify_on_agent_done);
    assert_eq!(reopened.active_project_id, before.active_project_id);
    assert_eq!(
        serde_json::to_value(&reopened.worktrees).unwrap(),
        serde_json::to_value(&before.worktrees).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&reopened.tasks).unwrap(),
        serde_json::to_value(&before.tasks).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&reopened.project_folders).unwrap(),
        serde_json::to_value(&before.project_folders).unwrap()
    );

    // Existing names, folders, and re-registration cannot reset the preference.
    let renamed = store
        .update_project_metadata(&main.id, "Renamed", None)
        .unwrap();
    assert!(renamed.notify_on_agent_done);
    assert!(
        store
            .add_project(&main.root, None)
            .unwrap()
            .notify_on_agent_done
    );
    let state_path = store.dir.join("state.json");
    let unchanged = fs::read(&state_path).unwrap();
    assert!(
        store
            .set_project_notifications(&main.id, true)
            .unwrap()
            .notify_on_agent_done
    );
    assert_eq!(fs::read(&state_path).unwrap(), unchanged);
    assert!(store.set_project_notifications("missing", true).is_err());
    assert_eq!(fs::read(&state_path).unwrap(), unchanged);
    assert!(
        !store
            .set_project_notifications(&main.id, false)
            .unwrap()
            .notify_on_agent_done
    );
}

#[test]
fn concurrent_notification_and_display_updates_preserve_each_other() {
    use std::sync::{Arc, Barrier};
    use std::thread;
    let fixture = Fixture::new();
    let store = fixture.store();
    let project = store
        .add_project(fixture.directory("project"), Some("Original"))
        .unwrap();
    let folder = store.create_project_folder("Work").unwrap();
    let start = Arc::new(Barrier::new(2));
    let notify_store = store.clone();
    let notify_id = project.id.clone();
    let notify_start = start.clone();
    let notifications = thread::spawn(move || {
        notify_start.wait();
        for _ in 0..32 {
            notify_store
                .set_project_notifications(&notify_id, true)
                .unwrap();
        }
    });
    let metadata_store = store.clone();
    let metadata_id = project.id.clone();
    let folder_id = folder.id.clone();
    let metadata = thread::spawn(move || {
        start.wait();
        for index in 0..32 {
            metadata_store
                .update_project_metadata(
                    &metadata_id,
                    &format!("Renamed {index}"),
                    Some(&folder_id),
                )
                .unwrap();
        }
    });
    notifications.join().unwrap();
    metadata.join().unwrap();
    let reopened = fixture.store().snapshot().unwrap();
    let saved = reopened.project(&project.id).unwrap();
    assert!(saved.notify_on_agent_done);
    assert_eq!(saved.name, "Renamed 31");
    assert_eq!(saved.folder_id.as_deref(), Some(folder.id.as_str()));
    assert_eq!(saved.root, project.root);
    assert_eq!(saved.created_at, project.created_at);
}

#[test]
fn virtual_folder_names_and_assignments_validate_before_persisting() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let root = fixture.directory("project");
    let project = store.add_project(&root, Some("Original")).unwrap();
    let folder = store.create_project_folder("  Äpp Projects  ").unwrap();
    let other = store.create_project_folder("Work").unwrap();
    let state = store.snapshot().unwrap();
    assert_eq!(state.project_folder("äpp projects").unwrap().id, folder.id);
    assert_eq!(state.project_folder(&folder.id[..8]).unwrap().id, folder.id);
    assert!(state.project_folder("missing").is_err());
    assert!(state.project_folder(&folder.id[..7]).is_err());

    let before = fs::read(store.dir.join("state.json")).unwrap();
    for invalid in ["", " \n "] {
        assert!(store.create_project_folder(invalid).is_err());
        assert!(store.rename_project_folder(&folder.id, invalid).is_err());
        assert!(
            store
                .update_project_metadata(&project.id, invalid, None)
                .is_err()
        );
    }
    assert!(store.create_project_folder("äpp projects").is_err());
    assert!(
        store
            .rename_project_folder(&other.id, " ÄPP PROJECTS ")
            .is_err()
    );
    assert!(
        store
            .update_project_metadata(&project.id, "Changed", Some("missing"))
            .is_err()
    );
    assert!(
        store
            .update_project_metadata("missing", "Changed", Some(&folder.id))
            .is_err()
    );
    assert!(store.rename_project_folder("missing", "Changed").is_err());
    assert!(store.remove_project_folder("missing").is_err());
    assert_eq!(fs::read(store.dir.join("state.json")).unwrap(), before);

    let renamed = store
        .rename_project_folder(&folder.id, " ÄPP PROJECTS ")
        .unwrap();
    assert_eq!(renamed.id, folder.id);
    assert_eq!(renamed.name, "ÄPP PROJECTS");
    let assigned = store
        .update_project_metadata(&project.id, "  Updated  ", Some("work"))
        .unwrap();
    assert_eq!(assigned.name, "Updated");
    assert_eq!(assigned.folder_id, Some(other.id));
    let unassigned = store
        .update_project_metadata(&project.id, "Updated", None)
        .unwrap();
    assert_eq!(unassigned.folder_id, None);
}

#[test]
fn virtual_folder_rename_and_delete_preserve_projects_worktrees_and_files() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let first_root = fixture.directory("first");
    fs::write(first_root.join("keep.txt"), "keep this file").unwrap();
    let first = store.add_project(&first_root, Some("First")).unwrap();
    let second_root = fixture.directory("second");
    let second = store.add_project(&second_root, Some("Second")).unwrap();
    let third_root = fixture.directory("third");
    let third = store.add_project(&third_root, Some("Third")).unwrap();
    let folder = store.create_project_folder("Clients").unwrap();
    let other = store.create_project_folder("Personal").unwrap();
    for project in [&first, &second] {
        store
            .update_project_metadata(&project.id, &project.name, Some(&folder.id))
            .unwrap();
    }
    store
        .update_project_metadata(&third.id, &third.name, Some(&other.id))
        .unwrap();
    let task = store.add_task(&first.id, "Keep task", "Details").unwrap();
    let worktree = project_worktree(&store, &first.id, &first_root);
    store
        .assign_tasks(&worktree.id, &[task.id.clone()])
        .unwrap();
    let before = store.snapshot().unwrap();
    let renamed = store.rename_project_folder("clients", "Customers").unwrap();
    assert_eq!(renamed.id, folder.id);
    assert_eq!(renamed.created_at, folder.created_at);
    assert_eq!(
        store.snapshot().unwrap().projects[0].folder_id.as_deref(),
        Some(folder.id.as_str())
    );

    // Group operations must also work when a project's directory is unavailable.
    fs::remove_dir(&second_root).unwrap();
    store.remove_project_folder(&folder.id[..8]).unwrap();
    let after = store.snapshot().unwrap();
    assert_eq!(after.projects.len(), 3);
    assert_eq!(after.project_folders.len(), 1);
    assert_eq!(after.project_folders[0].id, other.id);
    for project in [&first, &second] {
        let updated = after.project(&project.id).unwrap();
        assert_eq!(updated.id, project.id);
        assert_eq!(updated.name, project.name);
        assert_eq!(updated.root, project.root);
        assert_eq!(updated.created_at, project.created_at);
        assert_eq!(updated.folder_id, None);
    }
    assert_eq!(
        after.project(&third.id).unwrap().folder_id.as_deref(),
        Some(other.id.as_str())
    );
    assert_eq!(after.active_project_id, before.active_project_id);
    assert_eq!(
        serde_json::to_value(&after.worktrees).unwrap(),
        serde_json::to_value(&before.worktrees).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&after.tasks).unwrap(),
        serde_json::to_value(&before.tasks).unwrap()
    );
    assert_eq!(
        fs::read_to_string(first_root.join("keep.txt")).unwrap(),
        "keep this file"
    );
    assert!(third_root.is_dir());
    assert!(!second_root.exists());
    assert!(!fixture.path("Customers").exists());
}

#[test]
fn project_registration_preserves_virtual_folder_assignment() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let root = fixture.directory("project");
    let project = store.add_project(&root, Some("Original")).unwrap();
    let folder = store.create_project_folder("Group").unwrap();
    store
        .update_project_metadata(&project.id, "Updated", Some(&folder.id))
        .unwrap();
    for registered in [
        store.add_project(&root, None).unwrap(),
        store.create_project(&root, Some("Again"), false).unwrap(),
    ] {
        assert_eq!(registered.id, project.id);
        assert_eq!(registered.folder_id.as_deref(), Some(folder.id.as_str()));
    }
    let reopened = fixture.store().snapshot().unwrap();
    assert_eq!(reopened.projects.len(), 1);
    assert_eq!(reopened.projects[0].name, "Again");
    assert_eq!(reopened.projects[0].folder_id, Some(folder.id));
}

#[test]
fn nested_folders_resolve_breadcrumbs_and_allow_leaf_names_in_different_parents() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let work = store.create_project_folder("Work").unwrap();
    let personal = store.create_project_folder("Personal").unwrap();
    let first = store
        .create_project_folder_in("Archive", Some(&work.id))
        .unwrap();
    let second = store
        .create_project_folder_in("archive", Some("personal"))
        .unwrap();
    let leaf = store
        .create_project_folder_in("2026", Some("WORK / Archive"))
        .unwrap();
    let state = store.snapshot().unwrap();
    assert_eq!(state.project_folder_path(&leaf.id), "Work / Archive / 2026");
    assert_eq!(
        state.project_folder(" work/archive /2026 ").unwrap().id,
        leaf.id
    );
    assert_eq!(state.project_folder("work/ARCHIVE").unwrap().id, first.id);
    assert_eq!(
        state.project_folder("personal / archive").unwrap().id,
        second.id
    );
    assert_eq!(state.project_folder(&first.id[..8]).unwrap().id, first.id);
    assert!(
        state
            .project_folder("archive")
            .unwrap_err()
            .contains("More than one")
    );
    assert_eq!(first.parent_id.as_deref(), Some(work.id.as_str()));
    assert_eq!(second.parent_id.as_deref(), Some(personal.id.as_str()));

    let before = fs::read(store.dir.join("state.json")).unwrap();
    assert!(
        store
            .create_project_folder_in(" ARCHIVE ", Some(&work.id))
            .is_err()
    );
    assert!(
        store
            .create_project_folder_in("invalid/name", Some(&work.id))
            .is_err()
    );
    assert!(
        store
            .create_project_folder_in("New", Some("missing"))
            .is_err()
    );
    assert_eq!(fs::read(store.dir.join("state.json")).unwrap(), before);

    // Older virtual folders did not encode ancestry; they remain root folders.
    let mut legacy: serde_json::Value = serde_json::from_slice(&before).unwrap();
    for item in legacy["project_folders"].as_array_mut().unwrap() {
        if item["id"] == work.id || item["id"] == personal.id {
            item.as_object_mut().unwrap().remove("parent_id");
        }
    }
    fs::write(
        store.dir.join("state.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let reopened = fixture.store().snapshot().unwrap();
    assert_eq!(reopened.project_folder(&work.id).unwrap().parent_id, None);
    assert_eq!(
        reopened.project_folder_path(&leaf.id),
        "Work / Archive / 2026"
    );
    assert_eq!(reopened.schema_version, 1);
}

#[test]
fn moving_folders_preserves_descendants_and_rejects_cycles_or_sibling_collisions_atomically() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let work = store.create_project_folder("Work").unwrap();
    let personal = store.create_project_folder("Personal").unwrap();
    let archive = store
        .create_project_folder_in("Archive", Some(&work.id))
        .unwrap();
    let leaf = store
        .create_project_folder_in("Invoices", Some(&archive.id))
        .unwrap();
    store
        .create_project_folder_in("ARCHIVE", Some(&personal.id))
        .unwrap();
    store
        .create_project_folder_in("Other", Some(&work.id))
        .unwrap();
    let project = store
        .add_project(fixture.directory("project"), Some("Project"))
        .unwrap();
    store
        .move_project_to_folder(&project.id, Some(&leaf.id))
        .unwrap();
    let before = fs::read(store.dir.join("state.json")).unwrap();
    for (source, parent) in [
        (&work.id, work.id.as_str()),
        (&work.id, leaf.id.as_str()),
        (&archive.id, leaf.id.as_str()),
        (&archive.id, personal.id.as_str()),
        (&archive.id, "missing"),
    ] {
        assert!(store.move_project_folder(source, Some(parent)).is_err());
    }
    assert!(store.move_project_folder("missing", None).is_err());
    assert!(store.rename_project_folder(&archive.id, " OTHER ").is_err());
    assert!(
        store
            .rename_project_folder(&archive.id, "invalid/name")
            .is_err()
    );
    assert_eq!(fs::read(store.dir.join("state.json")).unwrap(), before);

    let moved = store.move_project_folder("Work / Archive", None).unwrap();
    assert_eq!(moved.id, archive.id);
    assert_eq!(moved.parent_id, None);
    assert_eq!(moved.created_at, archive.created_at);
    let state = store.snapshot().unwrap();
    assert_eq!(state.project_folder_path(&leaf.id), "Archive / Invoices");
    assert_eq!(
        state.project_folder(&leaf.id).unwrap().parent_id.as_deref(),
        Some(archive.id.as_str())
    );
    assert_eq!(
        state.project(&project.id).unwrap().folder_id.as_deref(),
        Some(leaf.id.as_str())
    );
    store
        .move_project_folder(&archive.id, Some(&work.id))
        .unwrap();
    assert_eq!(
        fixture
            .store()
            .snapshot()
            .unwrap()
            .project_folder_path(&leaf.id),
        "Work / Archive / Invoices"
    );
}

#[test]
fn removing_nested_folders_promotes_direct_children_and_projects_without_touching_descendants() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let work = store.create_project_folder("Work").unwrap();
    let clients = store
        .create_project_folder_in("Clients", Some(&work.id))
        .unwrap();
    let active = store
        .create_project_folder_in("Active", Some(&clients.id))
        .unwrap();
    let deep = store
        .create_project_folder_in("Deep", Some(&active.id))
        .unwrap();
    let personal = store
        .create_project_folder_in("Personal", Some(&work.id))
        .unwrap();
    let roots: Vec<_> = ["direct", "child", "deep"]
        .into_iter()
        .map(|name| fixture.directory(name))
        .collect();
    let projects: Vec<_> = roots
        .iter()
        .map(|root| store.add_project(root, None).unwrap())
        .collect();
    fs::write(roots[0].join("keep.txt"), "keep").unwrap();
    for (project, folder) in projects.iter().zip([&clients, &active, &deep]) {
        store
            .move_project_to_folder(&project.id, Some(&folder.id))
            .unwrap();
    }
    let task = store
        .add_task(&projects[0].id, "Keep task", "Details")
        .unwrap();
    let worktree = project_worktree(&store, &projects[0].id, &roots[0]);
    store.assign_tasks(&worktree.id, &[task.id]).unwrap();
    let before = store.snapshot().unwrap();
    store.remove_project_folder(&clients.id).unwrap();
    let promoted = store.snapshot().unwrap();
    assert!(promoted.project_folder(&clients.id).is_err());
    assert_eq!(
        promoted
            .project_folder(&active.id)
            .unwrap()
            .parent_id
            .as_deref(),
        Some(work.id.as_str())
    );
    assert_eq!(
        promoted
            .project_folder(&deep.id)
            .unwrap()
            .parent_id
            .as_deref(),
        Some(active.id.as_str())
    );
    assert_eq!(
        promoted.project_folder_path(&deep.id),
        "Work / Active / Deep"
    );
    assert_eq!(
        promoted
            .project(&projects[0].id)
            .unwrap()
            .folder_id
            .as_deref(),
        Some(work.id.as_str())
    );
    assert_eq!(
        promoted
            .project(&projects[1].id)
            .unwrap()
            .folder_id
            .as_deref(),
        Some(active.id.as_str())
    );
    assert_eq!(
        promoted
            .project(&projects[2].id)
            .unwrap()
            .folder_id
            .as_deref(),
        Some(deep.id.as_str())
    );

    store.remove_project_folder(&work.id).unwrap();
    let after = fixture.store().snapshot().unwrap();
    assert_eq!(after.project_folder(&active.id).unwrap().parent_id, None);
    assert_eq!(after.project_folder(&personal.id).unwrap().parent_id, None);
    assert_eq!(
        after.project_folder(&deep.id).unwrap().parent_id.as_deref(),
        Some(active.id.as_str())
    );
    assert_eq!(after.project(&projects[0].id).unwrap().folder_id, None);
    assert_eq!(after.projects.len(), projects.len());
    assert_eq!(after.active_project_id, before.active_project_id);
    assert_eq!(
        serde_json::to_value(&after.worktrees).unwrap(),
        serde_json::to_value(&before.worktrees).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&after.tasks).unwrap(),
        serde_json::to_value(&before.tasks).unwrap()
    );
    for (project, root) in projects.iter().zip(&roots) {
        assert_eq!(after.project(&project.id).unwrap().root, *root);
        assert!(root.is_dir());
    }
    assert_eq!(
        fs::read_to_string(roots[0].join("keep.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn folder_removal_fails_without_writes_when_promoting_children_would_collide() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let work = store.create_project_folder("Work").unwrap();
    let parent = store
        .create_project_folder_in("Container", Some(&work.id))
        .unwrap();
    let child = store
        .create_project_folder_in("Archive", Some(&parent.id))
        .unwrap();
    let conflicting = store
        .create_project_folder_in("ARCHIVE", Some(&work.id))
        .unwrap();
    let project = store
        .add_project(fixture.directory("project"), None)
        .unwrap();
    store
        .move_project_to_folder(&project.id, Some(&parent.id))
        .unwrap();
    let before = fs::read(store.dir.join("state.json")).unwrap();
    let error = store.remove_project_folder(&parent.id).unwrap_err();
    assert!(
        error.contains("Archive") && error.contains("duplicate"),
        "{error}"
    );
    assert_eq!(fs::read(store.dir.join("state.json")).unwrap(), before);
    store
        .rename_project_folder(&conflicting.id, "Old archive")
        .unwrap();
    store.remove_project_folder(&parent.id).unwrap();
    let state = store.snapshot().unwrap();
    assert_eq!(
        state
            .project_folder(&child.id)
            .unwrap()
            .parent_id
            .as_deref(),
        Some(work.id.as_str())
    );
    assert_eq!(
        state.project(&project.id).unwrap().folder_id.as_deref(),
        Some(work.id.as_str())
    );

    // The removed folder's name is available to a promoted child of the same name.
    let same_name = store.create_project_folder("Same").unwrap();
    let same_child = store
        .create_project_folder_in("Same", Some(&same_name.id))
        .unwrap();
    store.remove_project_folder(&same_name.id).unwrap();
    assert_eq!(
        store.snapshot().unwrap().project_folder("Same").unwrap().id,
        same_child.id
    );
}

#[test]
fn moving_projects_changes_only_assignment_and_preserves_the_latest_name() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let project = store
        .add_project(fixture.directory("project"), Some("Original"))
        .unwrap();
    let parent = store.create_project_folder("Work").unwrap();
    let child = store
        .create_project_folder_in("Clients", Some(&parent.id))
        .unwrap();
    // A drag can retain an old UI record while another window saves a newer name.
    store
        .update_project_metadata(&project.id, "Renamed elsewhere", None)
        .unwrap();
    let moved = store
        .move_project_to_folder(&project.id, Some("Work / Clients"))
        .unwrap();
    assert_eq!(moved.name, "Renamed elsewhere");
    assert_eq!(moved.folder_id.as_deref(), Some(child.id.as_str()));
    assert_eq!(moved.root, project.root);
    assert_eq!(moved.id, project.id);
    assert_eq!(moved.created_at, project.created_at);
    assert_eq!(moved.repository_roots, project.repository_roots);
    let before = fs::read(store.dir.join("state.json")).unwrap();
    assert!(
        store
            .move_project_to_folder(&project.id, Some("missing"))
            .is_err()
    );
    assert!(
        store
            .move_project_to_folder("missing", Some(&child.id))
            .is_err()
    );
    assert_eq!(fs::read(store.dir.join("state.json")).unwrap(), before);
    let unfiled = store.move_project_to_folder(&project.id, None).unwrap();
    assert_eq!(unfiled.folder_id, None);
    assert_eq!(unfiled.name, "Renamed elsewhere");
    assert_eq!(
        fixture
            .store()
            .snapshot()
            .unwrap()
            .project(&project.id)
            .unwrap()
            .name,
        "Renamed elsewhere"
    );
}

#[test]
fn project_creation_initializes_existing_and_missing_plain_directories() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let existing = fixture.directory("existing");
    fs::write(existing.join("keep.txt"), "keep this file").unwrap();
    let missing = fixture.path("new-parent/new-project");

    for path in [&existing, &missing] {
        let before = Store::inspect_project(path).unwrap();
        assert_eq!(before.exists, path == &existing);
        assert_eq!(before.repository_count, 0);
        assert!(before.can_init_git);
        assert!(before.discovery_complete);

        let project = store.create_project(path, None, true).unwrap();
        assert_eq!(project.root, path.canonicalize().unwrap());
        assert_eq!(project.repository_roots, vec![project.root.clone()]);
        assert!(project.root.join(".git").is_dir());
        let primary = project_worktree(&store, &project.id, &project.root);
        assert!(primary.is_primary);
        assert_eq!(primary.repository_root, Some(project.root.clone()));
    }
    assert_eq!(
        fs::read_to_string(existing.join("keep.txt")).unwrap(),
        "keep this file"
    );
    assert_eq!(store.snapshot().unwrap().projects.len(), 2);
}

#[test]
fn no_git_creation_and_passive_registration_preserve_plain_folders() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let created = fixture.path("created/plain");
    let project = store
        .create_project(&created, Some("plain"), false)
        .unwrap();
    assert!(created.is_dir());
    assert!(!created.join(".git").exists());
    assert!(project.repository_roots.is_empty());
    let primary = project_worktree(&store, &project.id, &project.root);
    assert!(primary.is_primary);
    assert_eq!(primary.repository_root, None);

    let existing = fixture.directory("registered");
    let registered = store.add_project(&existing, None).unwrap();
    assert!(registered.repository_roots.is_empty());
    assert!(!existing.join(".git").exists());
    let missing = fixture.path("not-created/by-registration");
    assert!(store.add_project(&missing, None).is_err());
    assert!(!missing.exists());
    assert_eq!(store.snapshot().unwrap().projects.len(), 2);

    let destination = fixture.path("plain-worktree-parent/worktree");
    let error = store
        .create_worktree(&project.id, "task", Some(&destination), None)
        .unwrap_err();
    assert!(error.contains("plain folder"), "{error}");
    assert!(!destination.parent().unwrap().exists());
}

#[test]
fn existing_repository_and_ancestor_repository_are_never_wrapped() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let repository = fixture.repository("repository", true);
    let root_project = store.create_project(&repository, None, true).unwrap();
    assert_eq!(root_project.repository_roots, vec![repository.clone()]);

    let subfolder = fixture.directory("repository/packages/component");
    let inspection = Store::inspect_project(&subfolder).unwrap();
    assert_eq!(inspection.repository_roots, vec![repository.clone()]);
    assert!(!inspection.can_init_git);
    let project = store.create_project(&subfolder, None, true).unwrap();
    assert_eq!(project.root, subfolder);
    assert_eq!(project.repository_roots, vec![repository.clone()]);
    assert!(!subfolder.join(".git").exists());
    assert_eq!(
        project_worktree(&store, &project.id, &subfolder).repository_root,
        Some(repository.clone())
    );

    // The nearest existing directory also identifies an ancestor repository
    // when the requested project directory does not exist yet.
    let missing = fixture.path("repository/new/deep/project");
    let inspection = Store::inspect_project(&missing).unwrap();
    assert!(!inspection.exists);
    assert_eq!(inspection.repository_roots, vec![repository.clone()]);
    let project = store.create_project(&missing, None, true).unwrap();
    assert_eq!(project.repository_roots, vec![repository]);
    assert!(!missing.join(".git").exists());
}

#[test]
fn container_discovers_multiple_repositories_and_external_worktrees_without_losing_ids() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let container = fixture.directory("container");
    let api = fixture.repository("container/api", true);
    let web = fixture.repository("container/web", true);
    let external = fixture.path("external-api");
    git(
        &api,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new("external"),
            external.as_os_str(),
        ],
    );

    let project = store.create_project(&container, None, true).unwrap();
    assert_eq!(project.repository_roots, vec![api.clone(), web.clone()]);
    assert!(!container.join(".git").exists());
    let initial = store.snapshot().unwrap();
    let original_ids: BTreeMap<_, _> = initial
        .worktrees_for(&project.id)
        .into_iter()
        .map(|worktree| (worktree.path.clone(), worktree.id.clone()))
        .collect();
    assert_eq!(original_ids.len(), 4); // Project folder, two repos, external worktree.
    let external_record = project_worktree(&store, &project.id, &external);
    assert_eq!(external_record.repository_root, Some(api.clone()));
    let task = store.add_task(&project.id, "Keep assignment", "").unwrap();
    store
        .assign_tasks(&external_record.id, &[task.id.clone()])
        .unwrap();

    git(&external, ["switch", "-c", "external-renamed"]);
    let later = fixture.path("later-web");
    git(
        &web,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new("later"),
            later.as_os_str(),
        ],
    );
    let synchronized = store.sync_worktrees(&project.id).unwrap();
    assert_eq!(synchronized.len(), 5);
    for worktree in &synchronized {
        if let Some(original) = original_ids.get(&worktree.path) {
            assert_eq!(&worktree.id, original);
        }
    }
    let updated_external = project_worktree(&store, &project.id, &external);
    assert_eq!(updated_external.id, external_record.id);
    assert_eq!(updated_external.branch, "external-renamed");
    assert_eq!(
        store
            .snapshot()
            .unwrap()
            .task(&task.id)
            .unwrap()
            .worktree_id,
        Some(external_record.id)
    );
    assert_eq!(
        project_worktree(&store, &project.id, &later).repository_root,
        Some(web)
    );
    assert!(!container.join(".git").exists());
}

#[test]
fn multi_repository_creation_requires_unambiguous_selection_and_uses_selected_repo() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let container = fixture.directory("container");
    let first = fixture.repository("container/service-a/repo", true);
    let second = fixture.repository("container/service-b/repo", true);
    // A branch label in another repo must not override a matching repo path.
    git(&second, ["switch", "-c", "service-a/repo"]);
    let project = store.add_project(&container, None).unwrap();
    let rejected = fixture.path("rejected-parent/worktree");

    let error = store
        .create_worktree(&project.id, "task", Some(&rejected), None)
        .unwrap_err();
    assert!(error.contains("choose --repo"), "{error}");
    let error = store
        .create_worktree_in_repo(&project.id, "task", Some(&rejected), None, Some("repo"))
        .unwrap_err();
    assert!(error.contains("ambiguous"), "{error}");
    assert!(!rejected.parent().unwrap().exists());

    let first_destination = fixture.path("first-worktree");
    let created = store
        .create_worktree_in_repo(
            &project.id,
            "first-task",
            Some(&first_destination),
            None,
            Some("service-a/repo"),
        )
        .unwrap();
    assert_eq!(created.repository_root, Some(first.clone()));
    assert_eq!(
        common_directory(&first_destination),
        common_directory(&first)
    );
    assert_ne!(
        common_directory(&first_destination),
        common_directory(&second)
    );

    let second_record = project_worktree(&store, &project.id, &second);
    let second_destination = fixture.path("second-worktree");
    let created = store
        .create_worktree_in_repo(
            &project.id,
            "second-task",
            Some(&second_destination),
            None,
            Some(&second_record.id),
        )
        .unwrap();
    assert_eq!(created.repository_root, Some(second.clone()));
    assert_eq!(
        common_directory(&second_destination),
        common_directory(&second)
    );
}

#[test]
fn unborn_or_invalid_base_fails_before_creating_destination_or_parent() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let unborn = fixture.repository("unborn", false);
    let project = store.add_project(&unborn, None).unwrap();
    let destination = fixture.path("unborn-parent/deep/worktree");
    let error = store
        .create_worktree(&project.id, "task", Some(&destination), None)
        .unwrap_err();
    assert!(error.contains("no initial commit"), "{error}");
    assert!(!fixture.path("unborn-parent").exists());
    assert_eq!(
        store.snapshot().unwrap().worktrees_for(&project.id).len(),
        1
    );

    git(
        &unborn,
        ["commit", "--allow-empty", "--no-gpg-sign", "-m", "fixture"],
    );
    let destination = fixture.path("invalid-parent/deep/worktree");
    let error = store
        .create_worktree(
            &project.id,
            "task",
            Some(&destination),
            Some("missing-base"),
        )
        .unwrap_err();
    assert!(error.contains("does not resolve to a commit"), "{error}");
    assert!(!fixture.path("invalid-parent").exists());
    assert_eq!(
        store.snapshot().unwrap().worktrees_for(&project.id).len(),
        1
    );
}

#[test]
fn linked_gitfiles_deduplicate_repositories_and_preserve_newline_paths() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let container = fixture.directory("container");
    let repository = fixture.repository("container/main\nrepo", true);
    let linked = fixture.path("container/linked\ncopy\n");
    git(
        &repository,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new("linked"),
            linked.as_os_str(),
        ],
    );
    assert!(linked.join(".git").is_file());

    for root in [&container, &linked] {
        let inspection = Store::inspect_project(root).unwrap();
        assert_eq!(inspection.repository_count, 1);
        assert_eq!(inspection.repository_roots, vec![repository.clone()]);
        assert!(inspection.discovery_complete);
    }
    let project = store.create_project(&container, None, true).unwrap();
    assert_eq!(
        store.snapshot().unwrap().worktrees_for(&project.id).len(),
        3
    );
    let linked_record = project_worktree(&store, &project.id, &linked);
    assert_eq!(linked_record.path, linked);
    assert_eq!(linked_record.branch, "linked");
    assert_eq!(linked_record.repository_root, Some(repository));
    assert!(!container.join(".git").exists());
    store.sync_worktrees(&project.id).unwrap();
    assert_eq!(
        project_worktree(&store, &project.id, &linked).id,
        linked_record.id
    );
}

#[cfg(unix)]
#[test]
fn discovery_does_not_follow_symlink_loops_or_scan_dependency_and_build_directories() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let store = fixture.store();
    let container = fixture.directory("container");
    let visible = fixture.repository("container/source", true);
    fixture.repository("container/target/ignored", true);
    fixture.repository("container/node_modules/ignored", true);
    let external = fixture.repository("outside", true);
    symlink(&container, container.join("loop")).unwrap();
    symlink(&external, container.join("external-link")).unwrap();

    let inspection = Store::inspect_project(&container).unwrap();
    assert_eq!(inspection.repository_roots, vec![visible.clone()]);
    assert_eq!(inspection.repository_count, 1);
    assert!(inspection.discovery_complete);
    let project = store.create_project(&container, None, true).unwrap();
    assert_eq!(project.repository_roots, vec![visible]);
    assert_eq!(
        store.snapshot().unwrap().worktrees_for(&project.id).len(),
        2
    );
    assert_eq!(fs::read_link(container.join("loop")).unwrap(), container);
    assert_eq!(
        fs::read_link(container.join("external-link")).unwrap(),
        external
    );
    assert!(!container.join(".git").exists());
}

#[test]
fn invalid_git_metadata_blocks_initialization_without_overwriting_it() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let root = fixture.directory("invalid");
    let metadata = "this is not valid Git metadata\n";
    fs::write(root.join(".git"), metadata).unwrap();

    let inspection = Store::inspect_project(&root).unwrap();
    assert_eq!(inspection.repository_count, 0);
    assert!(!inspection.discovery_complete);
    assert!(!inspection.can_init_git);
    assert!(inspection.warning.is_some());
    let error = store.create_project(&root, None, true).unwrap_err();
    assert!(error.contains("Cannot safely initialize Git"), "{error}");
    assert_eq!(fs::read_to_string(root.join(".git")).unwrap(), metadata);
    assert!(store.snapshot().unwrap().projects.is_empty());
}

#[test]
fn discovery_depth_limit_blocks_wrapper_initialization_and_preserves_deep_repository() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let root = fixture.directory("deep-project");
    let mut relative = PathBuf::from("deep-project");
    for _ in 0..26 {
        relative.push("nested");
    }
    let leaf_repository = fixture.repository(&relative, true);

    let inspection = Store::inspect_project(&root).unwrap();
    assert_eq!(inspection.repository_count, 0);
    assert!(!inspection.discovery_complete);
    assert!(!inspection.can_init_git);
    assert!(
        inspection
            .warning
            .as_deref()
            .unwrap()
            .contains("depth limit"),
        "{:?}",
        inspection.warning
    );
    let error = store.create_project(&root, None, true).unwrap_err();
    assert!(error.contains("Cannot safely initialize Git"), "{error}");
    assert!(!root.join(".git").exists());
    assert!(leaf_repository.join(".git").is_dir());
    assert_eq!(
        Store::inspect_project(&leaf_repository)
            .unwrap()
            .repository_roots,
        vec![leaf_repository]
    );
    assert!(store.snapshot().unwrap().projects.is_empty());
}

fn write_state_json(store: &Store, state: &serde_json::Value) {
    fs::write(
        store.dir.join("state.json"),
        serde_json::to_vec_pretty(state).unwrap(),
    )
    .unwrap();
}

fn read_state_json(store: &Store) -> serde_json::Value {
    serde_json::from_slice(&fs::read(store.dir.join("state.json")).unwrap()).unwrap()
}

#[test]
fn unknown_fields_from_a_newer_build_survive_a_transaction() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let root = fixture.directory("compat-project");
    write_state_json(
        &store,
        &serde_json::json!({
            "schema_version": 1,
            "active_project_id": "p1",
            "future_top_level": {"flag": true},
            "project_folders": [
                {"id": "f1", "name": "Work", "parent_id": null, "created_at": 1, "future_folder": "color"},
            ],
            "projects": [
                {"id": "p1", "name": "Compat", "root": root, "folder_id": "f1", "created_at": 2,
                 "codex_account": {"source": "inherit"}, "future_project": [1, 2, 3]},
            ],
            "worktrees": [
                {"id": "w1", "project_id": "p1", "branch": "main", "path": root, "is_primary": true,
                 "created_at": 3, "future_worktree": null},
            ],
            "tasks": [
                {"id": "t1", "project_id": "p1", "title": "Keep me", "created_at": 4, "updated_at": 4,
                 "future_task": {"priority": 9}},
                {"id": "t2", "project_id": "p1", "title": "Delete me", "created_at": 5, "updated_at": 5,
                 "future_task": "gone with its task"},
            ],
            "orca_import": {"source": "/orca", "project_count": 1, "folder_count": 0,
                            "worktree_count": 1, "completed_at": 6, "future_receipt": "kept"},
        }),
    );

    let added = store.add_task("p1", "New task", "").unwrap();
    store
        .update_project_metadata("p1", "Renamed", Some("f1"))
        .unwrap();
    store.set_task_status("t1", TaskStatus::InProgress).unwrap();
    store
        .transaction(|state| {
            state.tasks.retain(|task| task.id != "t2");
            Ok(())
        })
        .unwrap();

    let saved = read_state_json(&store);
    assert_eq!(saved["future_top_level"], serde_json::json!({"flag": true}));
    assert_eq!(saved["project_folders"][0]["future_folder"], "color");
    assert_eq!(saved["projects"][0]["name"], "Renamed");
    assert_eq!(
        saved["projects"][0]["future_project"],
        serde_json::json!([1, 2, 3])
    );
    assert!(saved["worktrees"][0]["future_worktree"].is_null());
    assert!(
        saved["worktrees"][0]
            .as_object()
            .unwrap()
            .contains_key("future_worktree")
    );
    assert_eq!(saved["orca_import"]["future_receipt"], "kept");
    let tasks = saved["tasks"].as_array().unwrap();
    assert_eq!(
        tasks.len(),
        2,
        "the deleted task takes its extra fields with it"
    );
    assert_eq!(tasks[0]["id"], "t1");
    assert_eq!(tasks[0]["status"], "in_progress");
    assert_eq!(tasks[0]["future_task"], serde_json::json!({"priority": 9}));
    assert_eq!(tasks[1]["id"], added.id);
    assert!(tasks[1].get("future_task").is_none());
    // The typed view never surfaces or depends on the unknown fields.
    let state = store.snapshot().unwrap();
    assert_eq!(state.projects[0].name, "Renamed");
    assert_eq!(state.tasks[0].status, TaskStatus::InProgress);
}

#[test]
fn moving_a_project_out_of_a_folder_does_not_resurrect_the_old_assignment() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let root = fixture.directory("clear-folder");
    write_state_json(
        &store,
        &serde_json::json!({
            "schema_version": 1,
            "project_folders": [{"id": "f1", "name": "Work", "created_at": 1}],
            "projects": [{"id": "p1", "name": "Clear", "root": root, "folder_id": "f1", "created_at": 2,
                          "codex_account": {"source": "saved", "account_id": "acct"}}],
        }),
    );
    store.move_project_to_folder("p1", None).unwrap();
    store
        .set_project_codex_account("p1", ProjectCodexAccount::SystemDefault)
        .unwrap();
    let saved = read_state_json(&store);
    assert!(saved["projects"][0]["folder_id"].is_null());
    assert_eq!(
        saved["projects"][0]["codex_account"],
        serde_json::json!({"source": "system_default"})
    );
}

#[test]
fn newer_store_schemas_are_refused_and_left_untouched() {
    let fixture = Fixture::new();
    let store = fixture.store();
    write_state_json(
        &store,
        &serde_json::json!({"schema_version": 2, "projects": [], "worktrees": [], "tasks": []}),
    );
    let before = fs::read(store.dir.join("state.json")).unwrap();
    assert!(
        store
            .snapshot()
            .unwrap_err()
            .contains("Unsupported store schema 2")
    );
    assert!(store.add_task("nothing", "Title", "").is_err());
    assert_eq!(fs::read(store.dir.join("state.json")).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn state_files_are_private_and_new_directories_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let fixture = Fixture::new();
    let nested = fixture.path("parent/riwork-home");
    let store = Store::open(&nested).unwrap();
    assert_eq!(mode(&nested), 0o700);
    let control = fixture.directory("control");
    assert_eq!(
        mode(nested.parent().unwrap()),
        mode(&control),
        "only the data directory is private"
    );
    let root = fixture.directory("private-project");
    store.add_project(&root, None).unwrap();
    assert_eq!(mode(&nested.join("state.json")), 0o600);
    assert_eq!(mode(&nested.join("state.lock")), 0o600);

    // An older 0644 file becomes private the next time it is rewritten, and an
    // existing directory keeps whatever mode its owner chose.
    let shared = fixture.directory("shared-home");
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
    let store = Store::open(&shared).unwrap();
    fs::write(shared.join("state.json"), r#"{"schema_version":1}"#).unwrap();
    fs::set_permissions(shared.join("state.json"), fs::Permissions::from_mode(0o644)).unwrap();
    store.add_project(&root, None).unwrap();
    assert_eq!(mode(&shared.join("state.json")), 0o600);
    assert_eq!(mode(&shared), 0o755);
}

#[test]
fn an_empty_data_directory_is_rejected_instead_of_using_the_working_directory() {
    let error = Store::open("").unwrap_err();
    assert!(error.contains("empty"), "{error}");
}

#[test]
#[cfg(unix)]
fn worktree_creation_runs_git_and_its_hooks_outside_the_store_lock() {
    use std::{os::unix::fs::PermissionsExt, sync::mpsc, thread, time::Duration};
    let fixture = Fixture::new();
    let store = fixture.store();
    let repository = fixture.repository("hooked", true);
    let project = store.add_project(&repository, None).unwrap();
    let marks = fixture.directory("marks");
    let hooks = fixture.directory("hooks");
    let hook = hooks.join("post-checkout");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\n: > '{started}'\nwhile [ ! -e '{release}' ]; do sleep 0.05; done\n",
            started = marks.join("started").display(),
            release = marks.join("release").display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &repository,
        [
            "config",
            "--local",
            "core.hooksPath",
            hooks.to_str().unwrap(),
        ],
    );

    let creator = {
        let store = store.clone();
        let project_id = project.id.clone();
        thread::spawn(move || store.create_worktree(&project_id, "feature", None, None))
    };
    let release = marks.join("release");
    let started = marks.join("started");
    for _ in 0..400 {
        if started.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let hook_running = started.exists();
    // While the hook blocks `git worktree add`, other windows and the hook's own
    // `riwork` calls must still read and write the store. A sync in another
    // window even discovers the half-finished worktree first.
    let (sent, received) = mpsc::channel();
    {
        let store = store.clone();
        let project_id = project.id.clone();
        thread::spawn(move || {
            let snapshot = store.snapshot().map(|state| state.worktrees.len());
            let synced = store.sync_worktrees(&project_id).map(|list| list.len());
            let _ = sent.send((snapshot, synced));
        });
    }
    let outcome = received.recv_timeout(Duration::from_secs(20));
    fs::write(&release, b"").unwrap();
    let created = creator.join().unwrap();
    assert!(hook_running, "the post-checkout hook never ran");
    let (snapshot, synced) = outcome.expect("the store stayed locked while git was running");
    assert_eq!(snapshot.unwrap(), 1);
    assert_eq!(synced.unwrap(), 2, "the sync saw the new worktree");

    let created = created.unwrap();
    let expected = fixture.path("hooked-feature").canonicalize().unwrap();
    assert_eq!(created.path, expected);
    let state = store.snapshot().unwrap();
    let recorded = state
        .worktrees
        .iter()
        .filter(|worktree| worktree.path == expected)
        .collect::<Vec<_>>();
    assert_eq!(
        recorded.len(),
        1,
        "the concurrent duplicate collapses to one record"
    );
    assert_eq!(recorded[0].id, created.id);
    assert_eq!(recorded[0].branch, "feature");
}
