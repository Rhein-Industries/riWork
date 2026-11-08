//! The existing chat view over a paired host. Reads resume by contiguous cursor;
//! every queued mutation gets one RPC exchange and an explicit receipt. While this
//! Mac's registry does not list the host nothing goes out, and the receipt says so.
//! The chats of one host take turns at what it lets them wait for; see `Feeds`.
use super::feed::{Backoff, Delivery, FeedMsg, Link, Sink};
use riwork_engine::{
    chat::{
        model::{ChatCommand, ChatEvent},
        wire::Envelope,
    },
    remote_hosts::RemoteError,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

pub type Rpc = Arc<dyn Fn(&str, &str, Value, Duration) -> Result<Value, RemoteError> + Send + Sync>;
/// Whether this Mac's registry no longer lists a host. May read the registry, which the service
/// limits for the whole process however many chats ask.
pub type RouteRemoved = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// What a chat tells the person when this Mac's registry no longer lists its host. A relay route
/// paired again keeps its id, and the chat comes back by itself. An SSH route added again is a new
/// route, which this chat never hears of.
const ROUTE_REMOVED: &str = "This host was removed from this Mac. Add it again, then open this chat from its folder if it does not come back.";
/// How often a chat on a removed host looks for it again.
const ROUTE_RECHECK: Duration = Duration::from_secs(3);

/// How long a host may keep a chat's request while it has no new events.
const WAIT_MS: u64 = 2000;
/// Chats of one host that may wait at a time. The host runs no more waiting calls per connection
/// (`LONG_POLL_SLOTS` in `remote/src/lanes.rs`), older hosts included, and all of this Mac's windows
/// share one connection. Any more would wait in the host's queue with their time running: a busy
/// chat's next page came after every idle chat's wait, and from about 16 chats a healthy host read
/// as reconnecting.
const WAITS_PER_HOST: usize = 2;

/// How a feed spaces its work. Tests shorten it.
#[derive(Clone, Copy)]
struct Pace {
    /// How often a chat on a removed host looks for it again.
    recheck: Duration,
    /// The least a quiet chat that may not wait rests between empty pages. Its reads take a slot
    /// of the host's too, and each is a process there.
    rest_floor: Duration,
    /// How long a rest grows with each chat of the host, so that together they read about twice a
    /// second at most, however many there are.
    rest_per_feed: Duration,
    /// How long after its last event, its history aside, a chat counts as busy rather than quiet.
    busy_for: Duration,
    /// What a busy chat rests instead. Its next event is likely soon, and it may well be the chat
    /// on screen: behind two others that stream, a quiet chat's rest would show its agent's steps
    /// seconds late.
    rest_busy: Duration,
}
const PACE: Pace = Pace {
    recheck: ROUTE_RECHECK,
    rest_floor: Duration::from_secs(5),
    rest_per_feed: Duration::from_millis(500),
    busy_for: Duration::from_secs(30),
    rest_busy: Duration::from_millis(500),
};
impl Pace {
    /// The rest of a chat on a host with `feeds` chats, whose last event after its history came
    /// `since_event` ago if one ever did. A chat with none yet is quiet: an open tab is no sign
    /// that its agent is about to say something, and nor is what it said before.
    fn rest(&self, feeds: usize, since_event: Option<Duration>) -> Duration {
        let feeds = u32::try_from(feeds).unwrap_or(u32::MAX);
        let quiet = self
            .rest_floor
            .max(self.rest_per_feed.saturating_mul(feeds));
        if since_event.is_some_and(|since| since < self.busy_for) {
            quiet.min(self.rest_busy)
        } else {
            quiet
        }
    }
}

/// Where one feed stands among those of its host.
struct Place {
    /// When it last had an event or sent a command, by `Feeds::clock`.
    active: u64,
    /// It has a request out that may wait.
    waiting: bool,
}
/// Every running feed by host, so that they can share out what the host allows.
#[derive(Default)]
struct Feeds {
    hosts: BTreeMap<String, BTreeMap<u64, Place>>,
    next_id: u64,
    clock: u64,
}
impl Feeds {
    /// A new feed counts as just active: someone has just opened it.
    fn join(&mut self, host: &str) -> u64 {
        self.next_id += 1;
        self.clock += 1;
        let place = Place {
            active: self.clock,
            waiting: false,
        };
        self.hosts
            .entry(host.to_owned())
            .or_default()
            .insert(self.next_id, place);
        self.next_id
    }
    fn leave(&mut self, host: &str, id: u64) {
        if let Some(feeds) = self.hosts.get_mut(host) {
            feeds.remove(&id);
            if feeds.is_empty() {
                self.hosts.remove(host);
            }
        }
    }
    fn place(&mut self, host: &str, id: u64) -> Option<&mut Place> {
        self.hosts.get_mut(host)?.get_mut(&id)
    }
    fn touch(&mut self, host: &str, id: u64) {
        self.clock += 1;
        let clock = self.clock;
        if let Some(place) = self.place(host, id) {
            place.active = clock;
        }
    }
    /// Whether a feed may ask the host to wait now: it is among those active last, and the ones
    /// out there leave room. A wait that was asked for before others passed it finishes first, so
    /// no more waits than the host's slots are ever asked for at once, even as the order changes.
    /// A wait that ran out of time here may still be held there, for up to `WAIT_MS` even after
    /// its feed has a page again. Its feed asks for no other until that page, though another chat
    /// may take its place meanwhile.
    fn may_wait(&self, host: &str, id: u64) -> bool {
        let Some(feeds) = self.hosts.get(host) else {
            return false;
        };
        let Some(mine) = feeds.get(&id) else {
            return false;
        };
        let later = feeds.values().filter(|other| other.active > mine.active);
        let out = feeds
            .iter()
            .filter(|(other, place)| **other != id && place.waiting);
        later.count() < WAITS_PER_HOST && out.count() < WAITS_PER_HOST
    }
    fn begin_wait(&mut self, host: &str, id: u64) -> bool {
        let may = self.may_wait(host, id);
        if let Some(place) = self.place(host, id) {
            place.waiting = may;
        }
        may
    }
    fn end_wait(&mut self, host: &str, id: u64) {
        if let Some(place) = self.place(host, id) {
            place.waiting = false;
        }
    }
    fn count(&self, host: &str) -> usize {
        self.hosts.get(host).map_or(0, BTreeMap::len)
    }
}
fn feeds() -> MutexGuard<'static, Feeds> {
    static FEEDS: OnceLock<Mutex<Feeds>> = OnceLock::new();
    FEEDS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}
/// A feed's place in `Feeds`, which it gives up when the thread holding this ends.
struct Seat {
    host: String,
    id: u64,
}
impl Seat {
    fn join(host: &str) -> Self {
        Self {
            host: host.to_owned(),
            id: feeds().join(host),
        }
    }
    fn touch(&self) {
        feeds().touch(&self.host, self.id);
    }
    fn may_wait(&self) -> bool {
        feeds().may_wait(&self.host, self.id)
    }
    fn begin_wait(&self) -> bool {
        feeds().begin_wait(&self.host, self.id)
    }
    fn end_wait(&self) {
        feeds().end_wait(&self.host, self.id);
    }
    fn rest(&self, pace: Pace, since_event: Option<Duration>) -> Duration {
        pace.rest(feeds().count(&self.host), since_event)
    }
}
impl Drop for Seat {
    fn drop(&mut self) {
        feeds().leave(&self.host, self.id);
    }
}

