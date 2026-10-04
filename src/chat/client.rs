//! A blocking client of the chat host: what a window (and the CLI) uses to
//! create chats, send commands and follow a chat's events.
//!
//! Plain std sockets and threads, no async runtime: a window runs these calls on
//! its background executor and a subscription on a thread of its own.

use super::model::{ChatCommand, ChatInfo, NewChat};
use super::wire::{Envelope, Request, Response};
use std::io::{self, BufRead, BufReader, Write};
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
///
/// `next_within` reads with a deadline instead, for a caller (the CLI's
/// `chat events`) that collects what arrives in a time window.
pub struct Subscription {
    stream: BufReader<UnixStream>,
    /// The bytes of a line that has begun and not ended. A read that times out
    /// keeps what it got here, so no byte of an event is lost between calls.
    pending: Vec<u8>,
    /// The read timeout the socket has now (`None`: block).
    timeout: Option<Duration>,
}

/// What `Subscription::next_within` found.
// An event is read and handled at once; boxing every one only to shrink the
// two variants without one would cost more than it saves.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Poll {
    Event(Envelope),
    /// No whole event arrived in time; a part of one is kept for the next call.
    TimedOut,
    /// The host ended the connection.
    Closed,
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
            pending: Vec::new(),
            timeout: None,
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
        match self.read(None) {
            Ok(Poll::Event(envelope)) => Some(Ok(envelope)),
            Ok(Poll::TimedOut) | Ok(Poll::Closed) => None,
            Err(error) => Some(Err(error)),
        }
    }

    /// The next event if it arrives within `wait`. A zero `wait` is read as one
    /// millisecond: a socket cannot be given a zero timeout. `Err` is an event
    /// that could not be read; the connection is then still usable.
    pub fn next_within(&mut self, wait: Duration) -> Result<Poll, String> {
        self.read(Some(wait.max(Duration::from_millis(1))))
    }

    fn read(&mut self, wait: Option<Duration>) -> Result<Poll, String> {
        if self.timeout != wait {
            match self.stream.get_ref().set_read_timeout(wait) {
                Ok(()) => self.timeout = wait,
                // A socket whose host has gone refuses the change (macOS says
                // EINVAL). What it still holds can be read, and the end after
                // it, without waiting.
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        match self.stream.read_until(b'\n', &mut self.pending) {
            // The host ended the connection, in the middle of a line or not.
            Ok(0) => Ok(Poll::Closed),
            Ok(_) if self.pending.last() == Some(&b'\n') => {
                let line = std::mem::take(&mut self.pending);
                serde_json::from_slice(&line)
                    .map(Poll::Event)
                    .map_err(|error| format!("chat host sent an unreadable event: {error}"))
            }
            // The end of the stream cut a line short.
            Ok(_) => Ok(Poll::Closed),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(Poll::TimedOut)
            }
            Err(_) => Ok(Poll::Closed),
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
    use crate::chat::model::{ApprovalMode, ChatEvent, ChatState, Delta, Provider};
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
                codex_account_id: None,
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

    /// A host stand-in that answers one `Subscribe` and then writes what the
    /// test script hands it: bytes, or a pause.
    fn scripted_subscription(script: Vec<Result<Vec<u8>, Duration>>) -> (Subscription, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rwchat-{}", &new_id()[..8]));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("c.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let Request::Subscribe { id, .. } = serde_json::from_str(&line).unwrap() else {
                panic!("expected a subscribe");
            };
            let response = Response {
                id,
                ok: true,
                result: None,
                error: None,
            };
            writeln!(writer, "{}", serde_json::to_string(&response).unwrap()).unwrap();
            for step in script {
                match step {
                    Ok(bytes) => {
                        writer.write_all(&bytes).unwrap();
                        writer.flush().unwrap();
                    }
                    Err(pause) => thread::sleep(pause),
                }
            }
        });
        (Subscription::open(&socket, "chat-1", 0).unwrap(), dir)
    }

    fn envelope_line(seq: u64, text: &str) -> Vec<u8> {
        let envelope = Envelope {
            chat_id: "chat-1".into(),
            seq,
            event: ChatEvent::ItemDelta {
                item_id: "agent-1".into(),
                delta: Delta::Text(text.into()),
            },
        };
        format!("{}\n", serde_json::to_string(&envelope).unwrap()).into_bytes()
    }

    #[test]
    fn a_read_with_a_deadline_times_out_and_then_still_gets_the_event() {
        let (mut subscription, dir) = scripted_subscription(vec![
            Err(Duration::from_millis(300)),
            Ok(envelope_line(1, "late")),
            Err(Duration::from_millis(300)),
        ]);
        let started = std::time::Instant::now();
        assert!(matches!(
            subscription.next_within(Duration::from_millis(20)),
            Ok(Poll::TimedOut)
        ));
        assert!(started.elapsed() < Duration::from_millis(250));
        let Ok(Poll::Event(first)) = subscription.next_within(Duration::from_secs(10)) else {
            panic!("the event never came");
        };
        assert_eq!(first.seq, 1);
        // A zero wait is a short one, not an error.
        assert!(matches!(
            subscription.next_within(Duration::ZERO),
            Ok(Poll::TimedOut)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_line_split_across_a_timeout_arrives_whole() {
        // The cut falls inside the two-byte character of the text.
        let line = envelope_line(7, "caf\u{e9} au lait");
        let cut = line.iter().position(|byte| *byte == 0xc3).unwrap() + 1;
        let (mut subscription, dir) = scripted_subscription(vec![
            Ok(line[..cut].to_vec()),
            Err(Duration::from_millis(300)),
            Ok(line[cut..].to_vec()),
        ]);
        assert!(matches!(
            subscription.next_within(Duration::from_millis(50)),
            Ok(Poll::TimedOut)
        ));
        let Ok(Poll::Event(envelope)) = subscription.next_within(Duration::from_secs(10)) else {
            panic!("the event never came");
        };
        assert_eq!(envelope.seq, 7);
        assert_eq!(
            envelope.event,
            ChatEvent::ItemDelta {
                item_id: "agent-1".into(),
                delta: Delta::Text("caf\u{e9} au lait".into())
            }
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_end_of_the_stream_closes_the_subscription_for_both_reads() {
        let (mut subscription, dir) = scripted_subscription(vec![
            Ok(envelope_line(1, "last")),
            // The host goes away in the middle of a line.
            Ok(b"{\"chat_id\":\"chat".to_vec()),
        ]);
        let first = subscription.next_within(Duration::from_secs(10));
        assert!(matches!(first, Ok(Poll::Event(_))), "{first:?}");
        assert!(matches!(
            subscription.next_within(Duration::from_secs(10)),
            Ok(Poll::Closed)
        ));
        assert!(subscription.next_envelope().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_blocking_read_after_a_deadline_blocks_again() {
        let (mut subscription, dir) = scripted_subscription(vec![
            Err(Duration::from_millis(200)),
            Ok(envelope_line(1, "after the pause")),
        ]);
        assert!(matches!(
            subscription.next_within(Duration::from_millis(20)),
            Ok(Poll::TimedOut)
        ));
        // `next_envelope` waits as long as it takes, as it always did.
        assert_eq!(subscription.next_envelope().unwrap().unwrap().seq, 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
