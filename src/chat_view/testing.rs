//! A stand-in for the chat host in tests: a Unix socket that speaks `chat::wire` the way the
//! real host must (a list, commands, a subscription that replays from `since` and then
//! follows), with knobs to push events, drop every connection as a host restart does, and
//! delete a chat.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::chat::{
    model::{ApprovalMode, ChatCommand, ChatEvent, ChatInfo, ChatState, Provider},
    wire::{Request, Response},
};

struct Shared {
    /// The events of each chat the host knows, as JSON; an event's `seq` is its place plus one.
    chats: Mutex<BTreeMap<String, Vec<serde_json::Value>>>,
    wake: Condvar,
    commands: Mutex<Vec<(String, ChatCommand)>>,
    subscribes: Mutex<Vec<(String, u64)>>,
    /// Every connection accepted, to hang up on.
    streams: Mutex<Vec<UnixStream>>,
    quit: AtomicBool,
}

pub struct FakeHost {
    pub socket: PathBuf,
    dir: PathBuf,
    shared: Arc<Shared>,
    acceptor: Option<JoinHandle<()>>,
}

impl FakeHost {
    pub fn with_chats(chats: &[&str]) -> Self {
        let dir =
            std::env::temp_dir().join(format!("rwfake-{}", &uuid::Uuid::new_v4().to_string()[..8]));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("c.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let shared = Arc::new(Shared {
            chats: Mutex::new(
                chats
                    .iter()
                    .map(|id| ((*id).to_owned(), Vec::new()))
                    .collect(),
            ),
            wake: Condvar::new(),
            commands: Mutex::new(Vec::new()),
            subscribes: Mutex::new(Vec::new()),
            streams: Mutex::new(Vec::new()),
            quit: AtomicBool::new(false),
        });
        let acceptor = {
            let shared = shared.clone();
            thread::spawn(move || {
                while !shared.quit.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            shared
                                .streams
                                .lock()
                                .unwrap()
                                .push(stream.try_clone().unwrap());
                            let shared = shared.clone();
                            thread::spawn(move || serve(stream, &shared));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(2)),
                    }
                }
            })
        };
        Self {
            socket,
            dir,
            shared,
            acceptor: Some(acceptor),
        }
    }

    /// Something happened in `chat`.
    pub fn push(&self, chat: &str, event: ChatEvent) {
        self.push_raw(chat, serde_json::to_value(event).unwrap());
    }

    /// Something happened in `chat` that is written as `event`, readable or not.
    pub fn push_raw(&self, chat: &str, event: serde_json::Value) {
        self.shared
            .chats
            .lock()
            .unwrap()
            .get_mut(chat)
            .expect("a chat the host knows")
            .push(event);
        self.shared.wake.notify_all();
    }

    /// Hang up on everyone, as a host that restarts does. It is listening again at once.
    pub fn drop_connections(&self) {
        for stream in self.shared.streams.lock().unwrap().drain(..) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// The host forgets `chat` and hangs up.
    pub fn delete(&self, chat: &str) {
        self.shared.chats.lock().unwrap().remove(chat);
        self.drop_connections();
        self.shared.wake.notify_all();
    }

    /// Every command received, with its chat.
    pub fn commands(&self) -> Vec<(String, ChatCommand)> {
        self.shared.commands.lock().unwrap().clone()
    }

    /// Every subscription asked for: the chat and the `since` it gave.
    pub fn subscribes(&self) -> Vec<(String, u64)> {
        self.shared.subscribes.lock().unwrap().clone()
    }
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::SeqCst);
        self.drop_connections();
        self.shared.wake.notify_all();
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn info(id: &str) -> ChatInfo {
    ChatInfo {
        id: id.into(),
        provider: Provider::Codex,
        project_id: None,
        worktree_id: None,
        cwd: "/work".into(),
        title: "Test chat".into(),
        created_at_unix: 1,
        provider_thread_id: None,
        model: None,
        effort: None,
        approval_mode: ApprovalMode::Supervised,
        codex_account_id: None,
        state: ChatState::Idle,
        orchestrator: None,
        fast: false,
    }
}

fn reply(stream: &mut UnixStream, response: Response) -> bool {
    let mut line = serde_json::to_string(&response).unwrap();
    line.push('\n');
    stream.write_all(line.as_bytes()).is_ok()
}

fn serve(stream: UnixStream, shared: &Shared) {
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
            return;
        }
        let Ok(request) = serde_json::from_str::<Request>(&line) else {
            return;
        };
        let known = |chat: &str| shared.chats.lock().unwrap().contains_key(chat);
        let ok = |id: String, result: Option<serde_json::Value>| Response {
            id,
            ok: true,
            result,
            error: None,
        };
        let refused = |id: String| Response {
            id,
            ok: false,
            result: None,
            error: Some("unknown chat".into()),
        };
        let alive = match request {
            Request::List { id } => {
                let chats: Vec<_> = shared
                    .chats
                    .lock()
                    .unwrap()
                    .keys()
                    .map(|id| info(id))
                    .collect();
                reply(
                    &mut writer,
                    ok(id, Some(serde_json::to_value(chats).unwrap())),
                )
            }
            Request::Command {
                id,
                chat_id,
                command,
            } => {
                if known(&chat_id) {
                    shared.commands.lock().unwrap().push((chat_id, command));
                    reply(&mut writer, ok(id, None))
                } else {
                    reply(&mut writer, refused(id))
                }
            }
            Request::Subscribe { id, chat_id, since } => {
                if !known(&chat_id) {
                    reply(&mut writer, refused(id))
                } else {
                    shared
                        .subscribes
                        .lock()
                        .unwrap()
                        .push((chat_id.clone(), since));
                    if reply(&mut writer, ok(id, None)) {
                        stream_events(&mut writer, shared, &chat_id, since);
                    }
                    return;
                }
            }
            Request::Close { id, .. } => reply(&mut writer, ok(id, None)),
            Request::Delete { id, chat_id } => {
                shared.chats.lock().unwrap().remove(&chat_id);
                reply(&mut writer, ok(id, None))
            }
            Request::Create { id, .. } => reply(&mut writer, refused(id)),
        };
        if !alive {
            return;
        }
    }
}

/// Send the chat's events after `since`, then each new one, until the connection or the
/// chat ends.
fn stream_events(writer: &mut UnixStream, shared: &Shared, chat: &str, since: u64) {
    let mut sent = since as usize;
    let mut chats = shared.chats.lock().unwrap();
    loop {
        let Some(events) = chats.get(chat) else {
            return;
        };
        let fresh: Vec<_> = events
            .iter()
            .enumerate()
            .skip(sent)
            .map(|(at, event)| (at, event.clone()))
            .collect();
        for (at, event) in fresh {
            let envelope = serde_json::json!({
                "chat_id": chat,
                "seq": at as u64 + 1,
                "event": event,
            });
            let mut line = envelope.to_string();
            line.push('\n');
            if writer.write_all(line.as_bytes()).is_err() {
                return;
            }
            sent = at + 1;
        }
        if shared.quit.load(Ordering::SeqCst) {
            return;
        }
        chats = shared
            .wake
            .wait_timeout(chats, Duration::from_millis(50))
            .unwrap()
            .0;
    }
}