#[derive(Clone)]
pub struct Target {
    pub host: String,
    pub chat: String,
    pub rpc: Rpc,
    pub route_removed: RouteRemoved,
    /// Set while the feed knows the registry does not list the host, for every copy of this
    /// target. Nothing goes to the daemon then: each attempt would run a `client ensure` that can
    /// only fail.
    pub removed: Arc<AtomicBool>,
}
impl Target {
    pub fn new(host: String, chat: String) -> Result<Self, String> {
        if host.is_empty()
            || host.len() > 256
            || host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == ':')
        {
            return Err("invalid: remote host identity".into());
        }
        crate::service::validate_uuid(&chat)?;
        Ok(Self {
            host,
            chat,
            rpc: Arc::new(crate::remote::call_host),
            route_removed: Arc::new(crate::remote::route_removed),
            removed: Arc::default(),
        })
    }
    pub fn key(&self) -> String {
        format!("remote-chat:{}:{}", self.host, self.chat)
    }
    /// UUID-shaped storage identity, namespaced by the full host and chat pair.
    /// DraftStore deliberately permits only canonical UUID filenames.
    pub fn draft_key(&self) -> String {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(self.key().as_bytes());
        let mut bytes: [u8; 16] = hash[..16].try_into().expect("SHA-256 prefix");
        bytes[6] = (bytes[6] & 0x0f) | 0x80;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid::Uuid::from_bytes(bytes).to_string()
    }
    pub fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RemoteError> {
        if self.removed.load(Ordering::Acquire) {
            return Err(RemoteError::Unreachable(ROUTE_REMOVED.into()));
        }
        (self.rpc)(&self.host, method, params, timeout)
    }
    pub fn command(&self, command: ChatCommand) -> Result<(), String> {
        // Wire Option fields are omitted: older remote validators reject explicit nulls.
        let mut value = serde_json::to_value(command).map_err(|e| format!("refused: {e}"))?;
        if let Some(fields) = value.as_object_mut() {
            fields.retain(|_, value| !value.is_null());
        }
        let reply = self
            .call(
                "chat.command",
                json!({"chat_id":self.chat,"command":value}),
                Duration::from_secs(90),
            )
            .map_err(command_error)?;
        if reply["status"] == "ok" {
            Ok(())
        } else {
            Err("broken: The host's command receipt could not be read; check the chat before trying again".into())
        }
    }
}

pub fn command_error(error: RemoteError) -> String {
    // Nothing left this Mac, so sending it again cannot repeat anything. An older daemon's plain
    // `offline` never arrives as `offline`: it may have written the request, so it comes as `Lost`.
    let not_sent = match &error {
        RemoteError::Rpc { code, .. } => code == "offline",
        RemoteError::NeedsSetup { .. } | RemoteError::Unreachable(_) => true,
        _ => false,
    };
    if not_sent {
        return format!(
            "refused: {}. Nothing was sent",
            error.message().trim_end_matches('.')
        );
    }
    match error {
        RemoteError::Rpc { code, message }
            if matches!(
                code.as_str(),
                "invalid_params"
                    | "invalid_request"
                    | "not_found"
                    | "unsupported"
                    | "unsupported_method"
                    | "method_not_found"
                    | "refused"
                    | "permission_denied"
            ) =>
        {
            format!("refused: {message}")
        }
        other => format!("broken: {}. The command was not retried", other.message()),
    }
}

/// Validate the entire page before advancing; a corrupt suffix must not hide events.
pub fn page(value: Value, chat: &str, since: u64) -> Result<(Vec<Envelope>, u64, bool), String> {
    if value["chat_id"].as_str() != Some(chat) {
        return Err("The host answered for another chat".into());
    }
    let rows = value["events"]
        .as_array()
        .ok_or("The host's event page is missing")?;
    if rows.len() > 500 {
        return Err("The host's event page is too large".into());
    }
    let mut cursor = since;
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let seq = row["seq"].as_u64().ok_or("Invalid chat event sequence")?;
        if cursor.checked_add(1) != Some(seq) {
            return Err("Chat event sequence gap; reconnecting from the last applied event".into());
        }
        let event: ChatEvent = serde_json::from_value(row["event"].clone())
            .map_err(|e| format!("Unsupported chat event: {e}"))?;
        if matches!(&event, ChatEvent::Info { info } if info.id != chat) {
            return Err("The host returned metadata for another chat".into());
        }
        events.push(Envelope {
            chat_id: chat.into(),
            seq,
            event,
        });
        cursor = seq;
    }
    let next = value["next"].as_u64().ok_or("Missing chat cursor")?;
    let more = value["more"].as_bool().ok_or("Missing chat continuation")?;
    if next != cursor || (more && cursor == since) {
        return Err("Invalid chat continuation cursor".into());
    }
    Ok((events, cursor, more))
}

pub struct RemoteFeed {
    stop: Arc<AtomicBool>,
}
impl RemoteFeed {
    pub fn start(target: Target, sink: Sink) -> (Self, mpsc::Sender<Delivery>) {
        Self::start_paced(target, sink, PACE)
    }

