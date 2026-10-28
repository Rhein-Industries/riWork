//! A chat handle's two threads on the chat host's socket: one follows the chat's
//! events, one delivers commands in order. Carried from the uncompiled GPUI
//! `chat_view/feed.rs` (parity CHATT-001..014) without GPUI types.
//!
//! The follower asks for events after the last one it handed over. When the
//! connection ends it resolves its follow address and subscribes again from that
//! point, so the transcript never misses or repeats an event. A chat the host no
//! longer lists twice in a row was deleted, and the follower ends. Both threads
//! end when the `Feed` is dropped; a subscription blocked on a quiet chat is
//! closed from outside, so no thread stays parked on the socket.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use riwork_engine::chat::{
    client::{CallError, Client, Subscription, SubscriptionCloser},
    model::{ChatCommand, ChatEvent, Item, ItemBody, ItemStatus, NoticeLevel},
    wire::Envelope,
};

/// How the handle is connected to the chat host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Link {
    #[default]
    Connecting,
    Live,
    /// The connection ended (the host restarted or went away); a new one is being made.
    Reconnecting,
    /// The host does not know the chat any more.
    Deleted,
}

impl Link {
    pub fn name(self) -> &'static str {
        match self {
            Link::Connecting => "connecting",
            Link::Live => "live",
            Link::Reconnecting => "reconnecting",
            Link::Deleted => "deleted",
        }
    }
}

/// What the threads tell the handle's worker.
#[derive(Debug)]
pub enum FeedMsg {
    /// Events, in order, none of them seen before.
    Events(Vec<Envelope>),
    Link(Link),
    /// A fire-and-forget command could not be delivered, and why.
    CommandFailed {
        command: ChatCommand,
        error: String,
    },
}

/// Resolves the host's socket. Following history never starts a host; explicit
/// delivery may (CHATT-013).
pub type Ensure = Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>;

/// Where the threads post: the handle's worker. `false` means it is gone.
pub type Sink = Arc<dyn Fn(FeedMsg) -> bool + Send + Sync>;

/// One exchange's answer, handed back to an awaiting JS promise as a frozen
/// error code: `refused:` (the host did nothing) or `broken:` (unknown).
pub type Receipt = tokio::sync::oneshot::Sender<Result<(), String>>;

pub enum Delivery {
    /// Generic command: retried once on a fresh connection when the old one broke
    /// (CHATT-011). Without a receipt a failure is reported to the worker.
    Command {
        command: ChatCommand,
        receipt: Option<Receipt>,
    },
    /// Exactly one exchange on a fresh connection; neither a refusal nor a broken
    /// reply is ever resent automatically (CHATC-034).
    Submission {
        command: ChatCommand,
        receipt: Receipt,
    },
}

pub fn call_error(error: CallError) -> String {
    match error {
        CallError::Refused(message) => format!("refused: {message}"),
        CallError::Broken(message) => format!("broken: {message}"),
    }
}

/// How long to wait before the next attempt to reach the host (CHATT-004).
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    pub first: Duration,
    pub longest: Duration,
}

impl Backoff {
    pub const DEFAULT: Self = Self {
        first: Duration::from_millis(500),
        longest: Duration::from_secs(5),
    };

    /// Doubles with each failed attempt in a row; the first retry is `first`.
    pub fn delay(self, failures: u32) -> Duration {
        self.first
            .saturating_mul(1u32 << failures.saturating_sub(1).min(16))
            .min(self.longest)
    }
}

/// Where an event stands against the ones already handed over (CHATT-002).
#[derive(Debug, PartialEq, Eq)]
pub enum Sequence {
    Next,
    Repeat,
    Gap,
}

pub fn sequence(last: u64, seq: u64) -> Sequence {
    match seq {
        seq if seq <= last => Sequence::Repeat,
        seq if seq == last + 1 => Sequence::Next,
        _ => Sequence::Gap,
    }
}

#[derive(Clone)]
struct Connections {
    chat_id: String,
    follow: Ensure,
    delivery: Ensure,
}

