//! The tab's two threads on the chat host's socket: one follows the chat's events, one
//! delivers the user's commands in order.
//!
//! The subscription asks for events after the last one it handed over. When the connection
//! ends, it resolves its follow address and subscribes again from that point, so the
//! transcript never misses or repeats an event. Ordinary feeds ensure the host on follow;
//! history browsing connects only to an existing host. Explicit delivery still ensures it. A
//! chat the host no longer lists was deleted, and the subscription ends. Both threads end
//! when the `Feed` is dropped; a subscription blocked on a quiet chat is closed from outside.

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

use crate::chat::{
    client::{CallError, Client, Subscription, SubscriptionCloser},
    model::{ChatCommand, ChatEvent, Item, ItemBody, ItemStatus, NoticeLevel},
    wire::Envelope,
};

use super::state::Link;

/// What the threads tell the tab.
#[derive(Debug)]
pub enum FeedMsg {
    /// Events, in order, none of them seen before.
    Events(Vec<Envelope>),
    /// Attachment submission receipt. Success is submission, not turn completion.
    AttachmentSubmission {
        command: ChatCommand,
        result: Result<(), CallError>,
    },
    /// A single-attempt UI dispatch, correlated to an immutable editor snapshot.
    Submission {
        id: u64,
        command: ChatCommand,
        result: Result<(), CallError>,
    },
    Link(Link),
    /// A command could not be delivered, and why.
    CommandFailed {
        command: ChatCommand,
        error: String,
    },
}

/// Makes sure the chat host is running and says where it listens.
pub type Ensure = Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>;

/// One immutable chat identity with separate follower and explicit-delivery policies.
/// Both transport paths resolve through this seam; browsing cannot select normal ensure.
#[derive(Clone)]
struct Connections {
    chat_id: String,
    follow: Ensure,
    delivery: Ensure,
}

struct Target<'a> {
    socket: PathBuf,
    chat_id: &'a str,
}

impl Connections {
    fn new(delivery: Ensure, follow: Ensure, chat_id: String) -> Self {
        Self {
            chat_id,
            follow,
            delivery,
        }
    }

    fn resolve_follow(&self) -> Result<Target<'_>, String> {
        (self.follow)().map(|socket| Target {
            socket,
            chat_id: &self.chat_id,
        })
    }

    fn resolve_delivery(&self) -> Result<Target<'_>, String> {
        (self.delivery)().map(|socket| Target {
            socket,
            chat_id: &self.chat_id,
        })
    }
}

/// How long to wait before the next attempt to reach the host.
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

/// Where an event stands against the ones already handed over.
#[derive(Debug, PartialEq, Eq)]
pub enum Sequence {
    /// The next one.
    Next,
    /// Already handed over.
    Repeat,
    /// Some are missing: subscribe again from the last one handed over.
    Gap,
}

pub fn sequence(last: u64, seq: u64) -> Sequence {
    match seq {
        seq if seq <= last => Sequence::Repeat,
        seq if seq == last + 1 => Sequence::Next,
        _ => Sequence::Gap,
    }
}

pub(super) enum Delivery {
    Command(ChatCommand),
    Submission { id: u64, command: ChatCommand },
}

pub struct Feed {
    stop: Arc<AtomicBool>,
    closer: Arc<Mutex<Option<SubscriptionCloser>>>,
    commands: Option<mpsc::Sender<Delivery>>,
    /// Kept to wait for the threads in tests; a window never waits for them.
    #[cfg(test)]
    threads: Vec<thread::JoinHandle<()>>,
}

impl Feed {
    /// Follow `chat_id` from the event after `since`.
    pub fn start(
        ensure: Ensure,
        chat_id: String,
        since: u64,
        messages: async_channel::Sender<FeedMsg>,
        backoff: Backoff,
    ) -> Self {
        Self::start_with_follow(ensure.clone(), ensure, chat_id, since, messages, backoff)
    }