    fn start_paced(target: Target, sink: Sink, pace: Pace) -> (Self, mpsc::Sender<Delivery>) {
        let stop = Arc::new(AtomicBool::new(false));
        let (commands, inbox) = mpsc::channel();
        let seat = Seat::join(&target.host);
        let sending = target.clone();
        let seat_id = seat.id;
        thread::spawn(move || {
            for delivery in inbox {
                // Unlike the local socket delivery path, NO mutation is automatically retried.
                let (command, receipt) = match delivery {
                    Delivery::Command { command, receipt } => (command, receipt),
                    Delivery::Submission { command, receipt } => (command, Some(receipt)),
                };
                // Someone is at this chat, so what comes of this should not wait for its next page.
                feeds().touch(&sending.host, seat_id);
                let result = sending.command(command);
                if let Some(receipt) = receipt {
                    let _ = receipt.send(result);
                }
            }
        });
        let stopped = stop.clone();
        thread::spawn(move || {
            let mut cursor = 0;
            let mut failures = 0;
            // Whether the person has been told of this run of failures.
            let mut told = false;
            let mut live = false;
            // When a page last brought events after the chat's history. A chat that has had none
            // is quiet.
            let mut last_event = None::<std::time::Instant>;
            // Whether a page has said there was no more. The events before that are the chat's
            // history, which says nothing of whether its agent is at work. A failure does not undo
            // it: what comes while the link is down is new.
            let mut caught_up = false;
            // A wait that ran out of time here was sent all the same, and the host may still be
            // holding it in one of its slots, or not have it yet. Until a page comes back this
            // chat asks without waiting, so that such waits do not pile up. They can still overlap:
            // that page may answer a later request the host answered at once, and the host may
            // then hold the old wait for up to `WAIT_MS` next to the new one this chat asks for.
            let mut unsettled = false;
            let pause = |delay: Duration| {
                let until = std::time::Instant::now() + delay;
                while !stopped.load(Ordering::Acquire) && std::time::Instant::now() < until {
                    thread::sleep(Duration::from_millis(50));
                }
            };
            while !stopped.load(Ordering::Acquire) {
                let wait_ms = if !unsettled && seat.begin_wait() {
                    WAIT_MS
                } else {
                    0
                };
                let result = target.call("chat.events", json!({"chat_id":target.chat,"since":cursor,"wait_ms":wait_ms,"max_events":500,"complete":true,"features":["rate_limits"]}), Duration::from_secs(15));
                seat.end_wait();
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                unsettled |= wait_ms > 0 && matches!(result, Err(RemoteError::Timeout));
                // The daemon's answer came apart, as when it closed the connection, rather than
                // the host's page being wrong.
                let closed = matches!(result, Err(RemoteError::Protocol(_)));
                match result
                    .and_then(|v| page(v, &target.chat, cursor).map_err(RemoteError::Protocol))
                {
                    Ok((events, next, more)) => {
                        let has_events = !events.is_empty();
                        if has_events {
                            if caught_up {
                                last_event = Some(std::time::Instant::now());
                            }
                            seat.touch();
                            if !sink(FeedMsg::Events(events)) {
                                break;
                            }
                        }
                        cursor = next;
                        failures = 0;
                        told = false;
                        unsettled = false;
                        if !live {
                            if !sink(FeedMsg::Link(Link::Live)) {
                                break;
                            }
                            live = true;
                        }
                        if !more {
                            caught_up = true;
                            if has_events || seat.may_wait() {
                                thread::sleep(Duration::from_millis(50));
                            } else {
                                // Until its time is up, or sooner when it may wait after all: a
                                // command was sent, or a chat that was ahead of it has closed.
                                // Only a chat that has been quiet for a while rests for long.
                                let since_event = last_event.map(|at| at.elapsed());
                                let until =
                                    std::time::Instant::now() + seat.rest(pace, since_event);
                                while !stopped.load(Ordering::Acquire)
                                    && std::time::Instant::now() < until
                                    && !seat.may_wait()
                                {
                                    thread::sleep(Duration::from_millis(50));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        live = false;
                        // A revoked/missing host is not proof that this chat was deleted.
                        if !sink(FeedMsg::Link(Link::Reconnecting)) {
                            break;
                        }
                        // A daemon that cannot be started is how a removed route looks, and so is
                        // much else. Only the registry can say, so its error text is not read.
                        if matches!(error, RemoteError::Unreachable(_))
                            && (target.route_removed)(&target.host)
                        {
                            target.removed.store(true, Ordering::Release);
                            if !sink(FeedMsg::Link(Link::Removed))
                                || !sink(FeedMsg::CommandFailed {
                                    command: ChatCommand::Interrupt,
                                    error: ROUTE_REMOVED.into(),
                                })
                            {
                                break;
                            }
                            // A relay route paired again keeps its id, and the chat then goes on
                            // from the same cursor, as after any other long disconnection. An SSH
                            // route added again has a new id, so for one this goes on until the
                            // tab is closed: the notice says to open the chat from the new folder.
                            while !stopped.load(Ordering::Acquire)
                                && (target.route_removed)(&target.host)
                            {
                                pause(pace.recheck);
                            }
                            target.removed.store(false, Ordering::Release);
                            failures = 0;
                            told = false;
                            if !sink(FeedMsg::Link(Link::Reconnecting)) {
                                break;
                            }
                            continue;
                        }
                        failures += 1;
                        // A daemon that closed the connection is how a removal in Settings
                        // begins, which the next attempt explains, and how a daemon restart
                        // looks, which the next attempt ends. Only a failure that goes on is
                        // reported, in the words of the latest attempt.
                        if !told && !(failures == 1 && closed) {
                            told = true;
                            let _ = sink(FeedMsg::CommandFailed {
                                command: ChatCommand::Interrupt,
                                error: format!("Remote chat: {}", error.message()),
                            });
                        }
                        pause(Backoff::DEFAULT.delay(failures));
                    }
                }
            }
        });
        (Self { stop }, commands)
    }
}
impl Drop for RemoteFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_identity_and_cursor_are_strict_and_never_skip_a_gap() {
        let event = json!({"seq":1,"event":{"event":"state","state":{"state":"stopped"}}});
        let valid = json!({"chat_id":"chat","events":[event.clone()],"next":1,"more":false});
        assert_eq!(page(valid.clone(), "chat", 0).unwrap().1, 1);
        assert!(page(valid.clone(), "another", 0).is_err());
        assert!(page(valid, "chat", 1).is_err());
        assert!(page(json!({"chat_id":"chat","events":[event,{"seq":3,"event":{"event":"state","state":{"state":"stopped"}}}],"next":3,"more":false}), "chat", 0).is_err());
        assert!(
            page(
                json!({"chat_id":"chat","events":[],"next":7,"more":false}),
                "chat",
                0
            )
            .is_err()
        );
    }
    #[test]
    fn ambiguous_remote_submission_is_sent_once_and_host_scoped() {
        use std::sync::Mutex;
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        let target = Target {
            host: "host-b".into(),
            chat: "chat".into(),
            rpc: Arc::new(move |host, method, params, _| {
                captured
                    .lock()
                    .unwrap()
                    .push((host.to_owned(), method.to_owned(), params));
                Err(RemoteError::Timeout)
            }),
            route_removed: Arc::new(|_| panic!("a command never asks about the route")),
            removed: Arc::default(),
        };
        assert!(
            target
                .command(ChatCommand::Send {
                    text: "hello".into()
                })
                .unwrap_err()
                .starts_with("broken:")
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "host-b");
        assert_eq!(calls[0].2["chat_id"], "chat");
        assert_ne!(
            target.key(),
            Target {
                host: "host-a".into(),
                ..target.clone()
            }
            .key()
        );
    }

    #[test]
    fn a_command_is_refused_only_when_nothing_can_have_been_sent() {
        for error in [
            RemoteError::Rpc {
                code: "offline".into(),
                message: "host is offline: relay closed".into(),
            },
            RemoteError::NeedsSetup {
                cause: "identity_changed".into(),
                message: "This host's identity changed. Remove and add it again.".into(),
            },
            RemoteError::Unreachable("Cannot start the connection to this Mac: no host".into()),
        ] {
            let said = error.message();
            let text = command_error(error);
            assert!(text.starts_with("refused: "), "{text}");
            assert!(text.contains(said.trim_end_matches('.')), "{text}");
            // The composer's notice goes on after this with a full stop of its own.
            assert!(text.ends_with(". Nothing was sent"), "{text}");
        }
        // Anything else may have reached the host, and the person has to look before repeating it.
        for error in [
            RemoteError::Lost("the session ended".into()),
            // What an older daemon's plain `offline` becomes, as it cannot tell the cases apart.
            RemoteError::Lost("host is offline: relay closed".into()),
            RemoteError::Timeout,
            RemoteError::Protocol("unreadable".into()),
            RemoteError::Rpc {
                code: "cli_error".into(),
                message: "the host's command failed".into(),
            },
        ] {
            let text = command_error(error);
            assert!(text.starts_with("broken: "), "{text}");
            assert!(text.ends_with(". The command was not retried"), "{text}");
        }
        // The host's own refusals read as they always did.
        assert_eq!(
            command_error(RemoteError::Rpc {
                code: "invalid_params".into(),
                message: "no such approval".into(),
            }),
            "refused: no such approval"
        );
    }

    /// A feed on a scripted host, telling the test everything it posts.
    fn watch(
        rpc: Rpc,
        route_removed: RouteRemoved,
    ) -> (
        Target,
        RemoteFeed,
        mpsc::Sender<Delivery>,
        mpsc::Receiver<FeedMsg>,
    ) {
        let (posted, messages) = mpsc::channel();
        let sink: Sink = Arc::new(move |message| posted.send(message).is_ok());
        let target = Target {
            host: "host-a".into(),
            chat: "chat".into(),
            rpc,
            route_removed,
            removed: Arc::default(),
        };
        let (feed, commands) = RemoteFeed::start_paced(target.clone(), sink, QUICK);
        (target, feed, commands, messages)
    }

    /// Short waits, for tests that are not about them. A chat stays busy for as long as it does in
    /// the app, so a test's quiet chats rest as long as it says.
    const QUICK: Pace = Pace {
        recheck: Duration::from_millis(20),
        rest_floor: Duration::from_millis(20),
        rest_per_feed: Duration::ZERO,
        busy_for: PACE.busy_for,
        rest_busy: Duration::from_millis(20),
    };
    /// A rest that would show as the test running out of time.
    const LONG_REST: Duration = Duration::from_secs(600);

    /// One request as a host saw it.
    #[derive(Clone, Debug)]
    struct Asked {
        chat: String,
        wait_ms: u64,
        at: std::time::Instant,
        params: Value,
    }
    /// The chats of a host, as far as their events go. A request with nothing new waits for up to
    /// `slot`, if it says it may, and each page holds one event.
    struct ChatHost {
        slot: Duration,
        latest: std::sync::Mutex<std::collections::BTreeMap<String, u64>>,
        asked: std::sync::Mutex<Vec<Asked>>,
        /// Requests waiting now, and the most there ever were.
        waits: std::sync::Mutex<(usize, usize)>,
        commands: std::sync::atomic::AtomicUsize,
    }
    impl ChatHost {
        fn new(slot: Duration) -> Arc<Self> {
            Arc::new(Self {
                slot,
                latest: Default::default(),
                asked: Default::default(),
                waits: Default::default(),
                commands: Default::default(),
            })
        }
        fn rpc(self: &Arc<Self>) -> Rpc {
            let host = self.clone();
            Arc::new(move |_, method, params, _| Ok(host.answer(method, params)))
        }
        fn answer(&self, method: &str, params: Value) -> Value {
            if method == "chat.command" {
                self.commands.fetch_add(1, Ordering::SeqCst);
                return json!({"status": "ok"});
            }
            let chat = params["chat_id"].as_str().unwrap().to_owned();
            let since = params["since"].as_u64().unwrap();
            let wait_ms = params["wait_ms"].as_u64().unwrap();
            self.asked.lock().unwrap().push(Asked {
                chat: chat.clone(),
                wait_ms,
                at: std::time::Instant::now(),
                params,
            });
            if wait_ms > 0 {
                {
                    let mut waits = self.waits.lock().unwrap();
                    waits.0 += 1;
                    waits.1 = waits.1.max(waits.0);
                }
                let until = std::time::Instant::now() + self.slot;
                while std::time::Instant::now() < until && self.latest(&chat) <= since {
                    thread::sleep(Duration::from_millis(1));
                }
                self.waits.lock().unwrap().0 -= 1;
            }
            if self.latest(&chat) > since {
                let seq = since + 1;
                json!({"chat_id": chat, "next": seq, "more": false, "events": [
                    {"seq": seq, "event": {"event": "state", "state": {"state": "stopped"}}},
                ]})
            } else {
                json!({"chat_id": chat, "next": since, "more": false, "events": []})
            }
        }
        fn latest(&self, chat: &str) -> u64 {
            self.latest.lock().unwrap().get(chat).copied().unwrap_or(0)
        }
        fn post(&self, chat: &str) {
            *self
                .latest
                .lock()
                .unwrap()
                .entry(chat.to_owned())
                .or_default() += 1;
        }
        fn peak(&self) -> usize {
            self.waits.lock().unwrap().1
        }
        fn asked(&self) -> Vec<Asked> {
            self.asked.lock().unwrap().clone()
        }
        fn wait_until(&self, what: &str, done: impl Fn(&[Asked]) -> bool) {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while !done(&self.asked()) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "still waiting for {what}"
                );
                thread::sleep(Duration::from_millis(1));
            }
        }
        /// Until `chat` has asked nothing for half a second, which is many of its pages, and
        /// how many it had asked by then. A feed thread a busy machine wakes late can still
        /// have one more to ask after the others have passed it.
        fn until_quiet(&self, chat: &str) -> usize {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            let (mut seen, mut since) =
                (of(&self.asked(), chat).count(), std::time::Instant::now());
            while since.elapsed() < Duration::from_millis(500) {
                assert!(std::time::Instant::now() < deadline, "{chat} never rested");
                thread::sleep(Duration::from_millis(5));
                let now = of(&self.asked(), chat).count();
                if now != seen {
                    (seen, since) = (now, std::time::Instant::now());
                }
            }
            seen
        }
    }
    /// What the host saw of one chat.
    fn of<'a>(asked: &'a [Asked], chat: &'a str) -> impl Iterator<Item = &'a Asked> {
        asked.iter().filter(move |seen| seen.chat == chat)
    }
    fn waited<'a>(asked: &'a [Asked], chat: &'a str) -> impl Iterator<Item = &'a Asked> {
        of(asked, chat).filter(|seen| seen.wait_ms > 0)
    }
    fn did_not_wait<'a>(asked: &'a [Asked], chat: &'a str) -> impl Iterator<Item = &'a Asked> {
        of(asked, chat).filter(|seen| seen.wait_ms == 0)
    }
    /// A feed of one chat on the named host.
    fn follow(
        host: &str,
        chat: &str,
        rpc: Rpc,
        pace: Pace,
    ) -> (RemoteFeed, mpsc::Sender<Delivery>, mpsc::Receiver<FeedMsg>) {
        let (posted, messages) = mpsc::channel();
        let sink: Sink = Arc::new(move |message| posted.send(message).is_ok());
        let (feed, commands) = RemoteFeed::start_paced(
            Target {
                host: host.into(),
                chat: chat.into(),
                rpc,
                route_removed: Arc::new(|_| false),
                removed: Arc::default(),
            },
            sink,
            pace,
        );
        (feed, commands, messages)
    }

