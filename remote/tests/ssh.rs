//! No network service or credentials: a fixture `ssh` runs the generated remote
//! command locally. Everything after that boundary is the real binary/RPC/PTY.
#![cfg(unix)]
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use riwork_remote::{
    client::{HostRecord, HostsFile},
    client_daemon,
    config::Storage,
    ssh::Route,
};
use serde_json::{Value, json};
use std::{
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

const REMOTE: &str = env!("CARGO_BIN_EXE_somtus-remote");

mod client_support;
use client_support::OtherBuild;

struct Rig {
    dir: tempfile::TempDir,
    daemon: Option<Child>,
    tmux: PathBuf,
    tmux_socket: PathBuf,
    tmux_pid: u32,
    pane_pid: u32,
    client: PathBuf,
    host: String,
    shell: String,
    route: String,
    socket: PathBuf,
    path: std::ffi::OsString,
}
impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = Command::new(&self.tmux)
            .arg("-S")
            .arg(&self.tmux_socket)
            .arg("kill-server")
            .output();
        // Some tmux builds wait for a raw-mode pane even after kill-server.
        // Kill only the fixture pane if it is still owned by this exact server.
        let parent = Command::new("ps")
            .args(["-p", &self.pane_pid.to_string(), "-o", "ppid="])
            .output();
        if parent
            .ok()
            .and_then(|p| String::from_utf8(p.stdout).ok())
            .is_some_and(|p| p.trim() == self.tmux_pid.to_string())
        {
            unsafe {
                libc::kill(self.pane_pid as i32, libc::SIGKILL);
            }
        }
    }
}
fn script(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
impl Rig {
    async fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("ssh")
            .tempdir_in("/tmp")
            .unwrap();
        let tmux = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|p| p.join("tmux"))
            .find(|p| p.is_file())
            .expect("fixture requires tmux");
        let tmux_socket = dir.path().join("tmux");
        let shell = uuid::Uuid::new_v4().to_string();
        let mut launch = Command::new(&tmux);
        // Test runners can inherit ignored signals. The isolated server and
        // pane must retain normal shutdown behavior after kill-server.
        unsafe {
            launch.pre_exec(|| {
                libc::signal(libc::SIGHUP, libc::SIG_DFL);
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
                Ok(())
            });
        }
        let started = launch
            .args(["-S"])
            .arg(&tmux_socket)
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                &shell,
                "stty raw -echo; printf 'fixture-ready'; exec cat",
            ])
            .output()
            .unwrap();
        assert!(
            started.status.success(),
            "{}",
            String::from_utf8_lossy(&started.stderr)
        );
        let pids = Command::new(&tmux)
            .arg("-S")
            .arg(&tmux_socket)
            .args(["display-message", "-p", "#{pid} #{pane_pid}"])
            .output()
            .unwrap();
        let pids: Vec<u32> = String::from_utf8(pids.stdout)
            .unwrap()
            .split_whitespace()
            .map(|p| p.parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 2);
        let host = uuid::Uuid::new_v4().to_string();
        std::fs::write(dir.path().join("identity"), &host).unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        script(
            &bin.join("ssh"),
            "#!/bin/sh\nfor arg do last=$arg; done\nexec /bin/sh -c \"$last\"\n",
        );
        let cli = dir.path().join("cli");
        script(
            &cli,
            &format!(
                r#"#!/usr/bin/env python3
import sys,json,pathlib,os
root=pathlib.Path({root:?})
args=sys.argv[1:]
with (root/'calls').open('a') as f: f.write(' '.join(args)+'\n')
if args[0]=='capabilities':
 print(json.dumps({{'v':1,'verifies_shell':True,'shell_attach_exec':True,'chat':True,'native':{{'v':1,'methods':['Hosts.local','Store.renameProject']}}}}))
elif args[:2]==['native','invoke']:
 print(json.dumps({{'v':1,'value':{{'hostId':(root/'identity').read_text()}}}}))
elif args[:2]==['chat','list']: print('[]')
elif args[:2]==['shell','attach']:
 os.execv({tmux:?},[{tmux:?},'-S',{socket:?},'attach-session','-t',args[2]])
elif args[:2]==['shell','list']: print(json.dumps([{{'id':{shell:?},'alive':True}}]))
else: print('[]')
"#,
                root = dir.path().to_string_lossy(),
                tmux = tmux.to_string_lossy(),
                socket = tmux_socket.to_string_lossy()
            ),
        );
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let client = dir.path().join("client");
        let host_home = dir.path().join("host");
        let output = tokio::process::Command::new(REMOTE)
            .env("RIWORK_HOME", &client)
            .env("PATH", &path)
            .args([
                "hosts",
                "add-ssh",
                "--target",
                "fixture",
                "--remote-bin",
                REMOTE,
                "--somtus",
            ])
            .arg(&cli)
            .arg("--home")
            .arg(host_home)
            .args(["--label", "Fixture", "--json"])
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let registered: Value = serde_json::from_slice(&output.stdout).unwrap();
        let route = registered["id"].as_str().unwrap().to_owned();
        assert_eq!(registered["host_id"], host);
        assert_eq!(registered["transport"], "ssh");
        let socket =
            client_daemon::socket_path(&Storage::at(client.clone()).unwrap(), &route).unwrap();
        let mut rig = Self {
            dir,
            daemon: None,
            tmux,
            tmux_socket,
            tmux_pid: pids[0],
            pane_pid: pids[1],
            client,
            host,
            shell,
            route,
            socket,
            path,
        };
        rig.start().await;
        rig
    }
    async fn start(&mut self) {
        self.daemon = Some(
            Command::new(REMOTE)
                .env("RIWORK_HOME", &self.client)
                .env("PATH", &self.path)
                .args([
                    "client",
                    "serve",
                    "--desktop",
                    &self.route,
                    "--idle-seconds",
                    "30",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        client_support::wait_for_daemon(&self.socket).await;
    }
    async fn stop(&mut self) {
        client_daemon::shutdown(&Storage::at(self.client.clone()).unwrap(), &self.route)
            .await
            .unwrap();
        let mut child = self.daemon.take().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while child.try_wait().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn call(&self, method: &str, params: Value) -> Value {
        let reply = client_daemon::daemon_call(&self.socket, method, params, 5000)
            .await
            .unwrap();
        assert_eq!(reply["ok"], true, "{reply}");
        reply["result"].clone()
    }
}

/// The status line once it names a cause.
async fn status_with_cause(socket: &std::path::Path, within: Duration) -> Value {
    tokio::time::timeout(within, async {
        loop {
            if let Ok(status) = client_daemon::daemon_status(socket).await
                && status.get("cause").is_some()
            {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the link names a cause")
}

/// A fixture `ssh` that runs `body`, noting every attempt, and a client home of its
/// own: for the ways a dial ends. It carries no tmux, unlike `Rig`.
struct Shim {
    dir: tempfile::TempDir,
    client: PathBuf,
    path: std::ffi::OsString,
}
/// Goes on to the generated remote command, as a real login does.
const RUN_REMOTE: &str = "for arg do last=$arg; done\nexec /bin/sh -c \"$last\"";
/// `RUN_REMOTE`, noting the target it was given and running what `during_dial` left. A
/// target whose name starts with `unreachable` is treated as one that cannot be reached.
const DIALS: &str = r#"for arg do target=$last; last=$arg; done
root=$(dirname "$0")/..
echo "$target" >> "$root/targets"
if [ -x "$root/during-dial" ]; then "$root/during-dial"; fi
case "$target" in unreachable*) echo "ssh: connect to host $target port 22: Connection refused" >&2; exit 255;; esac
exec /bin/sh -c "$last""#;
/// Arguments of mixed kinds, for `Shim::hosts`.
macro_rules! args {
    ($($arg:expr),* $(,)?) => { &[$(std::ffi::OsStr::new(&$arg)),*] };
}
impl Shim {
    fn new(body: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("ssh")
            .tempdir_in("/tmp")
            .unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        script(
            &bin.join("ssh"),
            &format!(
                "#!/bin/sh\necho attempt >> '{}'\n{body}\n",
                dir.path().join("attempts").display()
            ),
        );
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let client = dir.path().join("client");
        std::fs::create_dir(&client).unwrap();
        Self { client, dir, path }
    }
    /// A fixture CLI that answers `Hosts.local` with `identity`.
    fn cli(&self, identity: &str) -> PathBuf {
        let cli = self.dir.path().join("cli");
        script(
            &cli,
            &format!(
                r#"#!/usr/bin/env python3
import sys,json
if sys.argv[1]=='capabilities': print(json.dumps({{'v':1,'native':{{'v':1,'methods':['Hosts.local']}}}}))
elif sys.argv[1:3]==['native','invoke']: print(json.dumps({{'v':1,'value':{{'hostId':{identity:?}}}}}))
else: print('[]')
"#
            ),
        );
        cli
    }
    async fn add_ssh(&self, options: &[&std::ffi::OsStr]) -> std::process::Output {
        tokio::process::Command::new(REMOTE)
            .env("RIWORK_HOME", &self.client)
            .env("PATH", &self.path)
            .args([
                "hosts", "add-ssh", "--target", "fixture", "--label", "Fixture",
            ])
            .args(options)
            .stdin(Stdio::null())
            .output()
            .await
            .unwrap()
    }
    fn registered(&self) -> usize {
        HostsFile::load(&Storage::at(self.client.clone()).unwrap())
            .unwrap()
            .hosts
            .len()
    }
    fn attempts(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("attempts"))
            .unwrap_or_default()
            .lines()
            .count()
    }
    /// The binary, with `args`, on this fixture's client home.
    async fn remote(&self, args: &[&std::ffi::OsStr]) -> std::process::Output {
        tokio::process::Command::new(REMOTE)
            .env("RIWORK_HOME", &self.client)
            .env("PATH", &self.path)
            .env_remove("SOMTUS_CLI")
            .env_remove("RIWORK_CLI")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .await
            .unwrap()
    }
    /// `hosts ARGS`.
    async fn hosts(&self, args: &[&std::ffi::OsStr]) -> std::process::Output {
        let args: Vec<_> = std::iter::once(std::ffi::OsStr::new("hosts"))
            .chain(args.iter().copied())
            .collect();
        self.remote(&args).await
    }
    /// A route to `home` through `target`, registered the way a person does it.
    async fn added(&self, target: &str, cli: &std::path::Path, home: &std::path::Path) -> Value {
        let out = self
            .hosts(args![
                "add-ssh",
                "--target",
                target,
                "--remote-bin",
                REMOTE,
                "--somtus",
                cli,
                "--home",
                home,
                "--label",
                "Studio",
                "--json",
            ])
            .await;
        assert!(out.status.success(), "{}", text(&out.stderr));
        one_line(&out.stdout)
    }
    /// An installation on the far side of the fixture: a home with an identity saved.
    fn home(&self, name: &str, identity: &str) -> PathBuf {
        let home = self.dir.path().join(name);
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("host-identity"), identity).unwrap();
        home
    }
    fn storage(&self) -> Storage {
        Storage::at(self.client.clone()).unwrap()
    }
    fn registry_path(&self) -> PathBuf {
        self.client.join("remote/hosts.json")
    }
    fn registry_bytes(&self) -> Vec<u8> {
        std::fs::read(self.registry_path()).unwrap()
    }
    fn record(&self, id: &str) -> HostRecord {
        HostsFile::load(&self.storage())
            .unwrap()
            .find(id)
            .expect("the route is registered")
            .clone()
    }
    /// The targets a `DIALS` fixture was asked for.
    fn targets(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("targets"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
    /// What a `DIALS` fixture does while a dial is under way: for a registry that
    /// another command changes between the dial and the save.
    fn during_dial(&self, body: &str) {
        script(
            &self.dir.path().join("during-dial"),
            &format!("#!/bin/sh\n{body}\n"),
        );
    }
    /// The socket of a running daemon for a route that is already registered.
    async fn serve(&self) -> (tokio::process::Child, PathBuf) {
        let route = uuid::Uuid::new_v4().to_string();
        let storage = Storage::at(self.client.clone()).unwrap();
        HostsFile {
            v: 1,
            hosts: vec![HostRecord {
                id: route.clone(),
                pairing: None,
                ssh: Some(Route {
                    target: "fixture".into(),
                    remote_bin: "/never".into(),
                    somtus: None,
                    home: None,
                    host_id: uuid::Uuid::new_v4().to_string(),
                    client_id: uuid::Uuid::new_v4().to_string(),
                }),
                label: "Fixture".into(),
                relay: String::new(),
                route: String::new(),
                added_at_unix: 1,
                allow_insecure_loopback: false,
            }],
        }
        .save(&storage)
        .unwrap();
        let socket = client_daemon::socket_path(&storage, &route).unwrap();
        let log = self.dir.path().join("daemon.log");
        let daemon = tokio::process::Command::new(REMOTE)
            .env("RIWORK_HOME", &self.client)
            .env("PATH", &self.path)
            .args([
                "client",
                "serve",
                "--desktop",
                &route,
                "--idle-seconds",
                "30",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            while !socket.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no socket: {}", std::fs::read_to_string(&log).unwrap()));
        (daemon, socket)
    }
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
/// The JSON of output that is exactly one line.
fn one_line(bytes: &[u8]) -> Value {
    let line = bytes.strip_suffix(b"\n").expect("ends its line");
    assert!(!line.contains(&b'\n'), "{}", text(bytes));
    serde_json::from_slice(line).unwrap()
}

#[tokio::test]
async fn ssh_client_daemon_routes_chat_and_pty_reconnects_without_killing_host_program_and_pins_identity()
 {
    let mut rig = Rig::new().await;
    let snapshot = rig
        .call(
            "native.invoke",
            json!({"module":"Hosts","method":"local","args":{}}),
        )
        .await;
    assert_eq!(snapshot["value"]["hostId"], rig.host);
    assert_eq!(rig.call("chats.list", json!({})).await["chats"], json!([]));
    let shell = rig.shell.clone();
    let opened = rig
        .call(
            "pty.open",
            json!({"shell_id":shell,"columns":80,"rows":24,"term":"xterm-256color"}),
        )
        .await;
    let stream = opened["stream"].as_str().unwrap();
    rig.call(
        "pty.write",
        json!({"stream":stream,"seq":0,"data":URL_SAFE_NO_PAD.encode(b"ssh-pty-proof")}),
    )
    .await;
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let read = rig
                .call("pty.read", json!({"stream":stream,"wait_ms":100}))
                .await;
            bytes.extend(
                URL_SAFE_NO_PAD
                    .decode(read["data"].as_str().unwrap())
                    .unwrap(),
            );
            if String::from_utf8_lossy(&bytes).contains("ssh-pty-proof") {
                break;
            }
        }
    })
    .await
    .unwrap();
    rig.stop().await;
    assert!(
        Command::new(&rig.tmux)
            .arg("-S")
            .arg(&rig.tmux_socket)
            .args(["has-session", "-t", &shell])
            .status()
            .unwrap()
            .success(),
        "host tmux program survives transport loss"
    );
    rig.start().await;
    assert_eq!(rig.call("chats.list", json!({})).await["chats"], json!([]));
    rig.stop().await;
    std::fs::write(
        rig.dir.path().join("identity"),
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    rig.start().await;
    let refused = client_daemon::daemon_call(
        &rig.socket,
        "native.invoke",
        json!({"module":"Store","method":"renameProject","args":{}}),
        1200,
    )
    .await
    .unwrap();
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        !std::fs::read_to_string(rig.dir.path().join("calls"))
            .unwrap()
            .contains("Store.renameProject")
    );
    // The refusal says what is wrong: not a login problem, and not one that time fixes.
    let status = status_with_cause(&rig.socket, Duration::from_secs(15)).await;
    assert_eq!(status["state"], "offline", "{status}");
    assert_eq!(status["cause"], "identity_changed", "{status}");
    assert_eq!(status["needs_action"], true, "{status}");
    assert!(
        status["reason"]
            .as_str()
            .unwrap()
            .contains("identity changed"),
        "{status}"
    );
    let refused = client_daemon::daemon_call(
        &rig.socket,
        "native.invoke",
        json!({"module":"Store","method":"renameProject","args":{}}),
        1200,
    )
    .await
    .unwrap();
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("identity changed"),
        "{refused}"
    );
    rig.stop().await;
}

#[tokio::test]
async fn a_home_replaced_under_a_running_session_is_refused_within_seconds() {
    let mut rig = Rig::new().await;
    assert_eq!(rig.call("chats.list", json!({})).await["chats"], json!([]));
    std::fs::write(
        rig.dir.path().join("host/host-identity"),
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    let status = status_with_cause(&rig.socket, Duration::from_secs(20)).await;
    assert_eq!(status["cause"], "identity_changed", "{status}");
    let refused = client_daemon::daemon_call(
        &rig.socket,
        "native.invoke",
        json!({"module":"Store","method":"renameProject","args":{}}),
        1200,
    )
    .await
    .unwrap();
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        !std::fs::read_to_string(rig.dir.path().join("calls"))
            .unwrap()
            .contains("Store.renameProject")
    );
    rig.stop().await;
}

#[tokio::test]
async fn add_ssh_names_each_failure_shows_what_ssh_printed_and_registers_nothing() {
    for (body, named, printed) in [
        (
            "echo 'user@fixture: Permission denied (publickey).' >&2; exit 255",
            "SSH login was refused",
            Some("Permission denied (publickey)"),
        ),
        (
            "echo 'Host key verification failed.' >&2; exit 255",
            "does not trust this host's key",
            Some("Host key verification failed"),
        ),
        (
            "echo 'ssh: Could not resolve hostname fixture: nodename nor servname provided, or not known' >&2; exit 255",
            "could not resolve the host name",
            Some("Could not resolve hostname fixture"),
        ),
        (
            "echo 'ssh: connect to host fixture port 22: Connection refused' >&2; exit 255",
            "could not reach the host",
            Some("port 22: Connection refused"),
        ),
        (
            "echo 'sh: /never: not found' >&2; exit 127",
            "could not run on the host",
            Some("/never: not found"),
        ),
        ("echo Welcome; exit 1", "startup files", None),
    ] {
        let shim = Shim::new(body);
        let out = shim
            .add_ssh(&[
                "--remote-bin".as_ref(),
                "/never".as_ref(),
                "--json".as_ref(),
            ])
            .await;
        let stderr = text(&out.stderr);
        assert!(!out.status.success(), "{body}");
        assert!(out.stdout.is_empty(), "{body}");
        let named_at = stderr
            .find(named)
            .unwrap_or_else(|| panic!("{body}: {stderr}"));
        if let Some(printed) = printed {
            let printed_at = stderr
                .find(printed)
                .unwrap_or_else(|| panic!("{body}: {stderr}"));
            assert!(named_at < printed_at, "{stderr}");
        }
        assert_eq!(shim.registered(), 0, "{body}");
    }
}

#[tokio::test]
async fn add_ssh_gets_past_a_talkative_login() {
    let shim = Shim::new(&format!("echo Welcome to the host; echo; {RUN_REMOTE}"));
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.dir.path().join("host");
    let out = shim
        .add_ssh(&[
            "--remote-bin".as_ref(),
            REMOTE.as_ref(),
            "--somtus".as_ref(),
            cli.as_ref(),
            "--home".as_ref(),
            home.as_ref(),
            "--json".as_ref(),
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let added: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(added["host_id"], id.as_str());
    assert_eq!(shim.registered(), 1);
}

/// A mistyped `--somtus` must stop at registration, not look like a host whose
/// every request fails, though its identity no longer needs the CLI.
#[tokio::test]
async fn add_ssh_refuses_a_cli_that_cannot_start_with_a_healthy_identity() {
    let shim = Shim::new(RUN_REMOTE);
    let home = shim.dir.path().join("host");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("host-identity"), uuid::Uuid::new_v4().to_string()).unwrap();
    let out = shim
        .add_ssh(&[
            "--remote-bin".as_ref(),
            REMOTE.as_ref(),
            "--somtus".as_ref(),
            "/nonexistent/somtus".as_ref(),
            "--home".as_ref(),
            home.as_ref(),
        ])
        .await;
    assert!(!out.status.success());
    let stderr = text(&out.stderr);
    assert!(stderr.contains("cannot start the Somtus CLI"), "{stderr}");
    assert_eq!(shim.registered(), 0);
}

#[tokio::test]
async fn a_failure_only_a_person_can_fix_is_named_and_retried_slowly() {
    let needs_person =
        Shim::new("echo 'user@fixture: Permission denied (publickey).' >&2; exit 255");
    let passing =
        Shim::new("echo 'ssh: connect to host fixture port 22: Connection refused' >&2; exit 255");
    let ((_a, denied), (_b, refused)) = tokio::join!(needs_person.serve(), passing.serve());
    let (denied, refused) = tokio::join!(
        status_with_cause(&denied, Duration::from_secs(20)),
        status_with_cause(&refused, Duration::from_secs(20)),
    );
    assert_eq!(denied["state"], "offline", "{denied}");
    assert_eq!(denied["cause"], "auth_failed", "{denied}");
    assert_eq!(denied["needs_action"], true, "{denied}");
    assert_eq!(refused["cause"], "unreachable", "{refused}");
    assert!(refused.get("needs_action").is_none(), "{refused}");
    // Only fixed words: what ssh printed names the account and the host.
    for status in [&denied, &refused] {
        assert!(
            !status["reason"].as_str().unwrap().contains("fixture"),
            "{status}"
        );
    }
    // The other kind goes on along the normal curve, a retry within a second or two.
    tokio::time::timeout(Duration::from_secs(20), async {
        while passing.attempts() < 3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a failure that can pass is retried");
    assert_eq!(needs_person.attempts(), 1);
}

#[tokio::test]
async fn a_call_on_a_link_that_is_down_says_nothing_was_sent_and_what_is_wrong() {
    async fn refused(body: &str) -> Value {
        let shim = Shim::new(body);
        let (_daemon, socket) = shim.serve().await;
        status_with_cause(&socket, Duration::from_secs(20)).await;
        let reply = client_daemon::daemon_call(&socket, "chats.list", json!({}), 1000)
            .await
            .unwrap();
        assert_eq!(reply["ok"], false, "{reply}");
        assert_eq!(reply["error"]["code"], "offline", "{reply}");
        assert_eq!(reply["error"]["not_sent"], true, "{reply}");
        // Only fixed words: what ssh printed names the account and the host.
        assert!(
            !reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("fixture"),
            "{reply}"
        );
        reply["error"].clone()
    }
    let needs_person =
        refused("echo 'user@fixture: Permission denied (publickey).' >&2; exit 255").await;
    assert_eq!(needs_person["cause"], "auth_failed", "{needs_person}");
    assert_eq!(needs_person["needs_action"], true, "{needs_person}");
    let passing =
        refused("echo 'ssh: connect to host fixture port 22: Connection refused' >&2; exit 255")
            .await;
    assert_eq!(passing["cause"], "unreachable", "{passing}");
    assert!(passing.get("needs_action").is_none(), "{passing}");
}

/// The real host's limit of eight streams, reached by terminals nobody can see any more
/// if a stream opened for a bridge that left stays open.
#[tokio::test]
async fn terminals_that_leave_while_opening_leave_nothing_behind_on_a_real_host() {
    use client_daemon::{AttachRequest, daemon_attach, read_frame, write_frame};
    let mut rig = Rig::new().await;
    let request = AttachRequest {
        shell_id: rig.shell.clone(),
        columns: 80,
        rows: 24,
        term: "xterm-256color".into(),
        ignore_size: false,
    };
    let calls = rig.dir.path().join("calls");
    let opens_started = || {
        std::fs::read_to_string(&calls)
            .unwrap_or_default()
            .matches("shell attach")
            .count()
    };
    for left in 1..=9 {
        let bridge = daemon_attach(&rig.socket, &request).await.unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            while opens_started() < left {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("its open reaches the host");
        drop(bridge);
    }
    let mut bridge = daemon_attach(&rig.socket, &request).await.unwrap();
    let mut seen = Vec::new();
    let mut online = false;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (kind, payload) = read_frame(&mut bridge)
                .await
                .unwrap()
                .expect("the daemon stays");
            match kind {
                b'E' => panic!("it ended: {}", String::from_utf8_lossy(&payload)),
                b'S' if !online => {
                    online =
                        serde_json::from_slice::<Value>(&payload).unwrap()["state"] == "online";
                    if online {
                        write_frame(&mut bridge, b'D', b"echo attach-proof\r")
                            .await
                            .unwrap();
                    }
                }
                b'D' => {
                    seen.extend(payload);
                    if String::from_utf8_lossy(&seen).contains("attach-proof") {
                        return;
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the terminal works");
    rig.stop().await;
}

/// tokio's read of stdin cannot be cancelled, so a normal return from `main` would
/// wait for one more byte that may never come.
#[tokio::test]
async fn connect_exits_when_serving_ends_even_with_stdin_held_open() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let shim = Shim::new("exit 1");
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let mut child = tokio::process::Command::new(REMOTE)
        .env("RIWORK_HOME", shim.dir.path().join("host"))
        .arg("connect")
        .arg("--somtus")
        .arg(&cli)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let hello = json!({"v":1,"type":"hello","client_id":uuid::Uuid::new_v4().to_string(),"expected_host_id":id});
    stdin
        .write_all(format!("{hello}\n").as_bytes())
        .await
        .unwrap();
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
    let mut ready = String::new();
    stdout.read_line(&mut ready).await.unwrap();
    assert!(ready.contains("\"ready\""), "{ready}");
    // Nobody will read the answer to this request, which ends the session from
    // inside while it is already waiting for the next line.
    drop(stdout);
    let request = json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":"projects.list","params":{}});
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("the process does not wait for stdin")
        .unwrap();
    assert!(!status.success());
    let mut stderr = String::new();
    tokio::io::AsyncReadExt::read_to_string(child.stderr.as_mut().unwrap(), &mut stderr)
        .await
        .unwrap();
    assert!(stderr.contains("Error:"), "{stderr}");
    drop(stdin);
}

/// A failed print of the error must not stop the exit either: under a wrapper whose logger
/// has died, a panic there would leave the process waiting in the same place.
#[tokio::test]
async fn connect_exits_when_serving_ends_even_with_stderr_gone_and_stdin_held_open() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let shim = Shim::new("exit 1");
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let mut child = tokio::process::Command::new(REMOTE)
        .env("RIWORK_HOME", shim.dir.path().join("host"))
        .arg("connect")
        .arg("--somtus")
        .arg(&cli)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let hello = json!({"v":1,"type":"hello","client_id":uuid::Uuid::new_v4().to_string(),"expected_host_id":id});
    stdin
        .write_all(format!("{hello}\n").as_bytes())
        .await
        .unwrap();
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
    let mut ready = String::new();
    stdout.read_line(&mut ready).await.unwrap();
    assert!(ready.contains("\"ready\""), "{ready}");
    drop(stdout);
    drop(child.stderr.take());
    let request = json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":"projects.list","params":{}});
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("the process does not wait for stdin")
        .unwrap();
    assert_eq!(status.code(), Some(1));
    drop(stdin);
}

/// Typing goes by a process of its own when the CLI's typing worker will not start, and
/// says so on stderr. With stderr gone that must cost neither the batch nor the session.
#[tokio::test]
async fn typing_without_a_worker_is_answered_even_with_stderr_gone() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let (id, shell) = (
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    let (cli, typed) = (dir.path().join("cli"), dir.path().join("typed"));
    script(
        &cli,
        &format!(
            r#"#!/bin/sh
case "$1 $2" in
capabilities*) echo '{{"v":1,"shell_keys_serve":true,"native":{{"v":1,"methods":["Hosts.local"]}}}}';;
'native invoke') echo '{{"v":1,"value":{{"hostId":"{id}"}}}}';;
'shell list') echo '[{{"id":"{shell}","alive":true}}]';;
'shell keys') if [ "$3" = --serve ]; then exit 1; fi; echo "$*" >> '{typed}';;
*) echo '[]';;
esac
"#,
            typed = typed.display()
        ),
    );
    let mut child = tokio::process::Command::new(REMOTE)
        .env("RIWORK_HOME", dir.path().join("host"))
        .arg("connect")
        .arg("--somtus")
        .arg(&cli)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let hello = json!({"v":1,"type":"hello","client_id":uuid::Uuid::new_v4().to_string(),"expected_host_id":id});
    stdin
        .write_all(format!("{hello}\n").as_bytes())
        .await
        .unwrap();
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).await.unwrap();
    assert!(line.contains("\"ready\""), "{line}");
    drop(child.stderr.take());
    let request = json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":"shell.keys","params":{"shell_id":shell,"batch":uuid::Uuid::new_v4().to_string(),"items":[{"key":"Enter"}]}});
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    line.clear();
    tokio::time::timeout(Duration::from_secs(15), stdout.read_line(&mut line))
        .await
        .expect("the batch is answered")
        .unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    assert_eq!(reply["result"]["status"], "sent", "{line:?}");
    assert!(
        std::fs::read_to_string(&typed)
            .unwrap()
            .contains(&format!("shell keys {shell} --")),
        "typed by a process"
    );
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("the session ends with its stdin")
        .unwrap();
    assert!(status.success(), "{status:?}");
}

#[tokio::test]
async fn connect_exits_cleanly_when_its_client_goes() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let shim = Shim::new("exit 1");
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let mut child = tokio::process::Command::new(REMOTE)
        .env("RIWORK_HOME", shim.dir.path().join("host"))
        .arg("connect")
        .arg("--somtus")
        .arg(&cli)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let hello = json!({"v":1,"type":"hello","client_id":uuid::Uuid::new_v4().to_string(),"expected_host_id":id});
    stdin
        .write_all(format!("{hello}\n").as_bytes())
        .await
        .unwrap();
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
    let mut ready = String::new();
    stdout.read_line(&mut ready).await.unwrap();
    assert!(ready.contains("\"ready\""), "{ready}");
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("the process ends with its stdin")
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn connect_says_why_it_cannot_serve_before_it_exits() {
    let dir = tempfile::tempdir().unwrap();
    // A copy with no `somtus` beside it, and nothing in the environment that names one.
    let lone = dir.path().join("somtus-remote");
    std::fs::copy(REMOTE, &lone).unwrap();
    let out = tokio::process::Command::new(&lone)
        .env("RIWORK_HOME", dir.path().join("home"))
        .env_remove("SOMTUS_CLI")
        .env_remove("RIWORK_CLI")
        .arg("connect")
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(
        one_line(&out.stdout),
        json!({"v":1,"type":"refused","code":"cli_missing"})
    );
    assert!(
        text(&out.stderr).contains("--somtus"),
        "{}",
        text(&out.stderr)
    );
    // A home that cannot be made.
    let file = dir.path().join("a-file");
    std::fs::write(&file, "").unwrap();
    let out = tokio::process::Command::new(REMOTE)
        .env("RIWORK_HOME", &file)
        .args(["connect", "--somtus", "/nonexistent/somtus"])
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(
        one_line(&out.stdout),
        json!({"v":1,"type":"refused","code":"home_unavailable"})
    );
}

// ---- Registering and changing routes -----------------------------------------------

/// A dial runs on the target and leaves a marker there, so everything that can be
/// refused without one is.
#[tokio::test]
async fn add_ssh_refuses_what_it_can_before_it_connects() {
    let shim = Shim::new(DIALS);
    let long_label = "x".repeat(65);
    let long_target = "x".repeat(513);
    let cases: [(&[&std::ffi::OsStr], &str); 7] = [
        (
            args!["add-ssh", "--target", "fixture", "--label", " Studio"],
            "label needs",
        ),
        (
            args!["add-ssh", "--target", "fixture", "--label", ""],
            "label needs",
        ),
        (
            args!["add-ssh", "--target", "fixture", "--label", long_label],
            "label needs",
        ),
        (
            args!["add-ssh", "--target", "fixture", "--home", "relative/home"],
            "absolute",
        ),
        (
            args!["add-ssh", "--target", "fixture", "--home", "~/home"],
            "absolute",
        ),
        (
            args!["add-ssh", "--target", "fixture", "--home", "$HOME/home"],
            "absolute",
        ),
        (
            args!["add-ssh", "--target", long_target],
            "invalid SSH target",
        ),
    ];
    for (arguments, refusal) in cases {
        let out = shim.hosts(arguments).await;
        assert!(!out.status.success(), "{arguments:?}");
        assert!(
            text(&out.stderr).contains(refusal),
            "{arguments:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(shim.attempts(), 0, "{arguments:?}");
        assert_eq!(shim.registered(), 0, "{arguments:?}");
    }

    // What is already registered: the same target and home, with or without a home.
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let added = shim.added("fixture", &cli, &home).await;
    let dials = shim.attempts();
    let out = shim
        .hosts(args!["add-ssh", "--target", "fixture", "--home", home])
        .await;
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("already registered"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(shim.attempts(), dials);
    assert_eq!(shim.registered(), 1);
    // A full registry.
    let record = shim.record(added["id"].as_str().unwrap());
    let full: Vec<HostRecord> = (0..64)
        .map(|n| {
            let mut other = record.clone();
            other.id = uuid::Uuid::new_v4().to_string();
            other.ssh.as_mut().unwrap().target = format!("fixture-{n}");
            other
        })
        .collect();
    HostsFile { v: 1, hosts: full }
        .save(&shim.storage())
        .unwrap();
    let out = shim.hosts(args!["add-ssh", "--target", "one-more"]).await;
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("full"), "{}", text(&out.stderr));
    assert_eq!(shim.attempts(), dials);
    // A registry it could not save to anyway.
    std::fs::write(shim.registry_path(), "{\"v\":1,\"hosts\":[{}]}").unwrap();
    let out = shim.hosts(args!["add-ssh", "--target", "one-more"]).await;
    assert!(!out.status.success());
    assert_eq!(shim.attempts(), dials);
}

/// The checks before the dial are a courtesy; the ones after it, under the lock, are
/// the ones that count, as the dial happens outside the lock.
#[tokio::test]
async fn add_ssh_checks_again_after_it_has_connected() {
    let shim = Shim::new(DIALS);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let record = |target: String, home: &std::path::Path| HostRecord {
        id: uuid::Uuid::new_v4().to_string(),
        pairing: None,
        ssh: Some(Route {
            target,
            remote_bin: REMOTE.into(),
            somtus: Some(cli.to_string_lossy().into_owned()),
            home: Some(home.to_string_lossy().into_owned()),
            host_id: id.clone(),
            client_id: uuid::Uuid::new_v4().to_string(),
        }),
        label: "Other".into(),
        relay: String::new(),
        route: String::new(),
        added_at_unix: 1,
        allow_insecure_loopback: false,
    };
    let prepared = shim.dir.path().join("prepared.json");
    shim.during_dial(&format!(
        "cp '{}' '{}'",
        prepared.display(),
        shim.registry_path().display()
    ));
    let taken = vec![record("fixture".into(), &home)];
    let full: Vec<_> = (0..64)
        .map(|n| record(format!("other-{n}"), &home))
        .collect();
    for (hosts, refusal) in [(taken, "already registered"), (full, "full")] {
        // Empty as it starts, which is what gets it past the check before the dial.
        HostsFile {
            v: 1,
            hosts: vec![],
        }
        .save(&shim.storage())
        .unwrap();
        std::fs::write(
            &prepared,
            serde_json::to_vec(&HostsFile { v: 1, hosts }).unwrap(),
        )
        .unwrap();
        let out = shim
            .hosts(args![
                "add-ssh",
                "--target",
                "fixture",
                "--remote-bin",
                REMOTE,
                "--somtus",
                cli,
                "--home",
                home,
            ])
            .await;
        assert!(!out.status.success());
        assert!(text(&out.stderr).contains(refusal), "{}", text(&out.stderr));
        assert_eq!(shim.registry_bytes(), std::fs::read(&prepared).unwrap());
    }
}

/// A label the person never gave must not be what stops the add.
#[tokio::test]
async fn add_ssh_gives_a_long_target_a_label_that_fits() {
    let shim = Shim::new(RUN_REMOTE);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    // Counted in characters.
    let target = "ü".repeat(70);
    let out = shim
        .hosts(args![
            "add-ssh",
            "--target",
            target,
            "--remote-bin",
            REMOTE,
            "--somtus",
            cli,
            "--home",
            home,
            "--json",
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let added = one_line(&out.stdout);
    assert_eq!(added["label"], "ü".repeat(64));
    assert_eq!(added["ssh"]["target"], target.as_str());
}

/// A mistyped `--home` looks like a healthy host with no projects, so it gets a warning.
/// The home that does not exist yet stays legitimate: first use makes it.
#[tokio::test]
async fn add_ssh_warns_on_stderr_when_the_home_it_reached_looks_new() {
    let shim = Shim::new(RUN_REMOTE);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let typo = shim.dir.path().join("typo");
    let out = shim
        .hosts(args![
            "add-ssh",
            "--target",
            "fixture",
            "--remote-bin",
            REMOTE,
            "--somtus",
            cli,
            "--home",
            typo,
            "--json",
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    // Stdout stays the one JSON value.
    let added = one_line(&out.stdout);
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with("Warning: "), "{stderr}");
    assert!(stderr.contains("looks new"), "{stderr}");
    assert!(stderr.contains("new, empty installation"), "{stderr}");
    assert!(
        stderr.contains(&typo.to_string_lossy().into_owned()),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("hosts remove {}", added["id"].as_str().unwrap())),
        "{stderr}"
    );
    assert_eq!(shim.registered(), 1);

    // A home with state in it is no surprise, and neither is one with an identity, nor one
    // that has only ever held terminals or chats.
    let used = shim.dir.path().join("used");
    std::fs::create_dir(&used).unwrap();
    std::fs::write(used.join("state.json"), "{}").unwrap();
    let identified = shim.home("identified", &id);
    let terminals = shim.dir.path().join("terminals");
    std::fs::create_dir(&terminals).unwrap();
    std::fs::write(terminals.join("sessions.json"), "{}").unwrap();
    let chats = shim.dir.path().join("chats");
    std::fs::create_dir_all(chats.join("chats")).unwrap();
    for (target, home) in [
        ("used", &used),
        ("identified", &identified),
        ("terminals", &terminals),
        ("chats", &chats),
    ] {
        let out = shim
            .hosts(args![
                "add-ssh",
                "--target",
                target,
                "--remote-bin",
                REMOTE,
                "--somtus",
                cli,
                "--home",
                home,
                "--json",
            ])
            .await;
        assert!(out.status.success(), "{}", text(&out.stderr));
        assert!(
            !text(&out.stderr).contains("new, empty installation"),
            "{}",
            text(&out.stderr)
        );
    }
}

/// Two routes to one installation are designed for, so the second is not refused, but an
/// accident should not pass in silence.
#[tokio::test]
async fn add_ssh_warns_of_a_second_route_to_one_installation_and_still_adds_it() {
    let shim = Shim::new(RUN_REMOTE);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let first = shim.added("alias-one", &cli, &home).await;
    let first_id = first["id"].as_str().unwrap();
    let out = shim
        .hosts(args![
            "add-ssh",
            "--target",
            "alias-two",
            "--remote-bin",
            REMOTE,
            "--somtus",
            cli,
            "--home",
            home,
            "--json",
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let second = one_line(&out.stdout);
    assert_ne!(second["id"], first["id"]);
    assert_eq!(second["host_id"], first["host_id"]);
    assert_eq!(shim.registered(), 2);
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with("Warning: "), "{stderr}");
    assert!(stderr.contains(first_id), "{stderr}");
    assert!(stderr.contains("Studio"), "{stderr}");
    assert!(
        stderr.contains(&format!("hosts update-ssh {first_id}")),
        "{stderr}"
    );

    // Another installation is nothing to warn of.
    let other = shim.home("other", &uuid::Uuid::new_v4().to_string());
    let out = shim
        .hosts(args![
            "add-ssh",
            "--target",
            "alias-three",
            "--remote-bin",
            REMOTE,
            "--somtus",
            cli,
            "--home",
            other,
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        !text(&out.stderr).contains("Warning"),
        "{}",
        text(&out.stderr)
    );
}

/// A route back to this computer merges with its own row. What this computer's identity
/// is may be unknown, and looking must never create it or fail the add.
#[tokio::test]
async fn add_ssh_warns_of_a_route_back_to_this_computer_and_only_ever_reads_its_identity() {
    let id = uuid::Uuid::new_v4().to_string();
    let add = async |shim: &Shim, name: &str| {
        let cli = shim.cli(&id);
        let home = shim.home(name, &id);
        let out = shim
            .hosts(args![
                "add-ssh",
                "--target",
                "fixture",
                "--remote-bin",
                REMOTE,
                "--somtus",
                cli,
                "--home",
                home,
                "--json",
            ])
            .await;
        assert!(out.status.success(), "{}", text(&out.stderr));
        one_line(&out.stdout);
        text(&out.stderr)
    };
    let shim = Shim::new(RUN_REMOTE);
    let mine = shim.client.join("host-identity");
    std::fs::write(&mine, format!("{id}\n")).unwrap();
    let stderr = add(&shim, "host").await;
    assert!(stderr.contains("this computer"), "{stderr}");
    assert!(stderr.contains("hosts update-ssh"), "{stderr}");

    // Another computer's.
    let shim = Shim::new(RUN_REMOTE);
    std::fs::write(
        shim.client.join("host-identity"),
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    assert!(!add(&shim, "host").await.contains("Warning"));

    // Damaged is unknown, and stays as it is.
    let shim = Shim::new(RUN_REMOTE);
    let mine = shim.client.join("host-identity");
    std::fs::write(&mine, "not an identity").unwrap();
    assert!(!add(&shim, "host").await.contains("Warning"));
    assert_eq!(std::fs::read_to_string(&mine).unwrap(), "not an identity");

    // Missing is unknown, and stays missing.
    let shim = Shim::new(RUN_REMOTE);
    assert!(!add(&shim, "host").await.contains("Warning"));
    assert!(!shim.client.join("host-identity").exists());
}

#[tokio::test]
async fn update_ssh_changes_how_a_route_connects_and_keeps_what_hangs_on_its_id() {
    let shim = Shim::new(DIALS);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let added = shim.added("old-alias", &cli, &home).await;
    let route = added["id"].as_str().unwrap();
    let before = shim.record(route);
    let before_route = before.ssh.clone().unwrap();

    // The installation moved: another address, another home, the same identity.
    let moved = shim.home("moved", &id);
    let out = shim
        .hosts(args![
            "update-ssh",
            route,
            "--target",
            "new-alias",
            "--home",
            moved,
            "--json",
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let updated = one_line(&out.stdout);
    assert_eq!(updated["id"], route);
    let after = shim.record(route);
    let after_route = after.ssh.clone().unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.label, before.label);
    assert_eq!(after.added_at_unix, before.added_at_unix);
    assert_eq!(after_route.host_id, before_route.host_id);
    assert_eq!(after_route.client_id, before_route.client_id);
    assert_eq!(after_route.remote_bin, before_route.remote_bin);
    assert_eq!(after_route.somtus, before_route.somtus);
    assert_eq!(after_route.target, "new-alias");
    assert_eq!(after_route.home.as_deref(), moved.to_str());
    // It tried the new values, not the old ones.
    assert_eq!(shim.targets(), ["old-alias", "new-alias"]);
    // As the route's own client: another one would stay on the target as an upload owner.
    let markers: Vec<_> = std::fs::read_dir(moved.join("remote"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("ssh-client-"))
        .collect();
    assert_eq!(
        markers,
        [format!("ssh-client-{}.json", before_route.client_id)]
    );
    // `--json` shows the route the way the list does.
    let listed = shim.hosts(args!["list", "--json"]).await;
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed[0], updated);

    // Back to the account's default home, which on this fixture is the client's own: it
    // has no identity saved, so the fixture CLI answers, with the same one.
    let out = shim
        .hosts(args![
            "update-ssh",
            route,
            "--no-home",
            "--target",
            "third-alias"
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let summary = text(&out.stdout);
    assert!(
        summary.contains(route) && summary.contains("Studio"),
        "{summary}"
    );
    let cleared = shim.record(route).ssh.unwrap();
    assert_eq!(cleared.home, None);
    assert_eq!(cleared.somtus, before_route.somtus);

    // A companion with a CLI beside it needs no `--somtus`. One without is another way a
    // route stops working, and it then stays as it was.
    let installed = shim.dir.path().join("installed");
    std::fs::create_dir(&installed).unwrap();
    let companion = installed.join("somtus-remote");
    std::fs::copy(REMOTE, &companion).unwrap();
    let out = shim
        .hosts(args![
            "update-ssh",
            route,
            "--remote-bin",
            companion,
            "--no-somtus"
        ])
        .await;
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("--somtus"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(shim.record(route).ssh.unwrap().somtus, before_route.somtus);
    std::fs::copy(&cli, installed.join("somtus")).unwrap();
    let out = shim
        .hosts(args![
            "update-ssh",
            route,
            "--remote-bin",
            companion,
            "--no-somtus"
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let cleared = shim.record(route).ssh.unwrap();
    assert_eq!(cleared.somtus, None);
    assert_eq!(cleared.remote_bin, companion.to_str().unwrap());
}

/// A re-pin under a kept id would break every place that remembers the old identity.
#[tokio::test]
async fn update_ssh_refuses_another_installation_and_leaves_the_registry_as_it_was() {
    let shim = Shim::new(DIALS);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let added = shim.added("alias", &cli, &home).await;
    let route = added["id"].as_str().unwrap();
    let before = shim.registry_bytes();
    let other = shim.home("other", &uuid::Uuid::new_v4().to_string());
    let out = shim
        .hosts(args!["update-ssh", route, "--home", other])
        .await;
    assert!(!out.status.success());
    let stderr = text(&out.stderr);
    assert!(stderr.contains("identity changed"), "{stderr}");
    assert!(
        stderr.contains("remove the route and add it again"),
        "{stderr}"
    );
    assert_eq!(shim.registry_bytes(), before);

    // Nor does a candidate that cannot be reached change anything, and ssh's words are
    // for the person running this.
    let out = shim
        .hosts(args!["update-ssh", route, "--target", "unreachable-host"])
        .await;
    assert!(!out.status.success());
    let stderr = text(&out.stderr);
    assert!(stderr.contains("could not reach the host"), "{stderr}");
    assert!(stderr.contains("unreachable-host port 22"), "{stderr}");
    assert_eq!(shim.registry_bytes(), before);
}

#[tokio::test]
async fn update_ssh_of_the_label_alone_neither_dials_nor_disturbs_the_client_process() {
    let shim = Shim::new(DIALS);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let added = shim.added("alias", &cli, &home).await;
    let route = added["id"].as_str().unwrap();
    let daemon = OtherBuild::start(&shim.client, route, Some(2), true);
    let dials = shim.attempts();
    let before = shim.record(route);
    let out = shim
        .hosts(args!["update-ssh", route, "--label", "Renamed", "--json"])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(one_line(&out.stdout)["label"], "Renamed");
    let after = shim.record(route);
    assert_eq!(after.label, "Renamed");
    assert_eq!(after.ssh, before.ssh);
    assert_eq!(shim.attempts(), dials);
    assert_eq!(daemon.shutdowns(), 0);

    // A value the route already has changes nothing either, and the file stays as it is.
    let bytes = shim.registry_bytes();
    let out = shim
        .hosts(args![
            "update-ssh",
            route,
            "--target",
            "alias",
            "--home",
            home,
            "--label",
            "Renamed",
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(shim.attempts(), dials);
    assert_eq!(daemon.shutdowns(), 0);
    assert_eq!(shim.registry_bytes(), bytes);

    // Two routes at one address cannot come of `add-ssh`, but if a file holds them that is
    // no reason to refuse a new name.
    let mut twin = shim.record(route);
    twin.id = uuid::Uuid::new_v4().to_string();
    HostsFile {
        v: 1,
        hosts: vec![shim.record(route), twin],
    }
    .save(&shim.storage())
    .unwrap();
    let out = shim
        .hosts(args!["update-ssh", route, "--label", "Renamed again"])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(shim.record(route).label, "Renamed again");
}

/// A running client process holds the route it read, and would go on dialing it.
#[tokio::test]
async fn update_ssh_tells_the_client_process_of_a_route_that_changed() {
    let shim = Shim::new(DIALS);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let added = shim.added("alias", &cli, &home).await;
    let route = added["id"].as_str().unwrap();
    let daemon = OtherBuild::start(&shim.client, route, Some(2), true);
    let out = shim
        .hosts(args!["update-ssh", route, "--target", "new-alias"])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(daemon.shutdowns(), 1);
    assert_eq!(shim.record(route).ssh.unwrap().target, "new-alias");
}

/// The whole way: a real client process is on the old address, and after the update what
/// connects is a new one on the new address, for the same route id.
#[tokio::test]
async fn an_updated_route_is_what_its_client_process_dials_next() {
    let shim = Shim::new(DIALS);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let added = shim.added("old-alias", &cli, &home).await;
    let route = added["id"].as_str().unwrap();
    let socket = client_daemon::socket_path(&shim.storage(), route).unwrap();
    let online = async || {
        let ensured = shim
            .remote(args!["client", "ensure", "--desktop", route])
            .await;
        assert!(ensured.status.success(), "{}", text(&ensured.stderr));
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Ok(status) = client_daemon::daemon_status(&socket).await
                    && status["state"] == "online"
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the client process connects");
    };
    online().await;
    assert_eq!(shim.targets().last().unwrap(), "old-alias");
    let out = shim
        .hosts(args!["update-ssh", route, "--target", "new-alias"])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    tokio::time::timeout(Duration::from_secs(15), async {
        while socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the client process quits");
    let dials = shim.attempts();
    online().await;
    assert!(shim.attempts() > dials);
    assert_eq!(shim.targets().last().unwrap(), "new-alias");
    let reply = client_daemon::daemon_call(
        &socket,
        "native.invoke",
        json!({"module":"Hosts","method":"local","args":{}}),
        5000,
    )
    .await
    .unwrap();
    assert_eq!(reply["result"]["value"]["hostId"], id.as_str(), "{reply}");
    let removed = shim.hosts(args!["remove", route]).await;
    assert!(removed.status.success(), "{}", text(&removed.stderr));
}

/// A label the target gave it goes out of date with it, which the person should hear of.
#[tokio::test]
async fn update_ssh_mentions_a_label_that_still_names_the_old_target() {
    let shim = Shim::new(DIALS);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let out = shim
        .hosts(args![
            "add-ssh",
            "--target",
            "old-alias",
            "--remote-bin",
            REMOTE,
            "--somtus",
            cli,
            "--home",
            home,
            "--json",
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let added = one_line(&out.stdout);
    let route = added["id"].as_str().unwrap();
    assert_eq!(added["label"], "old-alias");
    let out = shim
        .hosts(args![
            "update-ssh",
            route,
            "--target",
            "new-alias",
            "--json"
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(one_line(&out.stdout)["label"], "old-alias");
    let stderr = text(&out.stderr);
    assert!(stderr.contains("Warning: "), "{stderr}");
    assert!(stderr.contains("--label"), "{stderr}");
    // Named by the person: theirs to keep, even when it looks like that target.
    let out = shim
        .hosts(args![
            "update-ssh",
            route,
            "--target",
            "newer-alias",
            "--label",
            "new-alias",
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        !text(&out.stderr).contains("Warning"),
        "{}",
        text(&out.stderr)
    );
    let out = shim
        .hosts(args!["update-ssh", route, "--label", "Studio"])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = shim
        .hosts(args!["update-ssh", route, "--target", "newest-alias"])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        !text(&out.stderr).contains("Warning"),
        "{}",
        text(&out.stderr)
    );
}

#[tokio::test]
async fn update_ssh_refuses_what_it_can_before_it_connects() {
    let shim = Shim::new(DIALS);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let other_home = shim.home("other", &id);
    let added = shim.added("alias", &cli, &home).await;
    let route = added["id"].as_str().unwrap();
    let other = shim.added("other-alias", &cli, &other_home).await;
    let other_route = other["id"].as_str().unwrap();
    let before = shim.registry_bytes();
    let dials = shim.attempts();
    let nobody = uuid::Uuid::new_v4().to_string();
    let long_label = "x".repeat(65);
    let cases: [(&[&std::ffi::OsStr], &str); 10] = [
        // Nothing asked for.
        (args!["update-ssh", route], "nothing to change"),
        (args!["update-ssh", "not-a-uuid", "--label", "x"], "full id"),
        (args!["update-ssh", nobody, "--label", "x"], "no host"),
        (args!["update-ssh", route, "--label", " x"], "label needs"),
        (
            args!["update-ssh", route, "--label", long_label],
            "label needs",
        ),
        (args!["update-ssh", route, "--home", "relative"], "absolute"),
        (
            args!["update-ssh", route, "--target=-oProxyCommand=x"],
            "invalid SSH target",
        ),
        (
            args!["update-ssh", route, "--target", "two words"],
            "invalid SSH target",
        ),
        (
            args!["update-ssh", route, "--somtus", "/x", "--no-somtus"],
            "cannot be used with",
        ),
        (
            args!["update-ssh", route, "--home", "/x", "--no-home"],
            "cannot be used with",
        ),
    ];
    for (arguments, refusal) in cases {
        let out = shim.hosts(arguments).await;
        assert!(!out.status.success(), "{arguments:?}");
        assert!(
            text(&out.stderr).contains(refusal),
            "{arguments:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(shim.attempts(), dials, "{arguments:?}");
        assert_eq!(shim.registry_bytes(), before, "{arguments:?}");
    }
    // The address of another route is refused; its own is not.
    let out = shim
        .hosts(args![
            "update-ssh",
            route,
            "--target",
            "other-alias",
            "--home",
            other_home,
        ])
        .await;
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("already registered"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(shim.attempts(), dials);
    assert_eq!(shim.registry_bytes(), before);
    let moved_cli = shim.dir.path().join("cli-moved");
    std::fs::copy(&cli, &moved_cli).unwrap();
    let out = shim
        .hosts(args![
            "update-ssh",
            other_route,
            "--target",
            "other-alias",
            "--home",
            other_home,
            "--somtus",
            moved_cli,
        ])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(shim.attempts(), dials + 1);
    assert_eq!(
        shim.record(other_route).ssh.unwrap().somtus.as_deref(),
        moved_cli.to_str()
    );
}

/// The dial happens outside the lock, so what it tried may no longer be what is there.
#[tokio::test]
async fn update_ssh_looks_again_after_connecting_and_saves_nothing_it_did_not_try() {
    let shim = Shim::new(DIALS);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let added = shim.added("alias", &cli, &home).await;
    let route = added["id"].as_str().unwrap();
    let record = shim.record(route);
    let changed = |change: fn(&mut Route)| {
        let mut other = record.clone();
        change(other.ssh.as_mut().unwrap());
        other
    };
    let mut taken = changed(|route| route.target = "new-alias".into());
    taken.id = uuid::Uuid::new_v4().to_string();
    let cases: [(Vec<HostRecord>, &str); 5] = [
        (vec![], "no host"),
        (
            vec![changed(|route| route.target = "elsewhere".into())],
            "changed while",
        ),
        (
            vec![changed(|route| {
                route.host_id = uuid::Uuid::new_v4().to_string();
            })],
            "changed while",
        ),
        (
            vec![changed(|route| {
                route.client_id = uuid::Uuid::new_v4().to_string();
            })],
            "changed while",
        ),
        (vec![record.clone(), taken], "already registered"),
    ];
    let prepared = shim.dir.path().join("prepared.json");
    shim.during_dial(&format!(
        "cp '{}' '{}'",
        prepared.display(),
        shim.registry_path().display()
    ));
    for (hosts, refusal) in cases {
        HostsFile {
            v: 1,
            hosts: vec![record.clone()],
        }
        .save(&shim.storage())
        .unwrap();
        std::fs::write(
            &prepared,
            serde_json::to_vec(&HostsFile { v: 1, hosts }).unwrap(),
        )
        .unwrap();
        let out = shim
            .hosts(args!["update-ssh", route, "--target", "new-alias"])
            .await;
        assert!(!out.status.success());
        assert!(
            text(&out.stderr).contains(refusal),
            "{refusal}: {}",
            text(&out.stderr)
        );
        assert_eq!(shim.registry_bytes(), std::fs::read(&prepared).unwrap());
    }
    // A rename meanwhile is no reason to refuse, and is kept.
    let mut renamed = record.clone();
    renamed.label = "Renamed meanwhile".into();
    HostsFile {
        v: 1,
        hosts: vec![record.clone()],
    }
    .save(&shim.storage())
    .unwrap();
    std::fs::write(
        &prepared,
        serde_json::to_vec(&HostsFile {
            v: 1,
            hosts: vec![renamed],
        })
        .unwrap(),
    )
    .unwrap();
    let out = shim
        .hosts(args!["update-ssh", route, "--target", "new-alias"])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let after = shim.record(route);
    assert_eq!(after.label, "Renamed meanwhile");
    assert_eq!(after.ssh.unwrap().target, "new-alias");
}

/// The list has to hold what `add-ssh` needs, because removing a route takes the only
/// copy with it. But the client id stays out: it names this Mac to the host.
#[tokio::test]
async fn hosts_list_shows_a_route_in_full_as_json_and_keeps_paths_out_of_its_text() {
    let shim = Shim::new(RUN_REMOTE);
    let out = shim.hosts(args!["list"]).await;
    assert!(out.status.success());
    assert!(
        text(&out.stdout).contains("add-ssh"),
        "{}",
        text(&out.stdout)
    );
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host home", &id);
    let added = shim.added("alias", &cli, &home).await;
    let route = added["id"].as_str().unwrap();
    assert_eq!(
        added["ssh"],
        json!({
            "target": "alias",
            "remote_bin": REMOTE,
            "somtus": cli.to_str().unwrap(),
            "home": home.to_str().unwrap(),
        })
    );
    let listed = shim.hosts(args!["list", "--json"]).await;
    assert!(listed.status.success());
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed[0], added);
    let client_id = shim.record(route).ssh.unwrap().client_id;
    assert!(!listed.to_string().contains(&client_id), "{listed}");
    let shown = shim.hosts(args!["list"]).await;
    let shown = text(&shown.stdout);
    assert_eq!(shown.lines().count(), 1, "{shown}");
    for word in [route, "ssh", id.as_str(), "Studio"] {
        assert!(shown.contains(word), "{word}: {shown}");
    }
    for path in [REMOTE, cli.to_str().unwrap(), home.to_str().unwrap()] {
        assert!(!shown.contains(path), "{path}: {shown}");
    }
    assert!(!shown.contains(&client_id), "{shown}");
}

/// Whatever the paths hold, the printed line has to be one a shell runs as it was meant.
#[tokio::test]
async fn removing_a_route_prints_the_command_that_adds_it_back() {
    let shim = Shim::new(RUN_REMOTE);
    std::os::unix::fs::symlink(REMOTE, shim.dir.path().join("bin/somtus-remote")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("Bob's $(home)", &id);
    let added = shim
        .hosts(args![
            "add-ssh",
            "--target",
            "alias",
            "--remote-bin",
            REMOTE,
            "--somtus",
            cli,
            "--home",
            home,
            "--label",
            "Bob's Mac",
            "--json",
        ])
        .await;
    assert!(added.status.success(), "{}", text(&added.stderr));
    let added = one_line(&added.stdout);
    let route = added["id"].as_str().unwrap();
    let removed = shim.hosts(args!["remove", route]).await;
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    let printed = text(&removed.stdout);
    let mut lines = printed.lines();
    assert_eq!(
        lines.next(),
        Some(format!("Removed host {route} (\"Bob's Mac\").").as_str())
    );
    let command = lines.next().unwrap();
    let command = &command[command.find("somtus-remote hosts add-ssh").expect(&printed)..];
    assert_eq!(shim.registered(), 0);
    let again = tokio::process::Command::new("/bin/sh")
        .args(["-c", command])
        .env("RIWORK_HOME", &shim.client)
        .env("PATH", &shim.path)
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(again.status.success(), "{command}: {}", text(&again.stderr));
    let listed = shim.hosts(args!["list", "--json"]).await;
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_ne!(listed[0]["id"], added["id"], "the route is a new one");
    for field in ["label", "host_id", "transport", "ssh"] {
        assert_eq!(listed[0][field], added[field], "{field}");
    }
}

/// The damage is in the record's ids, and what is lost with it is how the route connects.
#[tokio::test]
async fn a_route_whose_own_record_is_damaged_can_still_be_added_back() {
    let shim = Shim::new(RUN_REMOTE);
    std::os::unix::fs::symlink(REMOTE, shim.dir.path().join("bin/somtus-remote")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let added = shim.added("alias", &cli, &home).await;
    let route = added["id"].as_str().unwrap();
    let mut file: Value = serde_json::from_slice(&shim.registry_bytes()).unwrap();
    file["hosts"][0]["ssh"]["host_id"] = json!("corrupted");
    file["hosts"][0]["ssh"]["from_a_newer_build"] = json!(1);
    std::fs::write(shim.registry_path(), file.to_string()).unwrap();
    let listed = shim.hosts(args!["list"]).await;
    assert!(!listed.status.success());
    let removed = shim.hosts(args!["remove", route]).await;
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    let printed = text(&removed.stdout);
    assert!(printed.contains("valid again"), "{printed}");
    let command = printed
        .lines()
        .find_map(|line| {
            line.find("somtus-remote hosts add-ssh")
                .map(|at| &line[at..])
        })
        .unwrap_or_else(|| panic!("no command: {printed}"));
    let again = tokio::process::Command::new("/bin/sh")
        .args(["-c", command])
        .env("RIWORK_HOME", &shim.client)
        .env("PATH", &shim.path)
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(again.status.success(), "{command}: {}", text(&again.stderr));
    let listed = shim.hosts(args!["list", "--json"]).await;
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    for field in ["label", "host_id", "transport", "ssh"] {
        assert_eq!(listed[0][field], added[field], "{field}");
    }
}

/// Such a route still works and still loads, and the command it prints cannot be run as it
/// is. Only the person can say where that home really is.
#[tokio::test]
async fn removing_a_route_whose_home_is_not_absolute_says_what_add_ssh_will_ask_for() {
    let shim = Shim::new(RUN_REMOTE);
    let record = |target: &str, home: &str| HostRecord {
        id: uuid::Uuid::new_v4().to_string(),
        pairing: None,
        ssh: Some(Route {
            target: target.into(),
            remote_bin: "/opt/somtus-remote".into(),
            somtus: None,
            home: Some(home.into()),
            host_id: uuid::Uuid::new_v4().to_string(),
            client_id: uuid::Uuid::new_v4().to_string(),
        }),
        label: "Studio".into(),
        relay: String::new(),
        route: String::new(),
        added_at_unix: 1,
        allow_insecure_loopback: false,
    };
    let (old, plain) = (
        record("old", "relative/home"),
        record("plain", "/data/home"),
    );
    let (old_id, plain_id) = (old.id.clone(), plain.id.clone());
    HostsFile {
        v: 1,
        hosts: vec![old, plain],
    }
    .save(&shim.storage())
    .unwrap();
    let removed = shim.hosts(args!["remove", old_id]).await;
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    let printed = text(&removed.stdout);
    assert!(printed.contains("--home='relative/home'"), "{printed}");
    assert!(printed.contains("absolute path on the host"), "{printed}");
    let removed = shim.hosts(args!["remove", plain_id]).await;
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    let printed = text(&removed.stdout);
    assert!(printed.contains("--home='/data/home'"), "{printed}");
    assert!(!printed.contains("absolute"), "{printed}");
}

/// No client process can run under such a home, but a phone gateway can still dial from it,
/// and the save has happened by the time anyone would be told to quit.
#[tokio::test]
async fn a_home_too_long_for_a_socket_does_not_fail_a_save_that_worked() {
    let mut shim = Shim::new(DIALS);
    let mut long = shim.client.clone();
    while long.as_os_str().len() < 110 {
        long.push("a-directory-with-a-long-name");
    }
    std::fs::create_dir_all(&long).unwrap();
    shim.client = long;
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let added = shim.added("alias", &cli, &home).await;
    let route = added["id"].as_str().unwrap();
    let updated = shim
        .hosts(args![
            "update-ssh",
            route,
            "--target",
            "new-alias",
            "--json"
        ])
        .await;
    assert!(updated.status.success(), "{}", text(&updated.stderr));
    assert_eq!(one_line(&updated.stdout)["ssh"]["target"], "new-alias");
    let removed = shim.hosts(args!["remove", route]).await;
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    assert!(text(&removed.stdout).contains("Removed host"));
    assert_eq!(shim.registered(), 0);
}

/// `[]` from this would read as "every host was removed", and the desktop would believe it.
#[tokio::test]
async fn hosts_list_fails_rather_than_saying_no_hosts_when_it_cannot_look() {
    let shim = Shim::new(RUN_REMOTE);
    let storage = shim.storage();
    std::os::unix::fs::symlink(shim.dir.path().join("away.json"), shim.registry_path()).unwrap();
    let listed = shim.hosts(args!["list", "--json"]).await;
    assert!(!listed.status.success(), "{}", text(&listed.stdout));
    assert!(listed.stdout.is_empty(), "{}", text(&listed.stdout));
    std::fs::remove_file(shim.registry_path()).unwrap();
    if unsafe { libc::geteuid() } != 0 {
        std::fs::set_permissions(&storage.dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let listed = shim.hosts(args!["list", "--json"]).await;
        std::fs::set_permissions(&storage.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!listed.status.success(), "{}", text(&listed.stdout));
        assert!(listed.stdout.is_empty(), "{}", text(&listed.stdout));
    }
    let listed = shim.hosts(args!["list", "--json"]).await;
    assert!(listed.status.success(), "{}", text(&listed.stderr));
    assert_eq!(text(&listed.stdout).trim(), "[]");
}

/// From the command line, what a person does when `hosts list` says the file is bad.
#[tokio::test]
async fn hosts_remove_takes_out_the_record_that_broke_the_registry() {
    let shim = Shim::new(RUN_REMOTE);
    let id = uuid::Uuid::new_v4().to_string();
    let cli = shim.cli(&id);
    let home = shim.home("host", &id);
    let added = shim.added("alias", &cli, &home).await;
    let good = added["id"].as_str().unwrap();
    let mut file: Value = serde_json::from_slice(&shim.registry_bytes()).unwrap();
    let kept = file["hosts"][0].clone();
    let bad = "\u{1b}[31mnot-a-uuid";
    let mut broken = kept.clone();
    broken["id"] = json!(bad);
    broken["label"] = json!("evil\u{1b}]0;title\u{7}");
    file["hosts"].as_array_mut().unwrap().push(broken);
    std::fs::write(shim.registry_path(), file.to_string()).unwrap();

    // Everything that reads the file refuses it, and says what to run.
    let listed = shim.hosts(args!["list"]).await;
    assert!(!listed.status.success());
    let stderr = text(&listed.stderr);
    assert!(stderr.contains("hosts remove"), "{stderr}");
    assert!(!stderr.contains('\u{1b}'), "{stderr:?}");

    // A name that is not there changes nothing, and what is there is shown safely, as
    // the list cannot be.
    let before = shim.registry_bytes();
    let missing = shim
        .hosts(args!["remove", "11111111-2222-4333-8444-555555555555"])
        .await;
    assert!(!missing.status.success());
    let stderr = text(&missing.stderr);
    assert!(stderr.contains("no host"), "{stderr}");
    assert!(stderr.contains(good), "{stderr}");
    assert!(!stderr.contains('\u{1b}'), "{stderr:?}");
    // The bad one can be named (the shell can type an escape), but not as it is shown.
    assert!(stderr.contains("by hand"), "{stderr}");
    assert_eq!(shim.registry_bytes(), before);

    // With an id that could not be a client process's, so none is told.
    let removed = shim.hosts(args!["remove", bad]).await;
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    let printed = text(&removed.stdout);
    assert!(printed.contains("Removed host"), "{printed}");
    assert!(printed.contains("valid again"), "{printed}");
    // Its route is still good to add again, under a label that is not.
    assert!(
        printed.contains("hosts add-ssh --target='alias'"),
        "{printed}"
    );
    assert!(!printed.contains("--label"), "{printed}");
    assert!(
        !printed.contains('\u{1b}') && !printed.contains('\u{7}'),
        "{printed:?}"
    );
    assert!(!text(&removed.stderr).contains('\u{1b}'));
    let listed = shim.hosts(args!["list", "--json"]).await;
    assert!(listed.status.success(), "{}", text(&listed.stderr));
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed[0], added);
    assert_eq!(listed.as_array().unwrap().len(), 1);

    // One of two bad records: it says the rest is still in the way, and what to do.
    let mut file: Value = serde_json::from_slice(&shim.registry_bytes()).unwrap();
    for name in ["bad one", "bad two"] {
        let mut broken = kept.clone();
        broken["id"] = json!(name);
        file["hosts"].as_array_mut().unwrap().push(broken);
    }
    std::fs::write(shim.registry_path(), file.to_string()).unwrap();
    let removed = shim.hosts(args!["remove", "bad one"]).await;
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    assert!(!text(&removed.stdout).contains("valid again"));
    let stderr = text(&removed.stderr);
    assert!(stderr.contains("still invalid"), "{stderr}");
    assert!(stderr.contains("bad two"), "{stderr}");
    assert!(stderr.contains("hosts remove"), "{stderr}");
}