pub struct Feed {
    stop: Arc<AtomicBool>,
    closer: Arc<Mutex<Option<SubscriptionCloser>>>,
    commands: Option<mpsc::Sender<Delivery>>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Feed {
    /// Follow `chat_id` from the event after `since`, forwarding to `messages`.
    pub fn start(
        delivery: Ensure,
        follow: Ensure,
        chat_id: String,
        since: u64,
        messages: Sink,
        backoff: Backoff,
    ) -> Self {
        let connections = Connections {
            chat_id,
            follow,
            delivery,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let closer = Arc::new(Mutex::new(None));
        let (commands, queue) = mpsc::channel();
        let follower = {
            let (connections, messages) = (connections.clone(), messages.clone());
            let (stop, closer) = (stop.clone(), closer.clone());
            thread::Builder::new()
                .name("riwork-chat-follow".into())
                .spawn(move || follow_chat(&connections, since, &messages, &stop, &closer, backoff))
                .expect("spawn chat follower")
        };
        let deliverer = thread::Builder::new()
            .name("riwork-chat-deliver".into())
            .spawn(move || deliver(&connections, queue, &messages))
            .expect("spawn chat deliverer");
        Self {
            stop,
            closer,
            commands: Some(commands),
            threads: vec![follower, deliverer],
        }
    }

    /// The ordered command queue. Commands reach the host one at a time, in the
    /// order sent; a clone outliving the feed still delivers what it queued.
    pub fn commands(&self) -> Option<mpsc::Sender<Delivery>> {
        self.commands.clone()
    }

    /// End both threads without waiting. Commands already queued are still delivered.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(closer) = self.closer.lock().ok().and_then(|mut slot| slot.take()) {
            closer.close();
        }
        self.commands = None;
    }

    /// Test seam: stop and wait for both threads.
    pub fn join(mut self) {
        self.stop();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop();
    }
}

enum Ended {
    Stopped,
    /// The host answered and does not list the chat.
    Missing,
    /// The connection ended or could not be made; `progress` says whether it
    /// delivered anything first, which restarts the backoff.
    Dropped {
        progress: bool,
    },
}

fn follow_chat(
    connections: &Connections,
    mut since: u64,
    messages: &Sink,
    stop: &AtomicBool,
    closer: &Mutex<Option<SubscriptionCloser>>,
    backoff: Backoff,
) {
    let mut failures = 0u32;
    // A host that has only just started may not have loaded the chat yet, so one
    // miss is not yet a deleted chat (CHATT-005).
    let mut misses = 0u32;
    while !stop.load(Ordering::SeqCst) {
        let (progress, missing) =
            match connect_once(connections, &mut since, messages, stop, closer) {
                Ended::Stopped => return,
                Ended::Missing => (false, true),
                Ended::Dropped { progress } => (progress, false),
            };
        misses = if missing { misses + 1 } else { 0 };
        if misses >= 2 {
            messages(FeedMsg::Link(Link::Deleted));
            return;
        }
        failures = if progress { 1 } else { failures + 1 };
        if !messages(FeedMsg::Link(Link::Reconnecting)) {
            return;
        }
        pause(backoff.delay(failures), stop);
    }
}

fn connect_once(
    connections: &Connections,
    since: &mut u64,
    messages: &Sink,
    stop: &AtomicBool,
    closer: &Mutex<Option<SubscriptionCloser>>,
) -> Ended {
    let Ok(socket) = (connections.follow)() else {
        return Ended::Dropped { progress: false };
    };
    let chat_id = connections.chat_id.as_str();
    let mut subscription = match Subscription::open(&socket, chat_id, *since) {
        Ok(subscription) => subscription,
        Err(_) => {
            let listed = Client::connect(&socket).and_then(|mut client| client.list());
            return match listed {
                Ok(chats) if !chats.iter().any(|chat| chat.id == chat_id) => Ended::Missing,
                _ => Ended::Dropped { progress: false },
            };
        }
    };
    if let Ok(handle) = subscription.closer()
        && let Ok(mut slot) = closer.lock()
    {
        *slot = Some(handle);
    }
    if stop.load(Ordering::SeqCst) {
        return Ended::Stopped;
    }
    if !messages(FeedMsg::Link(Link::Live)) {
        return Ended::Stopped;
    }
    let mut progress = false;
    while let Some(read) = subscription.next_envelope() {
        // An event this build cannot read is the next one in the gapless log; it is
        // skipped with a note, not asked for again for ever (CHATT-006).
        let envelope = read.unwrap_or_else(|_| unreadable(chat_id, *since + 1));
        match sequence(*since, envelope.seq) {
            Sequence::Repeat => continue,
            Sequence::Gap => break,
            Sequence::Next => {}
        }
        *since = envelope.seq;
        progress = true;
        if !messages(FeedMsg::Events(vec![envelope])) {
            return Ended::Stopped;
        }
    }
    if stop.load(Ordering::SeqCst) {
        Ended::Stopped
    } else {
        Ended::Dropped { progress }
    }
}