    #[test]
    fn two_chats_on_a_host_ask_exactly_as_before() {
        let host = ChatHost::new(Duration::from_millis(10));
        let pace = Pace {
            rest_floor: LONG_REST,
            ..QUICK
        };
        let _one = follow("two-chats", "one", host.rpc(), pace);
        let _two = follow("two-chats", "two", host.rpc(), pace);
        host.wait_until("some pages of each", |asked| {
            ["one", "two"]
                .iter()
                .all(|chat| of(asked, chat).count() >= 4)
        });
        for seen in host.asked() {
            assert_eq!(
                seen.params,
                json!({
                    "chat_id": seen.chat,
                    "since": 0,
                    "wait_ms": 2000,
                    "max_events": 500,
                    "complete": true,
                    "features": ["rate_limits"],
                })
            );
        }
    }

    #[test]
    fn only_two_of_a_hosts_chats_wait_and_the_others_ask_now_and_then() {
        let host = ChatHost::new(Duration::from_millis(20));
        let rest = Duration::from_millis(150);
        let pace = Pace {
            rest_floor: rest,
            ..QUICK
        };
        let chats = ["c1", "c2", "c3", "c4", "c5"];
        let _feeds: Vec<_> = chats
            .iter()
            .map(|chat| follow("five-chats", chat, host.rpc(), pace))
            .collect();
        let joined = std::time::Instant::now();
        host.wait_until("every chat to ask", |asked| {
            chats.iter().all(|chat| of(asked, chat).count() >= 2)
        });
        assert!(host.peak() <= 2, "{} waiting at once", host.peak());
        // The two opened last wait, and the others ask from time to time.
        host.wait_until("the newest two to wait", |asked| {
            ["c4", "c5"]
                .iter()
                .all(|chat| waited(asked, chat).count() >= 3)
        });
        host.wait_until("the others to ask without waiting", |asked| {
            ["c1", "c2", "c3"]
                .iter()
                .all(|chat| did_not_wait(asked, chat).count() >= 3)
        });
        assert!(host.peak() <= 2, "{} waiting at once", host.peak());
        let asked = host.asked();
        for chat in ["c1", "c2", "c3"] {
            let times: Vec<_> = did_not_wait(&asked, chat)
                .map(|seen| seen.at)
                .filter(|at| *at >= joined)
                .collect();
            for pair in times.windows(2) {
                assert!(pair[1] - pair[0] >= rest, "{:?} apart", pair[1] - pair[0]);
            }
        }
    }

