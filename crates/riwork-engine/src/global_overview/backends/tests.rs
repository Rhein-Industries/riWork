//! The Backends section from fixed records: lock files, registrations and CLIs in temporary
//! homes, and a system whose processes the tests name. No real process is asked about, and
//! nothing is started.
use super::*;
use serde_json::json;
use std::{cell::RefCell, collections::HashMap, time::SystemTime};

const T: u64 = 1_800_000_000;

fn temp_home(name: &str) -> PathBuf {
    let home =
        std::env::temp_dir().join(format!("riwork-backends-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&home).unwrap();
    home.canonicalize().unwrap()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Set the modification time of `path` itself, a symbolic link included.
fn set_link_time(path: &Path, at: u64) {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let time = libc::timespec {
        tv_sec: at as libc::time_t,
        tv_nsec: 0,
    };
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let result = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            path.as_ptr(),
            [time, time].as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    assert_eq!(result, 0, "utimensat");
}

fn set_file_time(path: &Path, at: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(at))
        .unwrap();
}

fn executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn link(target: &Path, at: &Path) {
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, at).unwrap();
}

/// A system with the processes, GUIs and CLIs a test names; it records each pid asked about.
#[derive(Default)]
struct Fake {
    processes: HashMap<u32, (Option<u64>, Option<PathBuf>)>,
    guis: Vec<RegisteredGui>,
    clis: Vec<(HarnessKind, ProgramLookup)>,
    asked: RefCell<Vec<u32>>,
}

impl System for Fake {
    fn guis(&self) -> Vec<RegisteredGui> {
        self.guis.clone()
    }

    fn process(&self, pid: u32) -> Option<(Option<u64>, Option<PathBuf>)> {
        self.asked.borrow_mut().push(pid);
        self.processes.get(&pid).cloned()
    }

    fn cli(&self, provider: HarnessKind) -> ProgramLookup {
        self.clis
            .iter()
            .find(|(each, _)| *each == provider)
            .map_or(ProgramLookup::Missing, |(_, lookup)| lookup.clone())
    }
}

fn shell(id: &str, harness: Option<&str>, alive: bool, created: u64) -> ShellSession {
    let mut shell: ShellSession = serde_json::from_value(json!({
        "id": id,
        "project_id": null,
        "worktree_id": null,
        "kind": "project",
        "cwd": "/work",
        "command": null,
        "harness": harness,
        "created_at_unix": created
    }))
    .unwrap();
    shell.alive = alive;
    shell
}

fn chat(id: &str, provider: &str, state: &str, created: u64) -> ChatInfo {
    serde_json::from_value(json!({
        "id": id,
        "provider": provider,
        "cwd": "/work",
        "title": id,
        "created_at_unix": created,
        "state": {"state": state},
    }))
    .unwrap()
}

fn binary(version: Option<&str>, replaced_at: Option<u64>) -> Binary {
    Binary {
        version: version.map(str::to_owned),
        replaced_at,
    }
}

fn running(pid: u32, started_at: Option<u64>, replaced_at: Option<u64>) -> Process {
    Process {
        pid,
        started_at,
        binary: Some(binary(None, replaced_at)),
    }
}

// ---- The rule -------------------------------------------------------------------------------

#[test]
fn a_process_is_stale_exactly_when_it_started_before_its_binary_was_replaced() {
    assert_eq!(freshness(Start::At(T - 1), Some(T)), Freshness::Stale);
    assert_eq!(freshness(Start::At(T), Some(T)), Freshness::Current);
    assert_eq!(freshness(Start::At(T + 1), Some(T)), Freshness::Current);
    // A lower bound proves current, never stale.
    assert_eq!(freshness(Start::NotBefore(T), Some(T)), Freshness::Current);
    assert_eq!(
        freshness(Start::NotBefore(T + 9), Some(T)),
        Freshness::Current
    );
    assert_eq!(
        freshness(Start::NotBefore(T - 1), Some(T)),
        Freshness::Unknown
    );
    // Either time unknown: unknown, never a guess.
    assert_eq!(freshness(Start::Unknown, Some(T)), Freshness::Unknown);
    assert_eq!(freshness(Start::At(T), None), Freshness::Unknown);
    assert_eq!(freshness(Start::NotBefore(T), None), Freshness::Unknown);
}

// ---- Reading --------------------------------------------------------------------------------

#[test]
fn the_chat_host_is_the_process_its_lock_names_and_nothing_else() {
    let home = temp_home("host");
    let begun = unix_now();
    let host = home.join("bin/riwork");
    executable(&host, "host");
    let mut fake = Fake::default();
    let read = |fake: &Fake| read_with(&home, &[], &BTreeMap::new(), fake).chat_host;

    // No lock, as on a Mac where no chat was ever opened.
    assert_eq!(read(&fake), HostSample::Stopped);
    // A host that stopped empties its lock.
    let lock = home.join("run/chat.lock");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, "").unwrap();
    assert_eq!(read(&fake), HostSample::Stopped);
    // A lock that names no process is not read as one.
    fs::write(&lock, "not a pid\n").unwrap();
    assert!(matches!(read(&fake), HostSample::Unreadable(why) if why.contains("names no process")));

    // A host that ended without emptying its lock: its pid runs nothing now.
    fs::write(&lock, "4242\n").unwrap();
    set_file_time(&lock, T);
    assert_eq!(read(&fake), HostSample::Stopped);
    // Another process that took the pid after the lock was written is not the host.
    fake.processes
        .insert(4242, (Some(T + 60), Some(host.clone())));
    assert_eq!(read(&fake), HostSample::Stopped);

    // The host that wrote it.
    fake.processes
        .insert(4242, (Some(T - 1), Some(host.clone())));
    let HostSample::Running(process) = read(&fake) else {
        panic!("running")
    };
    assert_eq!((process.pid, process.started_at), (4242, Some(T - 1)));
    let replaced = process.binary.unwrap().replaced_at.unwrap();
    assert!(
        replaced + 5 >= begun,
        "the host's executable is read from disk: {replaced}"
    );
    // A process the system names no executable or start for is still the host, unknown in both.
    fake.processes.insert(4242, (None, None));
    assert_eq!(
        read(&fake),
        HostSample::Running(Process {
            pid: 4242,
            started_at: None,
            binary: None
        })
    );
    // The socket is never touched, and nothing is written.
    assert!(!home.join("run/chat.sock").exists());
    assert_eq!(fs::read_to_string(&lock).unwrap(), "4242\n");
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn guis_of_this_home_and_each_clis_version_on_disk_are_read_without_running_anything() {
    let home = temp_home("read");
    let other = temp_home("other");
    let root = temp_home("clis");
    let started = unix_now();

    // Claude's installer: a link to a file named for its version.
    let claude = root.join("share/claude/versions/2.1.301");
    executable(&claude, "claude");
    link(&claude, &root.join("bin/claude"));
    // npm: a link to the package's script, whose package.json says the version.
    let package = root.join("lib/node_modules/@openai/codex");
    executable(&package.join("bin/codex.js"), "#!/usr/bin/env node");
    fs::write(
        package.join("package.json"),
        r#"{"name":"@openai/codex","version":"0.47.0"}"#,
    )
    .unwrap();
    link(
        Path::new("../lib/node_modules/@openai/codex/bin/codex.js"),
        &root.join("bin/codex"),
    );
    // Grok's installer: a file named for the program and its version.
    let grok = root.join("grok/bin/grok-1.0.45-macos-aarch64");
    executable(&grok, "grok");
    link(&grok, &root.join("grok-bin/grok"));

    let gui = root.join("RiWork.app/Contents/MacOS/riwork");
    executable(&gui, "gui");
    let mut fake = Fake::default();
    fake.guis = vec![
        RegisteredGui {
            pid: 10,
            started_unix: Some(T),
            executable: Some(gui.clone()),
            state_home: home.clone(),
            windows: 3,
        },
        // A fixture GUI of another home is not this dashboard's.
        RegisteredGui {
            pid: 11,
            started_unix: Some(T),
            executable: Some(gui.clone()),
            state_home: other.clone(),
            windows: 1,
        },
    ];
    fake.clis = Vec::from([
        (
            HarnessKind::Claude,
            ProgramLookup::Found(root.join("bin/claude")),
        ),
        (
            HarnessKind::Codex,
            ProgramLookup::Found(root.join("bin/codex")),
        ),
        (
            HarnessKind::Grok,
            ProgramLookup::Found(root.join("grok-bin/grok")),
        ),
        (HarnessKind::Muse, ProgramLookup::Unknown),
    ]);
    fake.processes.insert(500, (Some(T + 5), None));
    fake.processes.insert(501, (Some(T + 6), None));
    let shells = vec![
        shell("claude-live", Some("claude"), true, T),
        shell("plain-live", None, true, T),
        shell("codex-gone", Some("codex"), false, T),
        shell("codex-unsampled", Some("codex"), true, T),
    ];
    let processes = BTreeMap::from([
        ("claude-live".to_owned(), 500),
        ("plain-live".to_owned(), 501),
        ("codex-gone".to_owned(), 502),
    ]);

    let sample = read_with(&home, &shells, &processes, &fake);
    assert_eq!(sample.guis.len(), 1);
    assert_eq!(sample.guis[0].process.pid, 10);
    assert_eq!(sample.guis[0].windows, 3);
    let versions: Vec<(HarnessKind, Option<String>)> = sample
        .providers
        .iter()
        .map(|(provider, cli)| {
            (
                *provider,
                match cli {
                    CliSample::Found(binary) => binary.version.clone(),
                    _ => None,
                },
            )
        })
        .collect();
    assert_eq!(
        versions,
        [
            (HarnessKind::Codex, Some("0.47.0".to_owned())),
            (HarnessKind::Claude, Some("2.1.301".to_owned())),
            (HarnessKind::Muse, None),
            (HarnessKind::Grok, Some("1.0.45".to_owned())),
        ]
    );
    assert_eq!(sample.providers[2].1, CliSample::Unknown);
    for (provider, cli) in &sample.providers {
        if let CliSample::Found(binary) = cli {
            assert!(
                binary.replaced_at.is_some_and(|at| at + 5 >= started),
                "{provider:?}: {binary:?}"
            );
        }
    }
    // Only live harness shells' panes are looked up: not a plain shell, not a dead one.
    assert_eq!(
        sample.shell_starts,
        BTreeMap::from([("claude-live".to_owned(), T + 5)])
    );
    assert_eq!(*fake.asked.borrow(), [500]);
    for directory in [home, other, root] {
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn replacing_a_link_on_the_way_counts_as_replacing_the_binary() {
    let root = temp_home("links");
    let file = root.join("versions/1.2.3");
    executable(&file, "x");
    let far = unix_now() + 86_400;
    set_file_time(&file, far);
    assert_eq!(
        replaced_at(&file),
        Some(far),
        "a file written later than its inode changed"
    );
    // bin/tool -> current -> versions/1.2.3: the newest link wins.
    link(Path::new("versions/1.2.3"), &root.join("current"));
    link(Path::new("../current"), &root.join("bin/tool"));
    set_link_time(&root.join("current"), far + 100);
    set_link_time(&root.join("bin/tool"), far + 50);
    assert_eq!(replaced_at(&root.join("bin/tool")), Some(far + 100));
    assert_eq!(
        version_named(&root.join("bin/tool"), "tool"),
        Some("1.2.3".to_owned())
    );
    // A dangling link is a file that cannot be read.
    link(Path::new("versions/9.9.9"), &root.join("dangling"));
    assert_eq!(replaced_at(&root.join("dangling")), None);
    assert_eq!(replaced_at(&root.join("missing")), None);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn switching_a_directory_link_to_a_preinstalled_binary_makes_running_agents_stale() {
    let root = temp_home("directory-links");
    let installed = unix_now() + 86_400;
    for version in ["1.2.3", "2.0.0"] {
        let file = root.join(format!("Cellar/tool/{version}/bin/tool"));
        executable(&file, "tool");
        set_file_time(&file, installed);
    }
    let current = root.join("opt/tool");
    let program = root.join("bin/tool");
    link(Path::new("../Cellar/tool/1.2.3"), &current);
    link(Path::new("../opt/tool/bin/tool"), &program);
    set_link_time(&current, installed);
    set_link_time(&program, installed);
    let started = Start::At(installed + 100);
    assert_eq!(
        freshness(started, replaced_at(&program)),
        Freshness::Current
    );

    fs::remove_file(&current).unwrap();
    link(Path::new("../Cellar/tool/2.0.0"), &current);
    set_link_time(&current, installed + 200);
    assert_eq!(replaced_at(&program), Some(installed + 200));
    assert_eq!(freshness(started, replaced_at(&program)), Freshness::Stale);

    // A missing target or a cycle cannot establish freshness.
    fs::remove_file(&current).unwrap();
    assert_eq!(replaced_at(&program), None);
    link(Path::new("tool"), &current);
    assert_eq!(replaced_at(&program), None);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_version_is_read_only_where_the_installed_location_names_it() {
    for (text, version) in [
        ("2.1.301", Some("2.1.301")),
        ("1.0.45-macos-aarch64", Some("1.0.45")),
        ("0.46.0_1", Some("0.46.0")),
        ("v22.3.0", Some("22.3.0")),
        ("1.2.", Some("1.2")),
        ("12", None),
        ("aarch64-apple-darwin", None),
        ("3.12abc", None),
        ("", None),
    ] {
        assert_eq!(leading_version(text).as_deref(), version, "{text:?}");
    }
    let root = temp_home("versions");
    // Homebrew: Cellar/<program>/<version>/bin/<program>.
    let cellar = root.join("Cellar/muse/0.9.2/bin/muse");
    executable(&cellar, "muse");
    assert_eq!(version_named(&cellar, "muse"), Some("0.9.2".to_owned()));
    // A version manager's directory names its runtime, not the CLI: no version.
    let nvm = root.join("nvm/versions/node/v22.3.0/bin/claude");
    executable(&nvm, "claude");
    assert_eq!(version_named(&nvm, "claude"), None);
    // A plain binary names none.
    let plain = root.join("bin/codex");
    executable(&plain, "codex");
    assert_eq!(version_named(&plain, "codex"), None);
    // Nor does a package.json outside the npm package the script is in.
    let outside = root.join("project/node_modules/.bin/codex");
    executable(&outside, "codex");
    fs::write(root.join("project/package.json"), r#"{"version":"7.7.7"}"#).unwrap();
    assert_eq!(version_named(&outside, "codex"), None);
    fs::remove_dir_all(root).unwrap();
}

// ---- Arranging ------------------------------------------------------------------------------

fn sample(host: HostSample) -> BackendSample {
    BackendSample {
        guis: Vec::new(),
        chat_host: host,
        providers: vec![
            (HarnessKind::Codex, CliSample::Unknown),
            (
                HarnessKind::Claude,
                CliSample::Found(binary(Some("2.1.301"), Some(T))),
            ),
            (
                HarnessKind::Muse,
                CliSample::Found(binary(Some("0.9.2"), Some(T - 900))),
            ),
            (HarnessKind::Grok, CliSample::Missing),
        ],
        shell_starts: BTreeMap::from([
            ("claude-old".to_owned(), T - 100),
            ("claude-new".to_owned(), T + 10),
            ("codex".to_owned(), T - 3000),
        ]),
    }
}

fn shells() -> Vec<ShellSession> {
    vec![
        shell("claude-old", Some("claude"), true, T - 200),
        shell("claude-new", Some("claude"), true, T + 5),
        // No pane start: its creation is a lower bound.
        shell("claude-unsampled-old", Some("claude"), true, T - 500),
        shell("claude-unsampled-new", Some("claude"), true, T + 1),
        shell("claude-dead", Some("claude"), false, T - 9000),
        shell("codex", Some("codex"), true, T - 3000),
        shell("plain", None, true, T - 9000),
    ]
}

fn chats() -> Vec<ChatInfo> {
    vec![
        chat("claude-chat", "claude", "idle", T - 1000),
        chat("claude-chat-stopped", "claude", "stopped", T - 1000),
        chat("muse-chat", "muse", "running", T - 2000),
    ]
}

#[test]
fn each_provider_counts_its_running_agents_and_which_predate_its_cli() {
    let host = HostSample::Running(running(77, Some(T - 50), Some(T - 60)));
    let shells = shells();
    let chats = chats();
    let section = arrange(&sample(host), &shells, Some(&chats));
    let rows: Vec<_> = section
        .providers
        .iter()
        .map(|row| {
            (
                row.provider,
                row.shells,
                row.chats,
                row.oldest_start,
                row.stale,
                row.unknown,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            // Its CLI is not where this process looked: every agent of it is unknown.
            (HarnessKind::Codex, 1, Some(0), Some(T - 3000), 0, 1),
            // claude-old predates the CLI; claude-new and the young unsampled shell are
            // current; the old unsampled shell and the chat (whose host started before the
            // CLI was replaced) cannot be told. The dead shell and the stopped chat run nothing.
            (HarnessKind::Claude, 4, Some(1), Some(T - 100), 1, 2),
            // Its chat started under a host younger than its CLI: current.
            (HarnessKind::Muse, 0, Some(1), None, 0, 0),
            // Grok: not installed and not running, so no row.
        ]
    );
    assert_eq!(
        section.providers[1].cli,
        CliState::Found {
            version: Some("2.1.301".into()),
            replaced_at: Some(T)
        }
    );
    assert_eq!(section.providers[1].running(), Some(5));
}

#[test]
fn chat_agents_are_none_without_a_host_and_unknown_until_the_chats_are_read() {
    let shells = shells();
    let chats = chats();
    // No host: no chat agent runs, whatever the saved chats say.
    let stopped = arrange(&sample(HostSample::Stopped), &shells, Some(&chats));
    assert!(stopped.providers.iter().all(|row| row.chats == Some(0)));
    assert_eq!(stopped.chat_host, ChatHostOverview::Stopped);
    // A host whose chats were not read yet: the chat figures are unknown, not zero.
    let host = HostSample::Running(running(77, Some(T + 100), Some(T + 90)));
    let unread = arrange(&sample(host), &shells, None);
    assert!(unread.providers.iter().all(|row| row.chats.is_none()));
    assert_eq!(unread.providers[1].running(), None);
    // A lock that could not be read: the chats are unknown and the reason is kept.
    let unreadable = arrange(
        &sample(HostSample::Unreadable("run/chat.lock: denied".into())),
        &shells,
        Some(&chats),
    );
    assert!(unreadable.providers.iter().all(|row| row.chats.is_none()));
    assert_eq!(
        unreadable.chat_host,
        ChatHostOverview::Unreadable("run/chat.lock: denied".into())
    );
    assert_eq!(unreadable.providers[1].shells, 4);
}

#[test]
fn the_app_and_the_chat_host_say_whether_they_run_the_build_on_disk() {
    let mut read = sample(HostSample::Running(running(77, Some(T - 50), Some(T))));
    read.guis = vec![
        Gui {
            process: running(10, Some(T + 10), Some(T)),
            windows: 2,
        },
        Gui {
            process: running(11, Some(T - 10), Some(T)),
            windows: 1,
        },
    ];
    let section = arrange(&read, &[], Some(&[]));
    assert_eq!(
        section.app,
        AppOverview {
            running: 2,
            windows: 3,
            started_at: Some(T - 10),
            build_at: Some(T),
            // One of them started before its build was replaced.
            freshness: Some(Freshness::Stale),
        }
    );
    assert_eq!(
        section.chat_host,
        ChatHostOverview::Running {
            pid: 77,
            started_at: Some(T - 50),
            build_at: Some(T),
            freshness: Freshness::Stale,
        }
    );
    // A host the system names no start for is unknown, not current.
    read.chat_host = HostSample::Running(running(77, None, Some(T)));
    assert!(matches!(
        arrange(&read, &[], Some(&[])).chat_host,
        ChatHostOverview::Running {
            freshness: Freshness::Unknown,
            ..
        }
    ));
    // Read from outside the app with no GUI running.
    read.guis.clear();
    assert_eq!(
        arrange(&read, &[], Some(&[])).app,
        AppOverview {
            running: 0,
            windows: 0,
            started_at: None,
            build_at: None,
            freshness: None,
        }
    );
}

#[test]
fn a_mac_where_no_provider_was_used_lists_only_what_is_installed() {
    let read = BackendSample {
        guis: Vec::new(),
        chat_host: HostSample::Stopped,
        providers: vec![
            (HarnessKind::Codex, CliSample::Missing),
            (HarnessKind::Claude, CliSample::Found(binary(None, None))),
            (HarnessKind::Muse, CliSample::Unknown),
            (HarnessKind::Grok, CliSample::Missing),
        ],
        shell_starts: BTreeMap::new(),
    };
    let section = arrange(&read, &[], Some(&[]));
    assert_eq!(
        section.providers,
        [ProviderOverview {
            provider: HarnessKind::Claude,
            cli: CliState::Found {
                version: None,
                replaced_at: None
            },
            shells: 0,
            chats: Some(0),
            oldest_start: None,
            stale: 0,
            unknown: 0,
        }]
    );
}

// ---- Words ----------------------------------------------------------------------------------

#[test]
fn the_rows_say_what_runs_and_what_a_restart_would_apply() {
    let host = HostSample::Running(running(4242, Some(T - 50), Some(T)));
    let mut read = sample(host);
    read.guis = vec![Gui {
        process: running(10, Some(T + 10), Some(T)),
        windows: 3,
    }];
    let shells = shells();
    let chats = chats();
    let now = T + 3600;
    let lines = lines(&arrange(&read, &shells, Some(&chats)), now);
    let keys: Vec<&str> = lines.iter().map(|line| line.key).collect();
    assert_eq!(keys, ["app", "chat-host", "codex", "claude", "muse"]);
    let at = |at: u64| local_time(at, now);

    assert_eq!(lines[0].title, "RiWork");
    assert_eq!(
        lines[0].detail,
        format!(
            "running since {} · 3 windows · runs the {} build",
            at(T + 10),
            at(T)
        )
    );
    assert_eq!(lines[0].tone, Tone::Current);
    assert_eq!(
        lines[1].detail,
        format!(
            "running since {} · pid 4242 · restart to apply the {} build",
            at(T - 50),
            at(T)
        )
    );
    assert_eq!(lines[1].tone, Tone::Stale);
    assert_eq!(
        lines[2].detail,
        format!(
            "not found where looked · 1 running, oldest {} · 1 unknown",
            at(T - 3000)
        )
    );
    assert_eq!(lines[2].tone, Tone::Unknown);
    assert_eq!(
        lines[3].detail,
        format!(
            "2.1.301 on disk · 5 running, oldest {} · 1 predates the installed binary · 2 unknown",
            at(T - 100)
        )
    );
    assert_eq!(lines[3].tone, Tone::Stale);
    // Its chat started under a host younger than the CLI.
    assert_eq!(lines[4].detail, "0.9.2 on disk · 1 running");
    assert_eq!(lines[4].tone, Tone::Current);

    // Stopped host, nothing running, version unknown.
    let quiet = BackendsOverview {
        app: AppOverview {
            running: 0,
            windows: 0,
            started_at: None,
            build_at: None,
            freshness: None,
        },
        chat_host: ChatHostOverview::Stopped,
        providers: vec![ProviderOverview {
            provider: HarnessKind::Muse,
            cli: CliState::Found {
                version: None,
                replaced_at: Some(T),
            },
            shells: 0,
            chats: Some(0),
            oldest_start: None,
            stale: 0,
            unknown: 0,
        }],
    };
    let lines = super::lines(&quiet, now);
    assert_eq!(
        (lines[0].detail.as_str(), lines[0].tone),
        ("not running", Tone::Idle)
    );
    assert_eq!(
        (lines[1].detail.as_str(), lines[1].tone),
        ("not running", Tone::Idle)
    );
    assert_eq!(
        lines[2].detail,
        format!("version unknown, installed {} · none running", at(T))
    );
    assert_eq!(lines[2].tone, Tone::Idle);
}

#[test]
fn local_times_name_the_day_only_when_it_is_not_today() {
    use chrono::{Local, TimeZone};
    let noon = Local
        .with_ymd_and_hms(2026, 10, 16, 12, 0, 0)
        .single()
        .unwrap()
        .timestamp() as u64;
    assert_eq!(local_time(noon - 3600, noon), "today 11:00");
    assert_eq!(local_time(noon - 86_400, noon), "yesterday 12:00");
    assert_eq!(local_time(noon - 2 * 86_400, noon), "Oct 14 12:00");
}
