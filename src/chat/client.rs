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

/// Why a request failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The host read the request and said no, with this reason. It did nothing.
    Refused(String),
    /// The exchange broke (the connection, or what the host sent back): nobody
    /// can tell whether the host acted on the request.
    Broken(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Refused(message) | Self::Broken(message)) = self;
        f.write_str(message)
    }
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
        self.exchange(request).map_err(|error| match error {
            CallError::Refused(message) | CallError::Broken(message) => message,
        })
    }

    /// One request and its answer, telling a refusal from a broken exchange.
    fn exchange(&mut self, request: Request) -> Result<Option<serde_json::Value>, CallError> {
        let broken = |message: String| CallError::Broken(message);
        let attachment_variant = match &request {
            Request::StageAttachment { .. } => Some("stage_attachment"),
            Request::Command {
                command: ChatCommand::SendAttachments { .. },
                ..
            } => Some("send_attachments"),
            _ => None,
        };
        let id = request_id(&request).to_owned();
        let mut line =
            serde_json::to_string(&request).map_err(|error| broken(error.to_string()))?;
        line.push('\n');
        self.stream
            .get_mut()
            .write_all(line.as_bytes())
            .map_err(|error| broken(format!("chat host: {error}")))?;
        let mut answer = String::new();
        if self
            .stream
            .read_line(&mut answer)
            .map_err(|error| broken(format!("chat host: {error}")))?
            == 0
        {
            return Err(broken("chat host closed the connection".into()));
        }
        let response: Response = serde_json::from_str(&answer)
            .map_err(|error| broken(format!("chat host answered something unreadable: {error}")))?;
        if response.id != id {
            return Err(broken("chat host answered another request".into()));
        }
        if response.ok {
            Ok(response.result)
        } else {
            let message = response
                .error
                .unwrap_or_else(|| "chat host refused the request".into());
            if message.starts_with(super::attachments::UNKNOWN_SUBMISSION) {
                Err(CallError::Broken(message))
            } else {
                Err(attachment_refusal(message, attachment_variant))
            }
        }
    }

    pub fn create(&mut self, chat: NewChat) -> Result<ChatInfo, String> {
        let result = self.call(Request::Create {
            id: new_id(),
            chat,
            chat_id: None,
        })?;
        decode(result)
    }

    pub fn create_identified(
        &mut self,
        chat_id: &str,
        chat: NewChat,
    ) -> Result<ChatInfo, CallError> {
        let result = self.exchange(Request::Create {
            id: new_id(),
            chat,
            chat_id: Some(chat_id.into()),
        })?;
        decode(result).map_err(CallError::Broken)
    }

    /// Inspect this connection before any caller-owned creation. A refusal or
    /// broken exchange is never permission to try Create on a legacy host.
    pub fn supports_identified_create(&mut self, wait: Duration) -> Result<bool, CallError> {
        let stream = self.stream.get_ref();
        let read = stream
            .read_timeout()
            .map_err(|e| CallError::Broken(e.to_string()))?;
        let write = stream
            .write_timeout()
            .map_err(|e| CallError::Broken(e.to_string()))?;
        let timeout = Some(wait.max(Duration::from_millis(1)));
        stream
            .set_read_timeout(timeout)
            .map_err(|e| CallError::Broken(e.to_string()))?;
        stream
            .set_write_timeout(timeout)
            .map_err(|e| CallError::Broken(e.to_string()))?;
        let result = self.exchange(Request::Capabilities { id: new_id() });
        let stream = self.stream.get_ref();
        stream
            .set_read_timeout(read)
            .map_err(|e| CallError::Broken(e.to_string()))?;
        stream
            .set_write_timeout(write)
            .map_err(|e| CallError::Broken(e.to_string()))?;
        let capabilities: super::wire::Capabilities = decode(result?).map_err(CallError::Broken)?;
        Ok(capabilities.identified_create)
    }

    /// No fallback on old hosts. The source path belongs to the host's local filesystem.
    pub fn stage_attachment(
        &mut self,
        chat_id: &str,
        path: &Path,
    ) -> Result<super::attachments::Attachment, CallError> {
        let value = self.exchange(Request::StageAttachment {
            id: new_id(),
            chat_id: chat_id.into(),
            path: path.into(),
        })?;
        decode(value).map_err(CallError::Broken)
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

    /// `command`, for a caller that must know whether the host can have acted on
    /// it: a refusal means it did not, a broken exchange leaves that open.
    pub fn command_checked(
        &mut self,
        chat_id: &str,
        command: ChatCommand,
    ) -> Result<(), CallError> {
        self.exchange(Request::Command {
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

/// Only a matched negative response establishes that a legacy host did nothing.
/// Transport failures, malformed replies and unknown submissions stay Broken.
fn attachment_refusal(message: String, variant: Option<&str>) -> CallError {
    if variant.is_some_and(|variant| {
        message.starts_with(&format!("unreadable request: unknown variant `{variant}`"))
    }) {
        CallError::Refused(format!(
            "The running chat backend does not support attachments. Update or refresh the chat backend, then explicitly retry staging or resend the saved draft. Your attachments and text draft are retained. Host refusal: {message}"
        ))
    } else {
        CallError::Refused(message)
    }
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn request_id(request: &Request) -> &str {
    match request {
        Request::Capabilities { id }
        | Request::StageAttachment { id, .. }
        | Request::Create { id, .. }
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
            parent_id: None,
            user_title: None,
            first_user_message: None,
            provider_title: None,
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
            orchestrator: None,
            fast: false,
            carried_over: None,
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
                parent_id: None,
                provider: Provider::Codex,
                project_id: None,
                worktree_id: None,
                cwd: dir.clone(),
                codex_account_id: None,
                title: None,
                approval_mode: ApprovalMode::Supervised,
                model: None,
                effort: None,
                orchestrator: None,
                fast: false,
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

    #[test]
    fn legacy_attachment_refusal_is_actionable_and_plain_send_is_unchanged() {
        #[derive(serde::Deserialize)]
        #[serde(tag = "command", rename_all = "snake_case")]
        enum OldCommand {
            Send { text: String },
        }
        #[derive(serde::Deserialize)]
        #[serde(tag = "op", rename_all = "snake_case")]
        enum OldRequest {
            Create,
            List,
            Command { command: OldCommand },
            Subscribe,
            Close,
            Delete,
        }
        let home = crate::chat::testing::private_socket_fixture_home();
        let socket = home.join("legacy.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let serving = thread::spawn(move || {
            let mut stream = crate::chat::testing::bounded_fixture_accept(&listener);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut requests = Vec::new();
            for _ in 0..3 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let value: serde_json::Value = serde_json::from_str(&line).unwrap();
                let error = match serde_json::from_str::<OldRequest>(&line) {
                    Ok(OldRequest::Command {
                        command: OldCommand::Send { text },
                    }) => {
                        assert_eq!(text, "  plain text unchanged 🦀\n");
                        None
                    }
                    Err(error) => Some(format!("unreadable request: {error}")),
                    _ => panic!("unexpected operation"),
                };
                let response = Response {
                    id: value["id"].as_str().unwrap().into(),
                    ok: error.is_none(),
                    result: None,
                    error,
                };
                writeln!(stream, "{}", serde_json::to_string(&response).unwrap()).unwrap();
                requests.push(value);
            }
            requests
        });
        let mut client = Client::connect(&socket).unwrap();
        let error = client
            .stage_attachment("fixture-chat", &home.join("unread-source.png"))
            .unwrap_err();
        let CallError::Refused(message) = error else {
            panic!("matched legacy refusal must stay Refused")
        };
        assert!(message.contains("Update or refresh the chat backend"));
        assert!(message.contains("unknown variant `stage_attachment`"));
        assert!(message.contains("`create`, `list`, `command`, `subscribe`, `close`, `delete`"));
        let error = client
            .command_checked(
                "fixture-chat",
                ChatCommand::SendAttachments {
                    text: "retain exact text 🦀  ".into(),
                    attachments: Vec::new(),
                },
            )
            .unwrap_err();
        let CallError::Refused(message) = error else {
            panic!("matched legacy refusal must stay Refused")
        };
        assert!(message.contains("unknown variant `send_attachments`"));
        assert!(message.contains("draft are retained"));
        client
            .command_checked(
                "fixture-chat",
                ChatCommand::Send {
                    text: "  plain text unchanged 🦀\n".into(),
                },
            )
            .unwrap();
        let requests = serving.join().unwrap();
        assert_eq!(
            requests.len(),
            3,
            "no capability probe, retry or text fallback"
        );
        assert_eq!(requests[0]["op"], "stage_attachment");
        assert_eq!(requests[1]["command"]["command"], "send_attachments");
        assert_eq!(requests[2]["command"]["command"], "send");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn attachment_protocol_diagnostic_never_relabels_ambiguous_or_unrelated_errors() {
        let refusal = "unreadable request: unknown variant `stage_attachment`";
        assert_eq!(
            attachment_refusal(refusal.into(), None),
            CallError::Refused(refusal.into())
        );
        assert_eq!(
            attachment_refusal("quota exceeded".into(), Some("stage_attachment")),
            CallError::Refused("quota exceeded".into())
        );
        let provider_refusal =
            "provider rejected payload containing unknown variant `stage_attachment`";
        assert_eq!(
            attachment_refusal(provider_refusal.into(), Some("stage_attachment")),
            CallError::Refused(provider_refusal.into())
        );
        let home = crate::chat::testing::private_socket_fixture_home();
        let socket = home.join("broken.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let serving = thread::spawn(move || {
            let mut stream = crate::chat::testing::bounded_fixture_accept(&listener);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            for case in 0..4 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let mut response = Response {
                    id: request["id"].as_str().unwrap().into(),
                    ok: false,
                    result: None,
                    error: Some(refusal.into()),
                };
                match case {
                    0 => {
                        response.error = Some(format!(
                            "{} {refusal}",
                            super::super::attachments::UNKNOWN_SUBMISSION
                        ))
                    }
                    1 => response.id = "wrong-request".into(),
                    2 => {
                        writeln!(stream, "{{unreadable reply").unwrap();
                        continue;
                    }
                    3 => break, // Connection lost after reading the request.
                    _ => unreachable!(),
                }
                writeln!(stream, "{}", serde_json::to_string(&response).unwrap()).unwrap();
            }
        });
        let mut client = Client::connect(&socket).unwrap();
        for _ in 0..4 {
            let error = client
                .stage_attachment("fixture-chat", &home.join("unread.png"))
                .unwrap_err();
            assert!(matches!(error, CallError::Broken(_)));
            assert!(!error.to_string().contains("Update or refresh"));
        }
        serving.join().unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}
