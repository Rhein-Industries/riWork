//! Fixed executable fixtures for the public output/desktop watch seams.
//! Never discovers, invokes or wraps tmux. Keep fixture receipts on disk;
//! there is no server, signalling or cleanup in this fixture.
use super::*;
use std::os::unix::fs::PermissionsExt;

const ID: &str = "11111111-1111-4111-8111-111111111111";

struct Fixture {
    root: PathBuf,
    manager: SessionManager,
}

impl Fixture {
    fn new() -> Self {
        let root = env::temp_dir().join(format!("rw-poll-{}", Uuid::new_v4().simple()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.join("home");
        fs::create_dir(&home).unwrap();
        fs::write(
            home.join("sessions.json"),
            serde_json::json!({"sessions": [{
                "id": ID, "project_id": null, "worktree_id": null,
                "kind": "project", "cwd": "/work", "command": null,
                "created_at_unix": 1
            }]})
            .to_string(),
        )
        .unwrap();
        let tmux = root.join("fixed-command");
        fs::write(
            &tmux,
            r##"#!/bin/sh
root=${0%/*}
printf '%s\n' "$*" >> "$root/calls"
capture=0
listing=0
marker=''
for arg do
    case "$arg" in
        -C|-CC|attach|attach-session|kill-*|start-server|new-session|send-keys|resize-*)
            printf '%s\n' "$*" >> "$root/forbidden"
            exit 97 ;;
        capture-pane) capture=1 ;;
        list-sessions) listing=1 ;;
        riwork-screen-*) marker=${arg%%:*}: ;;
    esac
done
IFS= read -r mode < "$root/mode"
if [ "$listing" = 1 ]; then
    if [ "$mode" != exited-after-first ]; then
        printf '11111111-1111-4111-8111-111111111111\t1\n'
    fi
    exit 0
fi
if [ "$capture" != 1 ]; then
    printf '%s\n' "$*" >> "$root/forbidden"
    exit 98
fi
n=0
if [ "$mode" != same ]; then
    IFS= read -r n < "$root/count"
    n=$((n + 1))
    printf '%s\n' "$n" > "$root/count"
fi
if [ "$n" -gt 1 ]; then
    case "$mode" in
        error-after-first|exited-after-first)
            printf 'synthetic capture failure\n' >&2
            exit 1 ;;
    esac
fi
if [ "$mode" = change ] && [ "$n" -gt 1 ]; then
    printf 'later\n\n'
else
    printf 'same\n\n'
fi
if [ -n "$marker" ]; then
    printf '%s2|40|2|0|0|0|0\n' "$marker"
fi
"##,
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self {
            manager: SessionManager {
                home,
                tmux,
                socket_name: "synthetic-only".into(),
            },
            root,
        };
        fixture.mode("same");
        fixture
    }

    fn mode(&self, mode: &str) {
        fs::write(self.root.join("mode"), format!("{mode}\n")).unwrap();
        fs::write(self.root.join("count"), "0\n").unwrap();
    }

    fn read(&self, hash: Option<&str>, wait: Duration) -> Result<OutputRead, String> {
        self.manager.read_output(
            ID,
            &OutputQuery {
                lines: 100,
                styled: false,
                if_changed: hash,
                wait,
            },
        )
    }

    fn hash(&self) -> String {
        let OutputRead::Changed { hash, .. } = self.read(None, Duration::ZERO).unwrap() else {
            panic!("expected initial capture");
        };
        hash
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn assert_no_control(&self) {
        let calls = self.calls();
        assert!(
            !self.root.join("forbidden").exists(),
            "{calls:?}; receipts: {}",
            self.root.display()
        );
        for call in calls {
            let args: Vec<_> = call.split_whitespace().collect();
            assert!(
                !args.iter().any(|arg| matches!(
                    *arg,
                    "-C" | "-CC" | "attach" | "attach-session"
                )),
                "{call}"
            );
            assert!(
                args.contains(&"capture-pane") || args.contains(&"list-sessions"),
                "{call}"
            );
        }
    }
}

#[test]
fn shared_desktop_and_direct_watch_entrypoints_start_no_commands() {
    let fixture = Fixture::new();
    for _ in 0..8 {
        assert!(fixture.manager.watch(ID).is_none());
        assert!(fixture.manager.watch_shell(ID).is_none());
        // Even an accidental direct caller supplying unsafe arguments is inert.
        for args in [["-C", "attach-session"], ["-CC", "attach"]] {
            let mut command = Command::new(&fixture.manager.tmux);
            command.args(args);
            assert!(watch::PaneWatch::start(command, ID).is_none());
        }
    }
    assert!(fixture.manager.watch_shell("invalid").is_none());
    assert!(fixture.calls().is_empty()); // No version probe or rejected attach either.
    fixture.assert_no_control();
}

#[test]
#[ignore = "slow: drives a fake tmux script with real polling waits"]
fn capture_error_and_exited_shell_after_a_poll_never_attempt_control() {
    for (mode, expected) in [
        ("error-after-first", "synthetic capture failure"),
        ("exited-after-first", "has exited"),
    ] {
        let fixture = Fixture::new();
        let hash = fixture.hash();
        fixture.mode(mode);
        let error = fixture.read(Some(&hash), MAX_OUTPUT_WAIT).unwrap_err();
        assert!(error.contains(expected), "{error}");
        fixture.assert_no_control();
    }
}