    #[test]
    fn however_many_chats_a_host_has_two_wait_at_a_time_and_nothing_is_lost() {
        let host = ChatHost::new(Duration::from_millis(10));
        let pace = Pace {
            rest_floor: Duration::from_millis(50),
            rest_per_feed: Duration::from_millis(1),
            ..QUICK
        };
        let chats: Vec<String> = (0..16).map(|n| format!("chat-{n}")).collect();
        let feeds: Vec<_> = chats
            .iter()
            .map(|chat| follow("sixteen-chats", chat, host.rpc(), pace))
            .collect();
        // Events and commands keep the order of the chats moving.
        let mut commands = 0;
        for round in 0..48 {
            let at = round * 7 % chats.len();
            host.post(&chats[at]);
            if round % 5 == 0 {
                send(&feeds[at].1, "hello").unwrap();
                commands += 1;
            }
            thread::sleep(Duration::from_millis(5));
        }
        for (chat, (_, _, messages)) in chats.iter().zip(&feeds) {
            let want = host.latest(chat);
            let mut got = Vec::new();
            while (got.len() as u64) < want {
                let seen = read_until(messages, |message| matches!(message, FeedMsg::Events(_)));
                for message in seen {
                    match message {
                        FeedMsg::Events(events) => {
                            got.extend(events.into_iter().map(|event| event.seq))
                        }
                        FeedMsg::Link(link) => assert_eq!(link, Link::Live),
                        other => panic!("{other:?}"),
                    }
                }
            }
            assert_eq!(got, (1..=want).collect::<Vec<_>>());
        }
        assert_eq!(host.commands.load(Ordering::SeqCst), commands);
        assert!(host.peak() <= 2, "{} waiting at once", host.peak());
    }

    #[test]
    fn a_chat_that_gets_events_takes_the_place_of_the_quietest() {
        let host = ChatHost::new(Duration::from_millis(20));
        let pace = Pace {
            rest_floor: Duration::from_millis(100),
            ..QUICK
        };
        let (_old, _old_commands, old_messages) = follow("moves-up", "old", host.rpc(), pace);
        let _mid = follow("moves-up", "mid", host.rpc(), pace);
        let _new = follow("moves-up", "new", host.rpc(), pace);
        host.wait_until("the two newer chats to wait", |asked| {
            ["mid", "new"]
                .iter()
                .all(|chat| waited(asked, chat).count() >= 2)
        });
        let before = host.asked().len();
        host.post("old");
        read_until(&old_messages, |message| {
            matches!(message, FeedMsg::Events(_))
        });
        host.wait_until("it to wait and the quietest to rest", |asked| {
            let after = &asked[before..];
            waited(after, "old").count() >= 2 && did_not_wait(after, "mid").count() >= 2
        });
        assert!(host.peak() <= 2, "{} waiting at once", host.peak());
    }

    #[test]
    fn a_chat_with_events_keeps_going_without_the_rest() {
        let host = ChatHost::new(Duration::from_secs(5));
        let pace = Pace {
            rest_floor: LONG_REST,
            ..QUICK
        };
        let _one = follow("keeps-going", "one", host.rpc(), pace);
        let _two = follow("keeps-going", "two", host.rpc(), pace);
        host.wait_until("both places to be taken", |_| {
            host.waits.lock().unwrap().0 == 2
        });
        // One event a page, for a chat that cannot wait because the two others are.
        for _ in 0..5 {
            host.post("three");
        }
        let (_three, _commands, messages) = follow("keeps-going", "three", host.rpc(), pace);
        let mut got = 0;
        while got < 5 {
            got += read_until(&messages, |message| matches!(message, FeedMsg::Events(_)))
                .iter()
                .filter(|message| matches!(message, FeedMsg::Events(_)))
                .count();
        }
        let asked = host.asked();
        assert_eq!(of(&asked, "three").next().map(|seen| seen.wait_ms), Some(0));
    }

