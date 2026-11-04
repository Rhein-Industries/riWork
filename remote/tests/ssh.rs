//! No network service or credentials: a fixture `ssh` runs the generated remote
//! command locally. Everything after that boundary is the real binary/RPC/PTY.
#![cfg(unix)]
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use riwork_remote::{client_daemon, config::Storage};
use serde_json::{Value, json};
use std::{
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

const REMOTE: &str = env!("CARGO_BIN_EXE_somtus-remote");

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
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.socket.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
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
    rig.stop().await;
}
