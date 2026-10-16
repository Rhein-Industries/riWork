//! `riwork project memo show|set ID` from a shell: what `show` writes to a pipe or a file is the
//! memo byte for byte, so `show > file` and `set < file` round-trip it, even a full 32 KiB memo
//! that does not end in a line break. Every child runs with a throwaway RIWORK_HOME and HOME.

use riwork_core::{project_memo::MAX_BYTES, store::Store};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};
use uuid::Uuid;

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-memo-cli-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("project")).unwrap();
        Self { root }
    }

    fn store(&self) -> Store {
        Store::open(self.root.join("state")).unwrap()
    }

    fn run(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_riwork-cli"))
            .args(args)
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("RIWORK_HOME", self.root.join("state"))
            .env("RIWORK_RUNTIME_DIR", self.root.join("runtime"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Made by this test for itself; it holds no user files.
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn show_prints_the_memo_exactly_and_set_takes_it_back() {
    let sandbox = Sandbox::new();
    let store = sandbox.store();
    let id = store
        .add_project(sandbox.root.join("project"), Some("Notes"))
        .unwrap()
        .id;
    for memo in [
        "x".repeat(MAX_BYTES),
        "# Plan\n- ship".to_owned(),
        "ends in a line break\n".to_owned(),
    ] {
        store.set_project_memo(&id, &memo).unwrap();
        let shown = sandbox.run(&["project", "memo", "show", &id], b"").stdout;
        assert!(
            shown == memo.as_bytes(),
            "{} bytes shown of a {}-byte memo",
            shown.len(),
            memo.len()
        );

        store.set_project_memo(&id, "replaced").unwrap();
        sandbox.run(&["project", "memo", "set", &id], &shown);
        assert_eq!(store.snapshot().unwrap().project_memo(&id).text, memo);
    }
}
