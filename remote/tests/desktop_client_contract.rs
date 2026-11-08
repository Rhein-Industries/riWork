//! The desktop's client code (`riwork_engine::remote_hosts`) reading the real daemon's answers.
//! Each side has tests of its own against a stand-in for the other. These are the ones that notice
//! the two disagreeing, say about where `not_sent` sits in a reply: a wrong guess there turns every
//! `offline` into "may have run" and no test on either side would fail. A fixture `ssh` runs the
//! generated remote command locally, as in tests/ssh.rs, so everything past it is the real binary.
#![cfg(unix)]
use riwork_engine::{
    remote_hosts::{Daemons, HostStatus, LinkState, RemoteCli, RemoteError},
    remote_tree::outcome_unknown,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

const REMOTE: &str = env!("CARGO_BIN_EXE_somtus-remote");
/// Goes on to the generated remote command, as a real login does.
const RUN_REMOTE: &str = "#!/bin/sh\nfor arg do last=$arg; done\nexec /bin/sh -c \"$last\"\n";

fn script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

struct Rig {
    dir: tempfile::TempDir,
    remote: PathBuf,
    route: String,
    host_id: String,
    daemons: Daemons,
}

impl Rig {
    /// One registered SSH route. Its daemon starts with the first request to it, so a test can
    /// still change what the next dial finds.
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("ssh")
            .tempdir_in("/tmp")
            .unwrap();
        let root = dir.path();
        let bin = root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        script(&bin.join("ssh"), RUN_REMOTE);
        let host_id = uuid::Uuid::new_v4().to_string();
        // `die` loses the reply after the request has run, as a dropped connection does:
        // the CLI's parent is the `connect` process on the far end of the ssh pipe.
        // `hold` keeps the request running until the file is gone.
        let cli = root.join("cli");
        script(
            &cli,
            &format!(
                r#"#!/usr/bin/env python3
import sys,json,pathlib,os,time
root=pathlib.Path({root:?})
args=sys.argv[1:]
with (root/'calls').open('a') as f: f.write(' '.join(args)+'\n')
if args[0]=='capabilities':
 print(json.dumps({{'v':1,'verifies_shell':True,'chat':True,'native':{{'v':1,'methods':['Hosts.local']}}}}))
elif args[:2]==['native','invoke']:
 print(json.dumps({{'v':1,'value':{{'hostId':{host_id:?}}}}}))
elif args[:2]==['chat','list']:
 if (root/'die').exists():
  os.remove(root/'die')
  os.kill(os.getppid(),9)
 while (root/'hold').exists(): time.sleep(0.02)
 print('[]')
else: print('[]')
"#,
                root = root.to_string_lossy(),
            ),
        );
        let path = std::env::join_paths(
            std::iter::once(bin.clone())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        // The engine runs what it is pointed at with this process's environment, and these
        // tests run side by side, so the home and the fixture `ssh` travel in a wrapper.
        let remote = bin.join("remote");
        script(
            &remote,
            &format!(
                "#!/bin/sh\nRIWORK_HOME='{}' PATH='{}' exec '{REMOTE}' \"$@\"\n",
                root.join("client").display(),
                path.to_string_lossy(),
            ),
        );
        let added = Command::new(&remote)
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
            .arg(root.join("host"))
            .args(["--label", "Fixture", "--json"])
            .output()
            .unwrap();
        assert!(
            added.status.success(),
            "{}",
            String::from_utf8_lossy(&added.stderr)
        );
        let added: Value = serde_json::from_slice(&added.stdout).unwrap();
        Self {
            route: added["id"].as_str().unwrap().to_owned(),
            host_id: added["host_id"].as_str().unwrap().to_owned(),
            daemons: Daemons::new(RemoteCli::at(remote.clone())),
            remote,
            dir,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Every dial from now on fails as ssh does when nothing listens on the far side.
    fn make_unreachable(&self) {
        script(
            &self.file("bin/ssh"),
            "#!/bin/sh\necho 'ssh: connect to host fixture port 22: Connection refused' >&2\nexit 255\n",
        );
    }

    fn call(&self, method: &str, params: Value, within: Duration) -> Result<Value, RemoteError> {
        self.daemons.call(&self.route, method, params, within)
    }

    fn list(&self) -> Result<Value, RemoteError> {
        self.call("chats.list", json!({}), Duration::from_secs(20))
    }

    /// Until the host answers: the link comes up after the first request starts the daemon.
    fn wait_online(&self) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            match self.list() {
                Ok(_) => return,
                Err(RemoteError::Rpc { code, .. }) if code == "offline" => {}
                Err(error) => panic!("{error:?}"),
            }
            assert!(Instant::now() < end, "the host never came online");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn status_with_cause(&self) -> HostStatus {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let status = self.daemons.status(&self.route).unwrap();
            if status.cause.is_some() {
                return status;
            }
            assert!(Instant::now() < end, "the link never named a cause");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// How many times the far side actually ran the list.
    fn lists_run(&self) -> usize {
        std::fs::read_to_string(self.file("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("chat list"))
            .count()
    }
}

impl Drop for Rig {
    /// Removing the route also tells its daemon to quit.
    fn drop(&mut self) {
        let _ = Command::new(&self.remote)
            .args(["hosts", "remove", &self.route])
            .output();
    }
}

#[test]
fn a_reply_lost_with_the_link_counts_as_possibly_run() {
    let rig = Rig::new();
    rig.wait_online();
    let before = rig.lists_run();
    std::fs::write(rig.file("die"), "").unwrap();
    let error = rig.list().unwrap_err();
    assert!(matches!(error, RemoteError::Lost(_)), "{error:?}");
    assert!(outcome_unknown(&error), "{error:?}");
    assert_eq!(rig.lists_run(), before + 1, "it did run");
    // The route is fine: the next request goes through a new session.
    rig.wait_online();
}

#[test]
fn a_call_while_the_host_cannot_be_reached_is_known_not_to_have_been_sent() {
    let rig = Rig::new();
    rig.make_unreachable();
    let status = rig.status_with_cause();
    assert_eq!(status.state, LinkState::Offline, "{status:?}");
    assert_eq!(status.cause.as_deref(), Some("unreachable"), "{status:?}");
    assert!(!status.needs_action, "{status:?}");
    let error = rig.list().unwrap_err();
    let RemoteError::Rpc { code, message } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(code, "offline", "{error:?}");
    // Only fixed words: what ssh printed names the host.
    assert!(!message.contains("fixture"), "{error:?}");
    assert!(!outcome_unknown(&error), "{error:?}");
    assert_eq!(rig.lists_run(), 0);
}

#[test]
fn a_host_only_a_person_can_fix_reads_as_needing_setup() {
    let rig = Rig::new();
    rig.wait_online();
    // Another installation behind the same address, seen by the running session too.
    std::fs::write(
        rig.file("host/host-identity"),
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    let status = rig.status_with_cause();
    assert_eq!(status.state, LinkState::Offline, "{status:?}");
    assert_eq!(
        status.cause.as_deref(),
        Some("identity_changed"),
        "{status:?}"
    );
    assert!(status.needs_action, "{status:?}");
    let before = rig.lists_run();
    let error = rig.list().unwrap_err();
    let RemoteError::NeedsSetup { cause, .. } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(cause, "identity_changed", "{error:?}");
    assert!(!outcome_unknown(&error), "{error:?}");
    assert_eq!(rig.lists_run(), before);
}

#[test]
fn an_answer_that_takes_too_long_counts_as_possibly_run() {
    let rig = Rig::new();
    rig.wait_online();
    std::fs::write(rig.file("hold"), "").unwrap();
    let error = rig
        .call("chats.list", json!({}), Duration::from_millis(300))
        .unwrap_err();
    assert_eq!(error, RemoteError::Timeout);
    assert!(outcome_unknown(&error));
    std::fs::remove_file(rig.file("hold")).unwrap();
    rig.wait_online();
}

/// What the agent overview takes for "this host needs an update" rather than for a dead link.
#[test]
fn an_older_host_reads_as_unsupported() {
    let rig = Rig::new();
    rig.wait_online();
    let unknown = rig
        .call(
            "native.invoke",
            json!({"module":"NotYetInvented","method":"anything","args":{}}),
            Duration::from_secs(20),
        )
        .unwrap_err();
    assert!(unknown.is_unsupported(), "{unknown:?}");
    assert!(!outcome_unknown(&unknown), "{unknown:?}");
}

#[test]
fn the_registry_reads_back_with_what_the_overview_needs() {
    let rig = Rig::new();
    let listed = RemoteCli::at(rig.remote.clone())
        .hosts_with_transport()
        .unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].host.id, rig.route);
    assert_eq!(listed[0].host.label, "Fixture");
    assert_eq!(listed[0].transport, "ssh");
    assert!(listed[0].added_at.is_some(), "{listed:?}");
    // The overview holds an SSH route's inventory to the identity it was added for.
    assert_eq!(listed[0].host_id.as_deref(), Some(rig.host_id.as_str()));
}

#[test]
fn a_status_read_starts_the_link_and_it_comes_online() {
    let rig = Rig::new();
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        let status = rig.daemons.status(&rig.route).unwrap();
        if status.state == LinkState::Online {
            assert_eq!(status.cause, None, "{status:?}");
            assert!(!status.needs_action, "{status:?}");
            break;
        }
        assert!(Instant::now() < end, "{status:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let local = rig
        .call(
            "native.invoke",
            json!({"module":"Hosts","method":"local","args":{}}),
            Duration::from_secs(20),
        )
        .unwrap();
    assert_eq!(local["value"]["hostId"], rig.host_id.as_str());
}