    /// Events for the named chats of a host, one each every 10 ms, until this is dropped.
    struct Streams {
        done: Arc<AtomicBool>,
        poster: Option<thread::JoinHandle<()>>,
    }
    impl Streams {
        fn start(host: &Arc<ChatHost>, chats: &[&str]) -> Self {
            let done = Arc::new(AtomicBool::new(false));
            let poster = {
                let (host, done) = (host.clone(), done.clone());
                let chats: Vec<String> = chats.iter().map(|chat| (*chat).to_owned()).collect();
                thread::spawn(move || {
                    while !done.load(Ordering::Acquire) {
                        for chat in &chats {
                            host.post(chat);
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                })
            };
            Self {
                done,
                poster: Some(poster),
            }
        }
    }
    impl Drop for Streams {
        fn drop(&mut self) {
            self.done.store(true, Ordering::Release);
            if let Some(poster) = self.poster.take() {
                let _ = poster.join();
            }
        }
    }

    #[test]
    fn a_chat_with_events_now_and_then_rests_only_a_moment_behind_two_busy_ones() {
        // Two hidden chats whose agents stream, and the one on screen, whose agent says something
        // every second or so. The busy two keep passing it, so it may not wait, and most of its
        // pages are empty.
        let host = ChatHost::new(Duration::from_millis(20));
        let pace = Pace {
            rest_floor: LONG_REST,
            ..QUICK
        };
        fn events_of(messages: &mpsc::Receiver<FeedMsg>, got: &mut Vec<u64>) {
            for message in read_until(messages, |message| matches!(message, FeedMsg::Events(_))) {
                match message {
                    FeedMsg::Events(events) => {
                        got.extend(events.into_iter().map(|event| event.seq))
                    }
                    FeedMsg::Link(link) => assert_eq!(link, Link::Live),
                    other => panic!("{other:?}"),
                }
            }
        }
        // Its history, and then its agent's first step while it is the only chat open.
        let mut got = Vec::new();
        host.post("shown");
        let (_shown, _commands, shown) = follow("now-and-then", "shown", host.rpc(), pace);
        events_of(&shown, &mut got);
        host.post("shown");
        events_of(&shown, &mut got);
        assert_eq!(got, [1, 2]);
        let _streams = Streams::start(&host, &["busy-a", "busy-b"]);
        let _busy_a = follow("now-and-then", "busy-a", host.rpc(), pace);
        let _busy_b = follow("now-and-then", "busy-b", host.rpc(), pace);
        for seq in 3..=6u64 {
            // Each one after the two others have passed it again and it found nothing new.
            host.wait_until("a few empty pages", |asked| {
                did_not_wait(asked, "shown")
                    .filter(|seen| seen.params["since"] == seq - 1)
                    .count()
                    >= 3
            });
            host.post("shown");
            events_of(&shown, &mut got);
            assert_eq!(got, (1..=seq).collect::<Vec<_>>());
        }
        assert!(host.peak() <= 2, "{} waiting at once", host.peak());
    }

    #[test]
    fn a_chats_history_is_no_sign_that_its_agent_is_at_work() {
        // Two chats opened behind two whose agents stream. The first page of each is its history,
        // and their agents are idle.
        let host = ChatHost::new(Duration::from_millis(20));
        let pace = Pace {
            rest_floor: LONG_REST,
            ..QUICK
        };
        for chat in ["opened", "back"] {
            host.post(chat);
        }
        let _streams = Streams::start(&host, &["busy-a", "busy-b"]);
        let (_opened, _commands, opened) = follow("history", "opened", host.rpc(), pace);
        // The other's link goes down once it has caught up, and its agent says something meanwhile.
        let dropped = Arc::new(AtomicBool::new(false));
        let rpc: Rpc = {
            let (host, dropped) = (host.clone(), dropped.clone());
            Arc::new(move |_, method, params, _| {
                if params["since"] == 1 && !dropped.swap(true, Ordering::SeqCst) {
                    host.post("back");
                    return Err(RemoteError::Lost("the link went down".into()));
                }
                Ok(host.answer(method, params))
            })
        };
        let (_back, _back_commands, _back_messages) = follow("history", "back", rpc, pace);
        let _busy_a = follow("history", "busy-a", host.rpc(), pace);
        let _busy_b = follow("history", "busy-b", host.rpc(), pace);
        read_until(&opened, |message| matches!(message, FeedMsg::Events(_)));
        // What came while the link was down is new, so that chat goes on asking.
        let after_outage = |asked: &[Asked]| {
            did_not_wait(asked, "back")
                .filter(|seen| seen.params["since"] == 2)
                .count()
        };
        host.wait_until("pages after what came meanwhile", |asked| {
            after_outage(asked) >= 3
        });
        assert!(dropped.load(Ordering::SeqCst));
        // Passed by the two others, the one that has had only its history rests as long as a chat
        // that has had nothing. Counted against the other, a machine that stalls the test stalls
        // both alike.
        let asked = host.asked();
        let (opened_before, back_before) = (of(&asked, "opened").count(), after_outage(&asked));
        host.wait_until("more pages after what came meanwhile", |asked| {
            after_outage(asked) >= back_before + 10
        });
        let more = of(&host.asked(), "opened").count() - opened_before;
        // A feed thread a busy machine wakes late can still have one more to ask.
        assert!(more <= 1, "{more} pages while the other had 10");
        assert!(host.peak() <= 2, "{} waiting at once", host.peak());
    }

    #[test]
    fn after_a_wait_that_ran_out_of_time_a_chat_asks_without_waiting_until_a_page_comes() {
        // The link stalled with the wait written: the host may still be holding it, in one of its
        // slots, when the link comes back. Asking it to wait again would put another behind it.
        let asked = Arc::new(Mutex::new(Vec::new()));
        let rpc: Rpc = {
            let asked = asked.clone();
            Arc::new(move |_, _, params, _| {
                let mut asked = asked.lock().unwrap();
                asked.push(params["wait_ms"].as_u64().unwrap());
                if asked.len() <= 2 {
                    Err(RemoteError::Timeout)
                } else {
                    Ok(json!({"chat_id": "chat", "next": 0, "more": false, "events": []}))
                }
            })
        };
        let (_feed, _commands, messages) = follow("stalled", "chat", rpc, QUICK);
        read_until(&messages, is_link(Link::Live));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while asked.lock().unwrap().len() < 4 {
            assert!(
                std::time::Instant::now() < deadline,
                "the feed stopped asking"
            );
            thread::sleep(Duration::from_millis(1));
        }
        // No wait while one may still be out there, and waits again once a page has come.
        assert_eq!(asked.lock().unwrap()[..4], [2000, 0, 0, 2000]);
    }

    #[test]
    fn a_command_brings_a_resting_chat_back_at_once() {
        let host = ChatHost::new(Duration::from_millis(20));
        let pace = Pace {
            rest_floor: LONG_REST,
            ..QUICK
        };
        let (_old, commands, _messages) = follow("command", "old", host.rpc(), pace);
        let _mid = follow("command", "mid", host.rpc(), pace);
        let _new = follow("command", "new", host.rpc(), pace);
        host.wait_until("the two newer chats to wait", |asked| {
            ["mid", "new"]
                .iter()
                .all(|chat| waited(asked, chat).count() >= 2)
        });
        // Passed by the two others, it has stopped asking.
        let before = host.until_quiet("old");

        send(&commands, "hello").unwrap();
        assert_eq!(host.commands.load(Ordering::SeqCst), 1);
        host.wait_until("it to wait again", |asked| {
            of(asked, "old").skip(before).any(|seen| seen.wait_ms > 0)
        });
        assert!(host.peak() <= 2, "{} waiting at once", host.peak());
    }

    #[test]
    fn a_closed_chat_gives_up_its_place_and_leaves_the_registry() {
        let host = ChatHost::new(Duration::from_millis(20));
        let pace = Pace {
            rest_floor: LONG_REST,
            ..QUICK
        };
        let old = follow("closing", "old", host.rpc(), pace);
        let mid = follow("closing", "mid", host.rpc(), pace);
        let new = follow("closing", "new", host.rpc(), pace);
        host.wait_until("the two newer chats to wait", |asked| {
            ["mid", "new"]
                .iter()
                .all(|chat| waited(asked, chat).count() >= 2)
        });
        let before = host.until_quiet("old");
        drop(new);
        // Its rest does not have to run out for it to move up.
        host.wait_until("the rested chat to wait", |asked| {
            of(asked, "old").skip(before).any(|seen| seen.wait_ms > 0)
        });
        drop(old);
        drop(mid);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while feeds().hosts.contains_key("closing") {
            assert!(std::time::Instant::now() < deadline, "a feed stayed behind");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn the_chats_active_last_may_wait_while_two_others_are_not_waiting() {
        let mut feeds = Feeds::default();
        let (a, b, c) = (feeds.join("h"), feeds.join("h"), feeds.join("h"));
        assert!(!feeds.may_wait("h", a), "two came after it");
        assert!(feeds.may_wait("h", b));
        assert!(feeds.may_wait("h", c));
        assert!(!feeds.begin_wait("h", a));
        assert!(feeds.begin_wait("h", b));
        assert!(feeds.begin_wait("h", c));

        // An event or a command moves a chat to the front.
        feeds.touch("h", a);
        assert!(!feeds.may_wait("h", b), "two were active after it");
        assert!(feeds.may_wait("h", c), "it is already waiting");
        // Yet the two out there are out there, so there is no room until one of them is back.
        assert!(!feeds.may_wait("h", a));
        assert!(!feeds.begin_wait("h", a));
        feeds.end_wait("h", b);
        assert!(feeds.may_wait("h", a));
        assert!(feeds.begin_wait("h", a));
        assert!(!feeds.begin_wait("h", b));

        // Hosts do not share anything.
        let other = feeds.join("other");
        assert!(feeds.begin_wait("other", other));
        // A place given up goes to the next in line, and an empty host leaves nothing behind.
        feeds.end_wait("h", a);
        feeds.end_wait("h", c);
        feeds.leave("h", a);
        assert!(feeds.may_wait("h", b));
        feeds.touch("h", a);
        feeds.leave("h", b);
        feeds.leave("h", c);
        assert!(!feeds.hosts.contains_key("h"));
        assert!(feeds.hosts.contains_key("other"));
    }

    #[test]
    fn a_rest_is_five_seconds_until_the_chats_of_a_host_ask_for_more() {
        assert_eq!(PACE.rest(1, None), Duration::from_secs(5));
        assert_eq!(PACE.rest(10, None), Duration::from_secs(5));
        assert_eq!(PACE.rest(11, None), Duration::from_millis(5500));
        assert_eq!(PACE.rest(30, None), Duration::from_secs(15));
    }

    #[test]
    fn a_chat_with_events_in_the_last_half_minute_rests_half_a_second() {
        let lately = Some(Duration::from_secs(29));
        assert_eq!(PACE.rest(1, lately), Duration::from_millis(500));
        assert_eq!(PACE.rest(30, lately), Duration::from_millis(500));
        // Quiet for half a minute, it rests as long as a chat that never had any.
        let quiet = Some(Duration::from_secs(30));
        assert_eq!(PACE.rest(1, quiet), Duration::from_secs(5));
        assert_eq!(PACE.rest(30, quiet), Duration::from_secs(15));
        // And never longer than a quiet chat.
        let short = Pace {
            rest_floor: Duration::from_millis(100),
            rest_per_feed: Duration::ZERO,
            ..PACE
        };
        assert_eq!(short.rest(1, lately), Duration::from_millis(100));
    }

    /// What was posted until `until` matched, which is included.
    fn read_until(
        messages: &mpsc::Receiver<FeedMsg>,
        until: impl Fn(&FeedMsg) -> bool,
    ) -> Vec<FeedMsg> {
        // One limit for the whole wait, as a feed that keeps retrying keeps posting.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        loop {
            let message = messages
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap_or_else(|_| panic!("still waiting after {seen:?}"));
            let done = until(&message);
            seen.push(message);
            if done {
                return seen;
            }
        }
    }

    fn notices(messages: &[FeedMsg]) -> Vec<&str> {
        messages
            .iter()
            .filter_map(|message| match message {
                FeedMsg::CommandFailed { error, .. } => Some(error.as_str()),
                _ => None,
            })
            .collect()
    }

    fn is_link(link: Link) -> impl Fn(&FeedMsg) -> bool {
        move |message| matches!(message, FeedMsg::Link(seen) if *seen == link)
    }

    fn send(commands: &mpsc::Sender<Delivery>, text: &str) -> Result<(), String> {
        let (receipt, answer) = tokio::sync::oneshot::channel();
        commands
            .send(Delivery::Submission {
                command: ChatCommand::Send { text: text.into() },
                receipt,
            })
            .unwrap();
        answer.blocking_recv().unwrap()
    }

    const CANNOT_START: &str = "Cannot start the connection to this Mac: no host";

    #[test]
    fn a_removed_route_is_explained_once_and_left_alone_until_it_is_back() {
        use std::sync::Mutex;
        let gone = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let asks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rpc: Rpc = {
            let (gone, calls) = (gone.clone(), calls.clone());
            Arc::new(move |_, method, params, _| {
                calls
                    .lock()
                    .unwrap()
                    .push((method.to_owned(), params.clone()));
                if method == "chat.command" {
                    return Ok(json!({"status": "ok"}));
                }
                if gone.load(Ordering::SeqCst) {
                    return Err(RemoteError::Unreachable(CANNOT_START.into()));
                }
                let since = params["since"].as_u64().unwrap();
                Ok(if since == 0 {
                    json!({"chat_id": "chat", "next": 1, "more": false, "events": [
                        {"seq": 1, "event": {"event": "state", "state": {"state": "stopped"}}},
                    ]})
                } else {
                    json!({"chat_id": "chat", "next": since, "more": false, "events": []})
                })
            })
        };
        let route: RouteRemoved = {
            let (gone, asks) = (gone.clone(), asks.clone());
            Arc::new(move |host| {
                assert_eq!(host, "host-a");
                asks.fetch_add(1, Ordering::SeqCst);
                gone.load(Ordering::SeqCst)
            })
        };
        let (target, feed, commands, messages) = watch(rpc, route);
        read_until(&messages, is_link(Link::Live));
        assert_eq!(asks.load(Ordering::SeqCst), 0, "a healthy chat never asks");

        // Removed: told once and in plain words, with no Deleted anywhere.
        gone.store(true, Ordering::SeqCst);
        let told = read_until(&messages, is_link(Link::Removed));
        assert!(!told.iter().any(is_link(Link::Deleted)), "{told:?}");
        let told = read_until(&messages, |message| {
            matches!(message, FeedMsg::CommandFailed { .. })
        });
        // True of both kinds of route: a relay route paired again keeps its id and this chat
        // comes back by itself, while an SSH route added again is a new route.
        assert_eq!(notices(&told), [REMOVED_TEXT]);

        // Nothing goes to the daemon now, which would run `client ensure` each time, and the
        // registry's answer is still looked at.
        let before = calls.lock().unwrap().len();
        let looked = asks.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(400));
        assert_eq!(calls.lock().unwrap().len(), before);
        assert!(
            asks.load(Ordering::SeqCst) > looked,
            "the feed stopped looking"
        );
        assert!(messages.try_recv().is_err(), "said only once");

        // A command in this state leaves nothing behind it and says so.
        let refused = send(&commands, "while it is gone").unwrap_err();
        assert!(
            refused.starts_with("refused: This host was removed"),
            "{refused}"
        );
        assert!(refused.ends_with("Nothing was sent"), "{refused}");
        assert_eq!(calls.lock().unwrap().len(), before);
        // And so does anything else a copy of the target is asked for, such as the composer's
        // suggestions as someone types.
        let suggestions = target
            .clone()
            .call("chat.suggestions", json!({}), Duration::from_secs(20))
            .unwrap_err();
        assert!(
            matches!(suggestions, RemoteError::Unreachable(_)),
            "{suggestions:?}"
        );
        assert_eq!(calls.lock().unwrap().len(), before);

        // Back: the same cursor, and commands go through again.
        gone.store(false, Ordering::SeqCst);
        let back = read_until(&messages, is_link(Link::Live));
        assert!(notices(&back).is_empty(), "{back:?}");
        let resumed = calls.lock().unwrap()[before].clone();
        assert_eq!(resumed.0, "chat.events");
        assert_eq!(resumed.1["since"], 1);
        send(&commands, "now it is back").unwrap();
        let sent = calls.lock().unwrap();
        assert_eq!(
            sent.iter()
                .filter(|(method, _)| method == "chat.command")
                .count(),
            1
        );
        drop(sent);

        // Stops like any other feed, also while it is waiting for the host to come back.
        gone.store(true, Ordering::SeqCst);
        read_until(&messages, is_link(Link::Removed));
        drop(feed);
        drop(commands);
        drop(target);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&asks) > 1 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            Arc::strong_count(&asks),
            1,
            "the feed's threads are still running"
        );
    }

