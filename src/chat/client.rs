//! A blocking client of the chat host: what a window (and the CLI) uses to
//! create chats, send commands and follow a chat's events.
//!
//! Plain std sockets and threads, no async runtime: a window runs these calls on
//! its background executor and a subscription on a thread of its own.

use super::model::{ChatCommand, ChatInfo, NewChat};
use super::wire::{Envelope, Request, Response};
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// Where the chat host listens for `home` (a RiWork data directory).
pub fn socket_path(home: &Path) -> PathBuf {
    home.join("run").join("chat.sock")
}

/// A connection for requests. Cheap to open; open one per burst of requests.
pub struct Client {
    stream: BufReader<UnixStream>,
}

impl Client {
    pub fn connect(socket: &Path) -> Result<Self, String> {
        let stream = UnixStream::connect(socket)
            .map_err(|error| format!("chat host at {}: {error}", socket.display()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            stream: BufReader::new(stream),
        })
    }

    fn call(&mut self, request: Request) -> Result<Option<serde_json::Value>, String> {
        let id = request_id(&request).to_owned();
        let mut line = serde_json::to_string(&request).map_err(|error| error.to_string())?;
        line.push('\n');
        self.stream
            .get_mut()
            .write_all(line.as_bytes())
            .map_err(|error| format!("chat host: {error}"))?;
        let mut answer = String::new();
        if self
            .stream
            .read_line(&mut answer)
            .map_err(|error| format!("chat host: {error}"))?
            == 0
        {
            return Err("chat host closed the connection".into());
        }
        let response: Response = serde_json::from_str(&answer)
            .map_err(|error| format!("chat host answered something unreadable: {error}"))?;
        if response.id != id {
            return Err("chat host answered another request".into());
        }
        if response.ok {
            Ok(response.result)
        } else {
            Err(response
                .error
                .unwrap_or_else(|| "chat host refused the request".into()))
        }
    }

    pub fn create(&mut self, chat: NewChat) -> Result<ChatInfo, String> {
        let result = self.call(Request::Create { id: new_id(), chat })?;
        decode(result)
    }

    pub fn list(&mut self) -> Result<Vec<ChatInfo>, String> {
        decode(self.call(Request::List { id: new_id() })?)
    }

    pub fn command(&mut self, chat_id: &str, command: ChatCommand) -> Result<(), String> {
        self.call(Request::Command {
            id: new_id(),
            chat_id: chat_id.into(),
            command,
        })
        .map(drop)
    }

    pub fn close(&mut self, chat_id: &str) -> Result<(), String> {
        self.call(Request::Close {
            id: new_id(),
            chat_id: chat_id.into(),
        })
        .map(drop)
    }

    pub fn delete(&mut self, chat_id: &str) -> Result<(), String> {
        self.call(Request::Delete {
            id: new_id(),
            chat_id: chat_id.into(),
        })
        .map(drop)
    }
}

/// A chat's events from `since` on, oldest first, then live. Blocks; run it on
/// a thread of its own. Ends (`None`) when the host goes away.
pub struct Subscription {
    stream: BufReader<UnixStream>,
}

impl Subscription {
    pub fn open(socket: &Path, chat_id: &str, since: u64) -> Result<Self, String> {
        let mut client = Client::connect(socket)?;
        // Live events can be minutes apart: no read timeout once subscribed.
        client
            .stream
            .get_ref()
            .set_read_timeout(None)
            .map_err(|error| error.to_string())?;
        client.call(Request::Subscribe {
            id: new_id(),
            chat_id: chat_id.into(),
            since,
        })?;
        Ok(Self {
            stream: client.stream,
        })
    }

    /// A handle that ends this subscription from another thread, so a window
    /// that closes its tab does not leave a thread blocked on a quiet chat.
    pub fn closer(&self) -> Result<SubscriptionCloser, String> {
        self.stream
            .get_ref()
            .try_clone()
            .map(SubscriptionCloser)
            .map_err(|error| error.to_string())
    }

    /// The next event, or `None` when the connection ended.
    pub fn next_envelope(&mut self) -> Option<Result<Envelope, String>> {
        let mut line = String::new();
        match self.stream.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(
                serde_json::from_str(&line)
                    .map_err(|error| format!("chat host sent an unreadable event: {error}")),
            ),
        }
    }
}

/// Ends a `Subscription` from another thread: its blocked `next_envelope`
/// returns `None`.
pub struct SubscriptionCloser(UnixStream);

impl SubscriptionCloser {
    pub fn close(&self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn request_id(request: &Request) -> &str {
    match request {
        Request::Create { id, .. }
        | Request::List { id }
        | Request::Command { id, .. }
        | Request::Subscribe { id, .. }
        | Request::Close { id, .. }
        | Request::Delete { id, .. } => id,
    }
}

fn decode<T: serde::de::DeserializeOwned>(result: Option<serde_json::Value>) -> Result<T, String> {
    serde_json::from_value(result.unwrap_or(serde_json::Value::Null))
        .map_err(|error| format!("chat host answered something unreadable: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ApprovalMode, ChatEvent, ChatState, Provider};
    use std::os::unix::net::UnixListener;
    use std::thread;

    /// A host stand-in that answers each request the way the real host must.
    #[test]
    fn requests_match_responses_and_a_subscription_streams_envelopes() {
        let dir = std::env::temp_dir().join(format!("rwchat-{}", &new_id()[..8]));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("c.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let info = ChatInfo {
            id: "chat-1".into(),
            provider: Provider::Codex,
            project_id: None,
            worktree_id: None,
            cwd: dir.clone(),
            title: "Codex chat".into(),
            created_at_unix: 1,
            provider_thread_id: None,
            model: None,
            effort: None,
            approval_mode: ApprovalMode::Supervised,
            codex_account_id: None,
            state: ChatState::Idle,
        };
        let served = info.clone();
        let host = thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Request = serde_json::from_str(&line).unwrap();
                let (id, result) = match request {
                    Request::Create { id, .. } => (id, serde_json::to_value(&served).unwrap()),
                    Request::Subscribe { id, .. } => (id, serde_json::Value::Null),
                    other => panic!("unexpected {other:?}"),
                };
                let response = Response {
                    id,
                    ok: true,
                    result: Some(result),
                    error: None,
                };
                writeln!(writer, "{}", serde_json::to_string(&response).unwrap()).unwrap();
                let envelope = Envelope {
                    chat_id: "chat-1".into(),
                    seq: 1,
                    event: ChatEvent::State {
                        state: ChatState::Running,
                    },
                };
                writeln!(writer, "{}", serde_json::to_string(&envelope).unwrap()).unwrap();
            }
        });
        let mut client = Client::connect(&socket).unwrap();
        let created = client
            .create(NewChat {
                provider: Provider::Codex,
                project_id: None,
                worktree_id: None,
                cwd: dir.clone(),
                title: None,
                approval_mode: ApprovalMode::Supervised,
                model: None,
                effort: None,
            })
            .unwrap();
        assert_eq!(created, info);
        let mut subscription = Subscription::open(&socket, "chat-1", 0).unwrap();
        let first = subscription.next_envelope().unwrap().unwrap();
        assert_eq!(first.seq, 1);
        host.join().unwrap();
        assert!(subscription.next_envelope().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
