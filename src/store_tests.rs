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