/// A stand-in for event number `seq` that could not be read.
pub fn unreadable(chat_id: &str, seq: u64) -> Envelope {
    Envelope {
        chat_id: chat_id.to_owned(),
        seq,
        event: ChatEvent::ItemCompleted {
            item: Item {
                started_at: None,
                ended_at: None,
                presentation: Default::default(),
                id: format!("unreadable-{seq}"),
                turn_id: None,
                status: ItemStatus::Completed,
                body: ItemBody::notice(
                    NoticeLevel::Warning,
                    "An event from the chat host could not be read. This RiWork may be older than the host.",
                    None,
                ),
            },
        },
    }
}

/// Sleep for `delay`, or until `stop`. Only between reconnect attempts.
fn pause(delay: Duration, stop: &AtomicBool) {
    let until = Instant::now() + delay;
    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10).min(delay));
    }
}

fn exchange_once(connections: &Connections, command: &ChatCommand) -> Result<(), CallError> {
    (connections.delivery)()
        .map_err(CallError::Broken)
        .and_then(|socket| Client::connect(&socket).map_err(CallError::Broken))
        .and_then(|mut client| client.command_checked(&connections.chat_id, command.clone()))
}

fn deliver(connections: &Connections, queue: mpsc::Receiver<Delivery>, messages: &Sink) {
    let mut client: Option<Client> = None;
    // Ends when the queue is closed and empty: commands sent just before a handle
    // closed are still delivered (CHATT-010).
    for delivery in queue {
        let (command, receipt) = match delivery {
            Delivery::Submission { command, receipt } => {
                let result = exchange_once(connections, &command).map_err(call_error);
                client = None;
                let _ = receipt.send(result);
                continue;
            }
            Delivery::Command { command, receipt } => (command, receipt),
        };
        // A connection kept from an earlier command may have been the old host's:
        // one fresh attempt before giving up (CHATT-011).
        let mut result = Err("no connection".to_owned());
        for _ in 0..2 {
            if client.is_none() {
                client = (connections.delivery)()
                    .and_then(|socket| Client::connect(&socket))
                    .ok();
            }
            let Some(connected) = client.as_mut() else {
                continue;
            };
            result = connected.command(&connections.chat_id, command.clone());
            match &result {
                Ok(()) => break,
                Err(error) if !connection_broke(error) => break,
                Err(_) => client = None,
            }
        }
        match receipt {
            Some(receipt) => {
                let _ = receipt.send(result.map_err(|error| {
                    if connection_broke(&error) {
                        format!("broken: {error}")
                    } else {
                        format!("refused: {error}")
                    }
                }));
            }
            None => {
                if let Err(error) = result {
                    messages(FeedMsg::CommandFailed { command, error });
                }
            }
        }
    }
}

/// Whether an error says the connection was gone before the host could answer.
pub fn connection_broke(error: &str) -> bool {
    ["Broken pipe", "closed the connection", "Connection reset"]
        .iter()
        .any(|sign| error.contains(sign))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_waits_500_ms_doubling_to_5_s() {
        let backoff = Backoff::DEFAULT;
        assert_eq!(backoff.delay(1), Duration::from_millis(500));
        assert_eq!(backoff.delay(2), Duration::from_secs(1));
        assert_eq!(backoff.delay(3), Duration::from_secs(2));
        assert_eq!(backoff.delay(4), Duration::from_secs(4));
        assert_eq!(backoff.delay(5), Duration::from_secs(5));
        assert_eq!(backoff.delay(40), Duration::from_secs(5));
    }

    #[test]
    fn events_are_next_repeated_or_missing() {
        assert_eq!(sequence(3, 4), Sequence::Next);
        assert_eq!(sequence(3, 3), Sequence::Repeat);
        assert_eq!(sequence(3, 1), Sequence::Repeat);
        assert_eq!(sequence(3, 6), Sequence::Gap);
    }

    #[test]
    fn only_a_connection_found_broken_is_worth_a_second_try() {
        assert!(connection_broke("write: Broken pipe (os error 32)"));
        assert!(connection_broke("the chat host closed the connection"));
        assert!(!connection_broke("refused: no such chat"));
        assert!(!connection_broke("timed out"));
    }
}