    #[test]
    fn a_chat_opened_on_a_removed_route_says_only_that() {
        let rpc: Rpc = Arc::new(|_, _, _, _| Err(RemoteError::Unreachable(CANNOT_START.into())));
        let (_target, _feed, _commands, messages) = watch(rpc, Arc::new(|_| true));
        let told = read_until(&messages, |message| {
            matches!(message, FeedMsg::CommandFailed { .. })
        });
        assert_eq!(notices(&told), [REMOVED_TEXT]);
        assert!(told.iter().any(is_link(Link::Removed)), "{told:?}");
    }

    /// What a chat says when its host was removed, as the person reads it.
    const REMOVED_TEXT: &str = "This host was removed from this Mac. Add it again, then open this chat from its folder if it does not come back.";
    const CLOSED: &str = "the daemon closed the connection";

    /// What a host answers a chat that has had one event, from wherever it asks.
    fn one_event(params: &Value) -> Value {
        let since = params["since"].as_u64().unwrap();
        if since == 0 {
            json!({"chat_id": "chat", "next": 1, "more": false, "events": [
                {"seq": 1, "event": {"event": "state", "state": {"state": "stopped"}}},
            ]})
        } else {
            json!({"chat_id": "chat", "next": since, "more": false, "events": []})
        }
    }

