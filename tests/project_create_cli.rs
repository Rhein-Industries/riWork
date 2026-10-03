//! `riwork project create --exclusive` as the remote connector drives it for the phone's
//! `project.create`: the argv it sends, the JSON it reads back (the project `project list` shows),
//! the `already_exists:` tokens it turns into an error code (`remote/src/rpc.rs`,
//! `project_create_fault`) and what the flag promises never to touch. Every child runs with a
//! throwaway RIWORK_HOME *and* a throwaway HOME, so the default projects folder
//! (`~/Documents/riwork`) is a folder of this test's own; nothing of a real installation is read
//! or written.

use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        // macOS temp dirs are symlinks; project roots are stored canonically.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-project-create-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("cwd")).unwrap();
        Self { root }
    }

    /// What `~` is for the children: `default_projects_directory()` is under it.
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn projects(&self) -> PathBuf {
        self.home().join("Documents/riwork")
    }

    /// Where the children run, so a flag read as a PATH would land here, not in the repository.
    fn cwd(&self) -> PathBuf {
        self.root.join("cwd")
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_riwork"));
        command
            .args(args)
            .current_dir(self.cwd())
            .env("HOME", self.home())
            .env("RIWORK_HOME", self.root.join("state"))
            .env("RIWORK_RUNTIME_DIR", self.root.join("runtime"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        finish(command.spawn().unwrap(), args)
    }

    fn run_vec(&self, args: &[String]) -> Output {
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run(&borrowed)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// The first line the CLI wrote to stderr for a failure, as the connector reads it.
    fn failure(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(!output.status.success(), "{args:?} succeeded");
        assert!(output.stdout.is_empty(), "{args:?} printed a project");
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    /// What the connector sends for a validated `project.create`, and the project the CLI prints.
    fn created(&self, name: &str, git: bool) -> Value {
        let output = self.run_vec(&connector_argv(name, git));
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// The same request, refused: the first line of stderr.
    fn refused(&self, name: &str) -> String {
        let output = self.run_vec(&connector_argv(name, true));
        assert!(!output.status.success(), "{name} was created");
        assert!(output.stdout.is_empty(), "{name} printed a project");
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    fn projects_listed(&self) -> Vec<Value> {
        self.ok(&["project", "list", "--json"])
            .as_array()
            .unwrap()
            .clone()
    }

    /// Everything under the sandbox that is not the state or runtime directory, as relative paths.
    fn tree(&self) -> Vec<String> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
            let mut entries: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let relative = path.strip_prefix(base).unwrap().display().to_string();
                if relative == "state" || relative == "runtime" || relative.ends_with(".git") {
                    continue;
                }
                out.push(relative);
                if path.is_dir() && !fs::symlink_metadata(&path).unwrap().is_symlink() {
                    walk(base, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &self.root, &mut out);
        out
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // The directory was created for this test and holds nothing of anyone's.
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn finish(mut child: Child, what: &[&str]) -> Output {
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("riwork {what:?} did not exit; it may have started the GUI");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_end(&mut output.stdout).unwrap();
    }
    if let Some(mut stderr) = child.stderr.take() {
        stderr.read_to_end(&mut output.stderr).unwrap();
    }
    output
}

/// The argv the connector builds for a validated `project.create`, then `--json`
/// (`remote/src/rpc.rs`, `project_args`).
fn connector_argv(name: &str, git: bool) -> Vec<String> {
    let mut args: Vec<String> = ["project", "create", "--name", name]
        .map(String::from)
        .to_vec();
    if !git {
        args.push("--no-git".into());
    }
    args.push("--exclusive".into());
    args.push("--json".into());
    args
}

fn is_canonical_uuid(text: &str) -> bool {
    Uuid::parse_str(text).is_ok_and(|uuid| uuid.to_string() == text)
}

#[test]
fn the_cli_says_it_can_create_exclusively() {
    let sandbox = Sandbox::new();
    let capabilities = sandbox.ok(&["capabilities", "--json"]);
    assert_eq!(capabilities["v"], 1);
    assert_eq!(capabilities["project_create_exclusive"], true);
    // The earlier question keeps its answer.
    assert_eq!(capabilities["verifies_shell"], true);
    let text = sandbox.run(&["capabilities"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("project_create_exclusive yes"), "{text}");
}

#[test]
fn a_project_is_made_in_the_default_folder_with_git_and_printed_as_the_list_shows_it() {
    let sandbox = Sandbox::new();
    assert!(!sandbox.projects().exists(), "the sandbox starts empty");
    let created = sandbox.created("Fresh App", true);
    let root = sandbox.projects().join("Fresh App");
    assert!(is_canonical_uuid(created["id"].as_str().unwrap()));
    assert_eq!(created["name"], "Fresh App");
    assert_eq!(created["root"], root.to_str().unwrap());
    assert!(created["created_at"].as_u64().unwrap() > 0);
    assert!(root.join(".git").is_dir(), "Git by default");
    // It is the very entry `project list` shows, which adds the optional recency and agent
    // counts of the project (`last_edited_unix` and `agents`) that `project create` does not know.
    let mut listed = sandbox.projects_listed();
    for entry in &mut listed {
        for additive in ["last_edited_unix", "agents"] {
            entry.as_object_mut().unwrap().remove(additive);
        }
    }
    assert_eq!(listed, vec![created.clone()]);
    // The fields the connector passes on are there; the others it leaves out.
    for field in ["id", "name", "root", "created_at"] {
        assert!(created.get(field).is_some(), "{field}");
    }
    // Only the sandbox changed.
    assert_eq!(
        sandbox.tree(),
        [
            "cwd",
            "home",
            "home/Documents",
            "home/Documents/riwork",
            "home/Documents/riwork/Fresh App"
        ]
    );
}

#[test]
fn no_git_makes_a_plain_folder() {
    let sandbox = Sandbox::new();
    let created = sandbox.created("Plain", false);
    let root = sandbox.projects().join("Plain");
    assert_eq!(created["root"], root.to_str().unwrap());
    assert!(root.is_dir());
    assert!(!root.join(".git").exists());
}

#[test]
fn names_with_spaces_quotes_and_unicode_are_one_folder_each() {
    let sandbox = Sandbox::new();
    for name in [
        "My App",
        "a;b $(touch pwned) `id` \"q\" 'z'",
        "caf\u{e9} \u{65e5}\u{672c}\u{8a9e} \u{1f600}",
        "x&y|z>w<v",
        "trailing.dot.",
    ] {
        let created = sandbox.created(name, false);
        assert_eq!(created["name"], *name);
        assert_eq!(
            created["root"],
            sandbox.projects().join(name).to_str().unwrap()
        );
        assert!(sandbox.projects().join(name).is_dir(), "{name}");
    }
    // No command ran, and nothing but the projects exists.
    assert!(!sandbox.cwd().join("pwned").exists());
    assert!(!sandbox.projects().join("pwned").exists());
    assert_eq!(sandbox.projects_listed().len(), 5);
}

#[test]
fn a_folder_that_is_there_is_refused_untouched() {
    let sandbox = Sandbox::new();
    let existing = sandbox.projects().join("Mine");
    fs::create_dir_all(&existing).unwrap();
    fs::write(existing.join("notes.txt"), "keep me").unwrap();
    let file = sandbox.projects().join("AFile");
    fs::write(&file, "not a folder").unwrap();
    for name in ["Mine", "AFile"] {
        let line = sandbox.refused(name);
        assert!(
            line.starts_with("riwork: already_exists: folder "),
            "{name}: {line}"
        );
    }
    assert_eq!(
        fs::read_to_string(existing.join("notes.txt")).unwrap(),
        "keep me"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "not a folder");
    // It was not made a repository, and no project was registered for it.
    assert!(!existing.join(".git").exists());
    assert!(sandbox.projects_listed().is_empty());
    // Plain `project create` is the one that adopts it, as it always did.
    let adopted = sandbox.ok(&["project", "create", "--name", "Mine", "--no-git", "--json"]);
    assert_eq!(adopted["root"], existing.to_str().unwrap());
}

#[test]
fn a_project_of_that_name_is_refused_whatever_the_case_and_a_repeat_is_refused_too() {
    let sandbox = Sandbox::new();
    // A project of that name that lives somewhere else entirely.
    let elsewhere = sandbox.root.join("code/Taken");
    fs::create_dir_all(&elsewhere).unwrap();
    sandbox.ok(&["project", "add", elsewhere.to_str().unwrap(), "--json"]);
    for name in ["Taken", "taken", "TAKEN"] {
        let line = sandbox.refused(name);
        assert!(
            line.starts_with("riwork: already_exists: project "),
            "{name}: {line}"
        );
        assert!(!sandbox.projects().join(name).exists(), "{name} was made");
    }
    // The answer a phone gets when it asks again after losing the reply.
    sandbox.created("Once", true);
    let line = sandbox.refused("Once");
    assert!(
        line.starts_with("riwork: already_exists: project "),
        "{line}"
    );
    assert_eq!(sandbox.projects_listed().len(), 2);
}

#[test]
fn a_registered_folder_is_never_renamed_by_a_new_project() {
    let sandbox = Sandbox::new();
    let root = sandbox.projects().join("renamed");
    fs::create_dir_all(&root).unwrap();
    sandbox.ok(&[
        "project",
        "add",
        root.to_str().unwrap(),
        "--name",
        "Something Else",
        "--json",
    ]);
    let line = sandbox.refused("renamed");
    assert!(
        line.starts_with("riwork: already_exists: folder "),
        "{line}"
    );
    let listed = sandbox.projects_listed();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["name"], "Something Else");
}

#[test]
fn the_flag_belongs_to_create_and_is_never_read_as_a_path() {
    let sandbox = Sandbox::new();
    // `add` has no such flag; the usage error leaves the working directory as it was.
    let line = sandbox.failure(&["project", "add", "--exclusive"]);
    assert!(line.contains("Usage: riwork project add PATH"), "{line}");
    assert_eq!(sandbox.tree(), ["cwd", "home"]);
    // Without a name or a path there is nothing to create.
    let line = sandbox.failure(&["project", "create", "--exclusive"]);
    assert!(line.contains("Usage: riwork project create"), "{line}");
    assert_eq!(sandbox.tree(), ["cwd", "home"]);
    // With an explicit PATH it is still exclusive, and still its own flag.
    let path = sandbox.root.join("explicit");
    let created = sandbox.ok(&[
        "project",
        "create",
        path.to_str().unwrap(),
        "--no-git",
        "--exclusive",
        "--json",
    ]);
    assert_eq!(created["root"], path.to_str().unwrap());
    let line = sandbox.failure(&[
        "project",
        "create",
        path.to_str().unwrap(),
        "--exclusive",
        "--json",
    ]);
    assert!(line.starts_with("riwork: already_exists: "), "{line}");
}

#[test]
fn the_help_describes_the_flag() {
    let sandbox = Sandbox::new();
    let help = String::from_utf8_lossy(&sandbox.run(&["help"]).stdout).into_owned();
    assert!(help.contains("[--no-git] [--exclusive]"), "{help}");
    assert!(help.contains("already_exists"), "{help}");
}