    /// Follow with an independent resolver while retaining normal explicit-send recovery.
    pub fn start_with_follow(
        ensure: Ensure,
        follow_address: Ensure,
        chat_id: String,
        since: u64,
        messages: async_channel::Sender<FeedMsg>,
        backoff: Backoff,
    ) -> Self {
        let connections = Connections::new(ensure, follow_address, chat_id);
        let stop = Arc::new(AtomicBool::new(false));
        let closer = Arc::new(Mutex::new(None));
        let (commands, queue) = mpsc::channel();
        let follower = {
            let (connections, messages) = (connections.clone(), messages.clone());
            let (stop, closer) = (stop.clone(), closer.clone());
            thread::spawn(move || follow(&connections, since, &messages, &stop, &closer, backoff))
        };
        let deliverer = thread::spawn(move || deliver(&connections, queue, &messages));
        let _ = (&follower, &deliverer);
        Self {
            stop,
            closer,
            commands: Some(commands),
            #[cfg(test)]
            threads: vec![follower, deliverer],
        }
    }

    /// Deterministic UI/action tests: record queue entries without sockets, threads, or providers.
    #[cfg(test)]
    pub(super) fn recording() -> (Self, mpsc::Receiver<Delivery>) {
        let (commands, queue) = mpsc::channel();
        (
            Self {
                stop: Arc::new(AtomicBool::new(false)),
                closer: Arc::new(Mutex::new(None)),
                commands: Some(commands),
                threads: Vec::new(),
            },
            queue,
        )
    }

    /// Queue a command. Commands reach the host one at a time, in the order sent.
    pub fn send(&self, command: ChatCommand) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Delivery::Command(command));
        }
    }

    /// Unlike legacy generic commands, UI drafts are exchanged exactly once, with a receipt.
    pub fn submit(&self, id: u64, command: ChatCommand) -> Result<(), CallError> {
        self.commands
            .as_ref()
            .ok_or_else(|| CallError::Refused("chat connection is closed".into()))?
            .send(Delivery::Submission { id, command })
            .map_err(|_| CallError::Refused("chat delivery queue is closed".into()))
    }

    /// End both threads. Does not wait for them.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Closing the connection wakes a subscription that is waiting for a quiet chat.
        if let Some(closer) = self.closer.lock().ok().and_then(|mut closer| closer.take()) {
            closer.close();
        }
        self.commands = None;
    }

    #[cfg(test)]
    pub fn join(mut self) {
        self.stop();
        for thread in self.threads.drain(..) {
            thread.join().unwrap();
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Why one connection to the host ended.
enum Ended {
    Stopped,
    /// The host answered, and does not list the chat.
    Missing,
    /// The connection ended or could not be made; `progress` says whether it delivered
    /// anything first, which restarts the backoff.
    Dropped {
        progress: bool,
    },
}

fn follow(
    connections: &Connections,
    mut since: u64,
    messages: &async_channel::Sender<FeedMsg>,
    stop: &AtomicBool,
    closer: &Mutex<Option<SubscriptionCloser>>,
    backoff: Backoff,
) {
    let mut failures = 0u32;
    // Chats the host did not list, in a row. A host that has only just started may not have
    // loaded them yet, so one miss is not yet a deleted chat.
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
            let _ = messages.send_blocking(FeedMsg::Link(Link::Deleted));
            return;
        }
        failures = if progress { 1 } else { failures + 1 };
        if messages
            .send_blocking(FeedMsg::Link(Link::Reconnecting))
            .is_err()
        {
            return;
        }
        pause(backoff.delay(failures), stop);
    }
}