    #[test]
    fn a_daemon_that_closes_for_a_removal_or_a_restart_says_nothing_of_its_own() {
        // Removed in Settings: the daemon is told to quit as the registry forgets the route, so
        // the chat first finds its connection closed. The retry finds the route gone, and that is
        // all the person is told.
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rpc: Rpc = {
            let calls = calls.clone();
            Arc::new(
                move |_, _, params, _| match calls.fetch_add(1, Ordering::SeqCst) {
                    0 => Ok(one_event(&params)),
                    1 => Err(RemoteError::Protocol(CLOSED.into())),
                    _ => Err(RemoteError::Unreachable(CANNOT_START.into())),
                },
            )
        };
        let (_target, _feed, _commands, messages) = watch(rpc, Arc::new(|_| true));
        read_until(&messages, is_link(Link::Live));
        let told = read_until(&messages, |message| {
            matches!(message, FeedMsg::CommandFailed { .. })
        });
        assert_eq!(notices(&told), [REMOVED_TEXT]);
        assert!(told.iter().any(is_link(Link::Removed)), "{told:?}");

        // Restarted, as after an update: back at the next attempt, with nothing to say.
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rpc: Rpc = {
            let calls = calls.clone();
            Arc::new(
                move |_, _, params, _| match calls.fetch_add(1, Ordering::SeqCst) {
                    1 => Err(RemoteError::Protocol(CLOSED.into())),
                    _ => Ok(one_event(&params)),
                },
            )
        };
        let (_feed, _commands, messages) = follow("restarted", "chat", rpc, QUICK);
        read_until(&messages, is_link(Link::Live));
        let back = read_until(&messages, is_link(Link::Live));
        assert!(back.iter().any(is_link(Link::Reconnecting)), "{back:?}");
        assert!(notices(&back).is_empty(), "{back:?}");

        // One that goes on closing it is reported, once, from the second attempt on.
        let rpc: Rpc = Arc::new(|_, _, _, _| Err(RemoteError::Protocol(CLOSED.into())));
        let (_feed, _commands, messages) = follow("keeps-closing", "chat", rpc, QUICK);
        let mut told = Vec::new();
        for _ in 0..3 {
            told.extend(read_until(&messages, is_link(Link::Reconnecting)));
        }
        let reconnecting = is_link(Link::Reconnecting);
        assert!(reconnecting(&told[0]) && reconnecting(&told[1]), "{told:?}");
        assert_eq!(
            notices(&told),
            [format!(
                "Remote chat: Unexpected answer from the client daemon: {CLOSED}"
            )]
        );
    }

    #[test]
    fn a_route_still_listed_keeps_retrying_with_the_daemons_own_words() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rpc: Rpc = {
            let attempts = attempts.clone();
            Arc::new(move |_, _, _, _| {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(RemoteError::Unreachable(CANNOT_START.into()))
            })
        };
        let asks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let route: RouteRemoved = {
            let asks = asks.clone();
            Arc::new(move |_| {
                asks.fetch_add(1, Ordering::SeqCst);
                false
            })
        };
        let (_target, _feed, _commands, messages) = watch(rpc, route);
        let told = read_until(&messages, |message| {
            matches!(message, FeedMsg::CommandFailed { .. })
        });
        assert_eq!(notices(&told), [format!("Remote chat: {CANNOT_START}")]);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while attempts.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(attempts.load(Ordering::SeqCst) >= 2, "the feed gave up");
        assert!(
            asks.load(Ordering::SeqCst) >= 1,
            "the registry was never asked"
        );
    }

    #[test]
    fn only_a_daemon_that_cannot_be_reached_makes_a_feed_ask_about_the_route() {
        for error in [
            RemoteError::Timeout,
            RemoteError::Lost("the session ended".into()),
            RemoteError::Protocol("the daemon closed the connection".into()),
            RemoteError::Rpc {
                code: "offline".into(),
                message: "host is offline: relay closed".into(),
            },
            RemoteError::NeedsSetup {
                cause: "identity_changed".into(),
                message: "Remove and add this host".into(),
            },
        ] {
            let said = format!("Remote chat: {}", error.message());
            let rpc: Rpc = Arc::new(move |_, _, _, _| Err(error.clone()));
            let (_target, _feed, _commands, messages) =
                watch(rpc, Arc::new(|_| panic!("asked the registry")));
            let told = read_until(&messages, |message| {
                matches!(message, FeedMsg::CommandFailed { .. })
            });
            assert_eq!(notices(&told), [said]);
        }
    }
}