fn connect_once(
    connections: &Connections,
    since: &mut u64,
    messages: &async_channel::Sender<FeedMsg>,
    stop: &AtomicBool,
    closer: &Mutex<Option<SubscriptionCloser>>,
) -> Ended {
    let Ok(target) = connections.resolve_follow() else {
        return Ended::Dropped { progress: false };
    };
    let Target { socket, chat_id } = target;
    let mut subscription = match Subscription::open(&socket, chat_id, *since) {
        Ok(subscription) => subscription,
        Err(_) => {
            // Refused: either the host is not itself, or the chat is gone. The host's list
            // says which; if it cannot be read either, try again later.
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
    // A stop that came between opening and storing the handle would have found no handle.
    if stop.load(Ordering::SeqCst) {
        return Ended::Stopped;
    }
    if messages.send_blocking(FeedMsg::Link(Link::Live)).is_err() {
        return Ended::Stopped;
    }
    let mut progress = false;
    while let Some(read) = subscription.next_envelope() {
        // An event this build cannot read (the host is newer) is the next one in the log, by
        // the gapless sequence; it is skipped and noted, not asked for again for ever.
        let envelope = read.unwrap_or_else(|_| unreadable(chat_id, *since + 1));
        match sequence(*since, envelope.seq) {
            Sequence::Repeat => continue,
            Sequence::Gap => break,
            Sequence::Next => {}
        }
        *since = envelope.seq;
        progress = true;
        if messages
            .send_blocking(FeedMsg::Events(vec![envelope]))
            .is_err()
        {
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
fn unreadable(chat_id: &str, seq: u64) -> Envelope {
    Envelope {
        chat_id: chat_id.to_owned(),
        seq,
        event: ChatEvent::ItemCompleted {
            item: Item {
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

/// Sleep for `delay`, or until `stop`.
fn pause(delay: Duration, stop: &AtomicBool) {
    let until = Instant::now() + delay;
    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10).min(delay));
    }
}

fn deliver(
    connections: &Connections,
    queue: mpsc::Receiver<Delivery>,
    messages: &async_channel::Sender<FeedMsg>,
) {
    let mut client: Option<Client> = None;
    // Ends when the queue is closed and empty: commands sent just before a tab closed are
    // still delivered.
    for delivery in queue {
        let command = match delivery {
            Delivery::Submission { id, command } => {
                let result = connections
                    .resolve_delivery()
                    .map_err(CallError::Broken)
                    .and_then(|target| {
                        Client::connect(&target.socket)
                            .map_err(CallError::Broken)
                            .and_then(|mut connection| {
                                connection.command_checked(target.chat_id, command.clone())
                            })
                    });
                client = None;
                if messages
                    .send_blocking(FeedMsg::Submission {
                        id,
                        command,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
                continue;
            }
            Delivery::Command(command) => command,
        };
        if matches!(command, ChatCommand::SendAttachments { .. }) {
            // Connect anew and exchange exactly once. A lost reply can follow a successful
            // submission, so neither Broken nor refusal permits an automatic resend.
            let result = connections
                .resolve_delivery()
                .map_err(CallError::Broken)
                .and_then(|target| {
                    Client::connect(&target.socket)
                        .map_err(CallError::Broken)
                        .and_then(|mut connection| {
                            connection.command_checked(target.chat_id, command.clone())
                        })
                });
            client = None;
            if messages
                .send_blocking(FeedMsg::AttachmentSubmission { command, result })
                .is_err()
            {
                return;
            }
            continue;
        }
        // A connection kept from an earlier command may have been the old host's: one
        // fresh attempt before giving up.
        let mut result = Err("no connection".to_owned());
        for _ in 0..2 {
            if client.is_none() {
                client = connections
                    .resolve_delivery()
                    .and_then(|target| Client::connect(&target.socket))
                    .ok();
            }
            let Some(connected) = client.as_mut() else {
                continue;
            };
            result = connected.command(&connections.chat_id, command.clone());
            match &result {
                Ok(()) => break,
                // Only a connection found broken is tried again. The host's own answer, or no
                // answer in time, may mean the command already went through.
                Err(error) if !connection_broke(error) => break,
                Err(_) => client = None,
            }
        }
        if let Err(error) = result
            && messages
                .send_blocking(FeedMsg::CommandFailed { command, error })
                .is_err()
        {
            return;
        }
    }
}

/// Whether an error says the connection was gone before the host could answer.
fn connection_broke(error: &str) -> bool {
    ["Broken pipe", "closed the connection", "Connection reset"]
        .iter()
        .any(|sign| error.contains(sign))
}

#[cfg(test)]
mod connection_policy_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn history_follow_and_explicit_delivery_use_separate_callbacks_with_the_same_uuid() {
        let ensured = Arc::new(AtomicUsize::new(0));
        let followed = Arc::new(AtomicUsize::new(0));
        let count = ensured.clone();
        let ensure: Ensure = Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Ok("/synthetic-only/recovered.sock".into())
        });
        let count = followed.clone();
        let follow: Ensure = Arc::new(move || {
            let attempt = count.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                Err("existing backend unavailable".into())
            } else {
                Ok("/synthetic-only/existing.sock".into())
            }
        });
        let id = "ed6d4e6e-8670-48c5-a1ea-9315379f8201";
        let policy = Connections::new(ensure, follow, id.into());
        // Constructing/browsing the policy itself does not resolve either callback.
        assert_eq!(ensured.load(Ordering::SeqCst), 0);
        assert_eq!(followed.load(Ordering::SeqCst), 0);
        assert!(policy.resolve_follow().is_err());
        let target = policy.resolve_follow().unwrap();
        assert_eq!(target.chat_id, id);
        assert_eq!(
            target.socket,
            PathBuf::from("/synthetic-only/existing.sock")
        );
        assert_eq!(
            ensured.load(Ordering::SeqCst),
            0,
            "even a failed follow never falls through to normal ensure"
        );
        assert_eq!(followed.load(Ordering::SeqCst), 2);
        // This is the same resolver the actual correlated/attachment delivery uses.
        let target = policy.resolve_delivery().unwrap();
        assert_eq!(target.chat_id, id);
        assert_eq!(
            target.socket,
            PathBuf::from("/synthetic-only/recovered.sock")
        );
        assert_eq!(ensured.load(Ordering::SeqCst), 1);
        assert_eq!(
            followed.load(Ordering::SeqCst),
            2,
            "delivery does not invoke the history resolver"
        );
        assert_eq!(policy.resolve_follow().unwrap().chat_id, id);
        assert_eq!(
            ensured.load(Ordering::SeqCst),
            1,
            "following remains read-only after explicit send recovery"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ChatEvent, ChatState, Decision};
    use crate::chat_view::testing::FakeHost;
    use std::sync::atomic::AtomicUsize;

    const WAIT: Duration = Duration::from_secs(5);

    fn fast() -> Backoff {
        Backoff {
            first: Duration::from_millis(10),
            longest: Duration::from_millis(40),
        }
    }

    fn feed_of(
        host: &FakeHost,
        chat: &str,
        since: u64,
    ) -> (Feed, async_channel::Receiver<FeedMsg>) {
        let socket = host.socket.clone();
        let ensure: Ensure = Arc::new(move || Ok(socket.clone()));
        let (sender, receiver) = async_channel::unbounded();
        (
            Feed::start(ensure, chat.into(), since, sender, fast()),
            receiver,
        )
    }

    fn state(state: ChatState) -> ChatEvent {
        ChatEvent::State { state }
    }

    /// Messages until `done` says enough, as (links, event seqs).
    fn collect(
        receiver: &async_channel::Receiver<FeedMsg>,
        mut done: impl FnMut(&[Link], &[u64]) -> bool,
    ) -> (Vec<Link>, Vec<u64>) {
        let (mut links, mut seqs) = (Vec::new(), Vec::new());
        let end = Instant::now() + WAIT;
        while !done(&links, &seqs) {
            assert!(Instant::now() < end, "timed out: {links:?} {seqs:?}");
            match receiver.try_recv() {
                Ok(FeedMsg::Link(link)) => links.push(link),
                Ok(FeedMsg::Events(events)) => seqs.extend(events.iter().map(|e| e.seq)),
                Ok(FeedMsg::CommandFailed { error, .. }) => panic!("{error}"),
                Ok(FeedMsg::AttachmentSubmission { .. } | FeedMsg::Submission { .. }) => {
                    panic!("unexpected submission receipt")
                }
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
        (links, seqs)
    }

    #[test]
    fn events_are_next_repeated_or_missing() {
        assert_eq!(sequence(0, 1), Sequence::Next);
        assert_eq!(sequence(4, 5), Sequence::Next);
        assert_eq!(sequence(4, 4), Sequence::Repeat);
        assert_eq!(sequence(4, 1), Sequence::Repeat);
        assert_eq!(sequence(4, 6), Sequence::Gap);
    }

    #[test]
    fn a_subscription_replays_what_happened_and_then_follows_the_chat() {
        let host = FakeHost::with_chats(&["chat"]);
        host.push("chat", state(ChatState::Starting));
        host.push("chat", state(ChatState::Idle));
        let (feed, receiver) = feed_of(&host, "chat", 0);
        let (links, seqs) = collect(&receiver, |_, seqs| seqs.len() == 2);
        assert_eq!(links, [Link::Live]);
        assert_eq!(seqs, [1, 2]);

        host.push("chat", state(ChatState::Running));
        let (_, seqs) = collect(&receiver, |_, seqs| seqs == [3]);
        assert_eq!(seqs, [3]);
        assert_eq!(host.subscribes(), [("chat".to_owned(), 0)]);
        feed.join();
    }

    #[test]
    fn after_the_host_drops_the_connection_it_subscribes_again_from_the_last_event() {
        let host = FakeHost::with_chats(&["chat"]);
        host.push("chat", state(ChatState::Starting));
        host.push("chat", state(ChatState::Idle));
        let (feed, receiver) = feed_of(&host, "chat", 0);
        collect(&receiver, |_, seqs| seqs.len() == 2);

        host.drop_connections();
        host.push("chat", state(ChatState::Running));
        let (links, seqs) = collect(&receiver, |links, seqs| {
            seqs == [3] && links.last() == Some(&Link::Live)
        });
        assert_eq!(links, [Link::Reconnecting, Link::Live]);
        assert_eq!(seqs, [3], "nothing replayed, nothing missed");
        assert_eq!(
            host.subscribes(),
            [("chat".to_owned(), 0), ("chat".to_owned(), 2)]
        );
        feed.join();
    }

    #[test]
    fn a_chat_the_host_does_not_list_any_more_ends_the_feed_as_deleted() {
        let host = FakeHost::with_chats(&["chat", "other"]);
        host.push("chat", state(ChatState::Idle));
        let (feed, receiver) = feed_of(&host, "chat", 0);
        collect(&receiver, |_, seqs| seqs.len() == 1);

        host.delete("chat");
        let (links, _) = collect(&receiver, |links, _| links.last() == Some(&Link::Deleted));
        // One miss is not yet a deleted chat: the host is asked again first.
        assert!(links.len() >= 3, "{links:?}");
        assert!(
            links[..links.len() - 1]
                .iter()
                .all(|link| *link == Link::Reconnecting)
        );
        // The other chat is untouched, and the feed has really ended.
        let (other, other_events) = feed_of(&host, "other", 0);
        collect(&other_events, |links, _| links.contains(&Link::Live));
        feed.join();
        other.join();
    }

    #[test]
    fn an_event_this_build_cannot_read_is_skipped_with_a_note_and_the_rest_follow() {
        let host = FakeHost::with_chats(&["chat"]);
        host.push("chat", state(ChatState::Starting));
        host.push_raw("chat", serde_json::json!({"event": "from_a_newer_host"}));
        host.push("chat", state(ChatState::Idle));
        let (feed, receiver) = feed_of(&host, "chat", 0);
        let mut events = Vec::new();
        let end = Instant::now() + WAIT;
        while events.len() < 3 {
            assert!(Instant::now() < end, "{events:?}");
            match receiver.try_recv() {
                Ok(FeedMsg::Events(more)) => events.extend(more),
                Ok(_) => {}
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(matches!(
            &events[1].event,
            ChatEvent::ItemCompleted { item } if item.id == "unreadable-2"
        ));
        assert_eq!(events[2].event, state(ChatState::Idle));
        // Nothing was asked for twice: one subscription served all of it.
        assert_eq!(host.subscribes().len(), 1);
        feed.join();
    }

    #[test]
    fn commands_sent_just_before_the_tab_closes_still_reach_the_host() {
        let host = FakeHost::with_chats(&["chat"]);
        let (feed, _receiver) = feed_of(&host, "chat", 0);
        feed.send(ChatCommand::Send { text: "one".into() });
        feed.send(ChatCommand::Send { text: "two".into() });
        feed.join();
        assert_eq!(
            host.commands()
                .into_iter()
                .map(|(_, c)| c)
                .collect::<Vec<_>>(),
            [
                ChatCommand::Send { text: "one".into() },
                ChatCommand::Send { text: "two".into() }
            ]
        );
    }

    #[test]
    fn only_a_connection_found_broken_is_worth_a_second_try_of_a_command() {
        assert!(connection_broke("chat host: Broken pipe (os error 32)"));
        assert!(connection_broke("chat host closed the connection"));
        assert!(connection_broke(
            "chat host: Connection reset by peer (os error 54)"
        ));
        // No answer in time, or the host's own refusal: the command may have gone through.
        assert!(!connection_broke(
            "chat host: Resource temporarily unavailable (os error 35)"
        ));
        assert!(!connection_broke("unknown chat"));
    }

    #[test]
    fn commands_reach_the_host_in_the_order_they_were_sent() {
        let host = FakeHost::with_chats(&["chat"]);
        let (feed, receiver) = feed_of(&host, "chat", 0);
        let sent = vec![
            ChatCommand::Send { text: "one".into() },
            ChatCommand::Approve {
                request_id: "r".into(),
                decision: Decision::Accept,
            },
            ChatCommand::Send { text: "two".into() },
            ChatCommand::Interrupt,
        ];
        for command in &sent {
            feed.send(command.clone());
        }
        let end = Instant::now() + WAIT;
        while host.commands().len() < sent.len() {
            assert!(Instant::now() < end, "{:?}", host.commands());
            thread::sleep(Duration::from_millis(5));
        }
        let got: Vec<_> = host
            .commands()
            .into_iter()
            .map(|(_, command)| command)
            .collect();
        assert_eq!(got, sent);
        assert!(host.commands().iter().all(|(chat, _)| chat == "chat"));
        drop(receiver);
        feed.join();
    }

    #[test]
    fn a_command_survives_the_host_having_restarted_since_the_last_one() {
        let host = FakeHost::with_chats(&["chat"]);
        let (feed, _receiver) = feed_of(&host, "chat", 0);
        feed.send(ChatCommand::Send {
            text: "before".into(),
        });
        let end = Instant::now() + WAIT;
        while host.commands().is_empty() {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(5));
        }
        host.drop_connections();
        feed.send(ChatCommand::Send {
            text: "after".into(),
        });
        while host.commands().len() < 2 {
            assert!(Instant::now() < end, "{:?}", host.commands());
            thread::sleep(Duration::from_millis(5));
        }
        feed.join();
    }

    #[test]
    fn a_command_the_host_cannot_deliver_is_reported() {
        let host = FakeHost::with_chats(&["chat"]);
        let (feed, receiver) = feed_of(&host, "chat", 0);
        host.delete("chat");
        feed.send(ChatCommand::Send {
            text: "to nobody".into(),
        });
        let end = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < end, "no report");
            match receiver.try_recv() {
                Ok(FeedMsg::CommandFailed { command, error }) => {
                    assert!(!error.is_empty());
                    assert_eq!(
                        command,
                        ChatCommand::Send {
                            text: "to nobody".into()
                        }
                    );
                    break;
                }
                Ok(_) => {}
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
        feed.join();
    }

    #[test]
    fn attachment_delivery_lost_reply_is_uncertain_and_attempted_once() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;
        let home = crate::chat::testing::short_home();
        let socket = home.join("lost.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            let request: crate::chat::wire::Request = serde_json::from_str(&line).unwrap();
            // The host acted, then its response was lost. A retry would duplicate the action.
            request
        });
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let ensure: Ensure = Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(socket.clone())
        });
        let (commands, queue) = mpsc::channel();
        let (messages, receiver) = async_channel::unbounded();
        let command = ChatCommand::SendAttachments {
            text: "exact draft".into(),
            attachments: Vec::new(),
        };
        commands.send(Delivery::Command(command.clone())).unwrap();
        drop(commands);
        let connections = Connections::new(ensure.clone(), ensure, "chat".into());
        deliver(&connections, queue, &messages);
        let FeedMsg::AttachmentSubmission {
            command: returned,
            result,
        } = receiver.recv_blocking().unwrap()
        else {
            panic!()
        };
        assert_eq!(returned, command);
        assert!(matches!(result, Err(CallError::Broken(_))));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(
            matches!(server.join().unwrap(),crate::chat::wire::Request::Command {command:c,..} if c==command)
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn stopping_wakes_a_subscription_waiting_on_a_quiet_chat() {
        let host = FakeHost::with_chats(&["chat"]);
        let (feed, receiver) = feed_of(&host, "chat", 0);
        collect(&receiver, |links, _| links.contains(&Link::Live));
        let started = Instant::now();
        feed.join();
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
