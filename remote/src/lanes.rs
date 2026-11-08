//! Which of one device's requests may run at the same time.
//!
//! The phone keeps a long `shell.output` poll open while it types, so requests
//! of one device cannot be answered strictly one after the other. But not all
//! of them can run side by side either: typing batches and resizes change the
//! shell and its geometry, and they must reach it in the order they were sent.
//!
//! - `Ordered`: `shell.keys`, `shell.input`, `shell.resize`,
//!   `shell.resize.clear`, `shell.create`, `shell.close`, `project.create`,
//!   `orchestrator.create`, `chat.create`, `chat.command`, `chat.stop` and `shell.paste`.
//!   One at a time, in arrival order. This is what keeps the batch ledger, the
//!   viewport and the order of typed text intact. Creating and closing a
//!   terminal, and creating a project or an orchestrator, change what exists, and a request that
//!   ends half way (a session started but not yet written down, one killed but
//!   still listed, or a project folder made but not registered) is worse than a
//!   slow one, so a session ending does not cut them short either; typing waits
//!   behind them. (A creation also keeps its CLI alive when the connection
//!   itself is torn down: see `Rpc::create`, `Rpc::create_project`,
//!   `Rpc::orchestrator_create` and `Rpc::chat_create`.) A message, an approval or a stop for a chat is typing
//!   of a kind: it is carried out in the order it was sent. A `native.invoke` of a
//!   method that writes (`rpc::native_is_write`) is ordered too, and its CLI also
//!   outlives a torn-down connection (`Rpc::native_invoke`): `Sessions.createSubshell`
//!   and `Sessions.openEditor` start terminals, as the creations above do.
//! - `LongPoll`: a `shell.output` that waits for a change, a `chat.events`
//!   that waits for an event and a `state.watch` that waits (any `wait_ms` above 0).
//!   At most two.
//! - `Read`: everything else, including a `shell.output` that does not wait,
//!   `shell.history`, a page of scrollback that never waits, `chats.list`, a
//!   `native.invoke` of a method that only reads, a `state.watch` with `wait_ms` 0,
//!   a `chat.events` with `wait_ms` 0 and the `upload.*` steps of a file from the
//!   phone (each a short write to disk; the paste that follows is `Ordered`). It changes nothing, so it needs no order,
//!   and it takes one of the three shared slots, never the `Ordered` one.
//!
//! At most four of those run at once: the one `Ordered` slot and three shared
//! by `LongPoll` and `Read`. So a wait, or three, can never keep a typing
//! batch or a resize from starting (it has a slot of its own), and two long
//! polls leave a slot for reads. Requests that find no slot wait in a queue in
//! arrival order; a request that has to wait does not hold up a later one it
//! does not compete with. One still waiting when the phone would give up is answered
//! as not sent (`gateway::queue_deadline`), so that its timer does not end the connection,
//! and so is what was ordered for its target after it (`Lanes::take_queued`).
//!
//! A desktop device's terminal streams (`pty.*`, see `pty`) have two lanes of
//! their own, outside those four, so a stream can never take a phone's slot nor
//! the other way round:
//!
//! - `Attach`: `pty.open`, which starts a process and answers on its first
//!   byte. One at a time.
//! - `Stream`: `pty.read`, parked until the stream has output. No process, so
//!   it is cheap to hold, but it is bounded: `STREAM_SLOTS` at once.
//!
//! `pty.write`, `pty.resize` and `pty.close` have no lane: the connection loop
//! answers them itself, in arrival order, because they never wait.
//!
//! All of that is per `Target`. A phone that reaches its SSH hosts through this
//! desktop (`gateway`) has one queue, in arrival order, but this desktop and each
//! host have slots of their own. What the slots protect is where the work is done:
//! the ledgers, the viewport, the order of typed text and the number of CLI
//! processes of that machine. A forwarded call runs no process here, and its host
//! has lanes of its own, so one host's slow command or dead link holds up neither
//! another host nor the desktop. `hosts.open` goes by route: opens of one route take
//! turns, so that the second finds the first's session, and different routes dial at
//! once (`gateway::MAX_SESSIONS` bounds them). The phone's own limit on outstanding
//! requests (`RequestBudget` on iOS) is what bounds the replies, as it always was.
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

/// Requests of the `Ordered` lane that run at once.
pub const ORDERED_SLOTS: usize = 1;
/// Requests of the `Read` and `LongPoll` lanes together.
pub const SHARED_SLOTS: usize = 3;
/// Of those, requests that wait for a change.
pub const LONG_POLL_SLOTS: usize = 2;
// Four in flight at most, and always a shared slot that no wait can take.
const _: () = assert!(ORDERED_SLOTS + SHARED_SLOTS == 4 && LONG_POLL_SLOTS < SHARED_SLOTS);

/// `pty.open` requests that run at once.
pub const ATTACH_SLOTS: usize = 1;
/// `pty.read` requests that are parked at once (`max_reads` in `features.pty`).
pub const STREAM_SLOTS: usize = 12;

/// Requests that are received but not started. A device that queues more is
/// not read from until some have started.
pub const MAX_QUEUED: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Ordered,
    LongPoll,
    Read,
    Attach,
    Stream,
}

impl Lane {
    /// Whether a request may be dropped half done when its session ends.
    /// Typing and resizing are not: they finish, and only the answer is lost.
    pub fn cancellable(self) -> bool {
        self != Self::Ordered
    }
}

/// Whose slots a request takes. The name is a session token or a route id as the
/// phone wrote it: `gateway` checks it, and until then it only counts slots.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// This machine.
    Local,
    /// Dialing one registered route.
    Route(String),
    /// One open routed session.
    Session(String),
}

/// The target of a decoded request. A request that cannot be forwarded as it stands
/// is `Local`: it is answered with an error, and it must not get slots of its own for
/// naming something else each time.
pub fn target_of(request: &Value) -> Target {
    let params = &request["params"];
    let name = |key: &str| {
        params[key]
            .as_str()
            .filter(|name| crate::crypto::uuid(name).is_ok())
            .map(str::to_owned)
    };
    match request["method"].as_str() {
        Some("hosts.open") => name("route_id").map_or(Target::Local, Target::Route),
        Some("hosts.call")
            if params["method"]
                .as_str()
                .is_some_and(crate::gateway::allowed) =>
        {
            name("session").map_or(Target::Local, Target::Session)
        }
        _ => Target::Local,
    }
}

/// The lane of a decoded request. Anything unrecognizable is a `Read`: it is
/// answered with an error at once.
pub fn classify(request: &Value) -> Lane {
    // Inspect one forwarding layer only; recursive host calls are invalid.
    if request["method"] == "hosts.call" {
        let inner = &request["params"];
        return if inner["method"]
            .as_str()
            .is_some_and(crate::gateway::allowed)
        {
            classify(inner)
        } else {
            Lane::Read
        };
    }
    let params = request.get("params");
    match request.get("method").and_then(Value::as_str) {
        Some(
            "hosts.open"
            | "shell.keys"
            | "shell.input"
            | "shell.resize"
            | "shell.resize.clear"
            | "shell.create"
            | "shell.close"
            | "project.create"
            | "project.memo.set"
            | "orchestrator.create"
            | "chat.create"
            | "chat.command"
            | "chat.stop"
            | "tabs.update"
            | "tabs.open"
            | "shell.paste",
        ) => Lane::Ordered,
        Some("pty.open") => Lane::Attach,
        Some("pty.read") => Lane::Stream,
        Some("shell.output")
            if params
                .and_then(|p| p.get("if_changed"))
                .is_some_and(Value::is_string)
                && params
                    .and_then(|p| p.get("wait_ms"))
                    .and_then(Value::as_i64)
                    .is_some_and(|ms| ms > 0) =>
        {
            Lane::LongPoll
        }
        // A chat's events wait for the first one after `since`: like a waiting
        // `shell.output`, but with no hash to compare, so `wait_ms` alone says it.
        // So does a wait for the desktop's data to change.
        Some("chat.events" | "state.watch")
            if params
                .and_then(|p| p.get("wait_ms"))
                .and_then(Value::as_i64)
                .is_some_and(|ms| ms > 0) =>
        {
            Lane::LongPoll
        }
        // One of the desktop's own methods: writes in order, reads shared. A method
        // that does not exist is answered with an error at once.
        Some("native.invoke") => {
            let name = |key: &str| {
                params
                    .and_then(|p| p.get(key))
                    .and_then(Value::as_str)
                    .unwrap_or("")
            };
            if crate::rpc::native_is_write(name("module"), name("method")) == Some(true) {
                Lane::Ordered
            } else {
                Lane::Read
            }
        }
        // `shell.history` and every other method: shared, never waiting.
        _ => Lane::Read,
    }
}

/// The slots one target has taken.
#[derive(Default)]
struct Slots {
    ordered: usize,
    shared: usize,
    long_polls: usize,
    attaching: usize,
    streaming: usize,
}
impl Slots {
    fn admits(&self, lane: Lane) -> bool {
        match lane {
            Lane::Ordered => self.ordered < ORDERED_SLOTS,
            Lane::Read => self.shared < SHARED_SLOTS,
            Lane::LongPoll => self.shared < SHARED_SLOTS && self.long_polls < LONG_POLL_SLOTS,
            Lane::Attach => self.attaching < ATTACH_SLOTS,
            Lane::Stream => self.streaming < STREAM_SLOTS,
        }
    }
    fn take(&mut self, lane: Lane) {
        match lane {
            Lane::Ordered => self.ordered += 1,
            Lane::Read => self.shared += 1,
            Lane::LongPoll => {
                self.shared += 1;
                self.long_polls += 1;
            }
            Lane::Attach => self.attaching += 1,
            Lane::Stream => self.streaming += 1,
        }
    }
    fn give_back(&mut self, lane: Lane) {
        match lane {
            Lane::Ordered => self.ordered = self.ordered.saturating_sub(1),
            Lane::Read => self.shared = self.shared.saturating_sub(1),
            Lane::LongPoll => {
                self.shared = self.shared.saturating_sub(1);
                self.long_polls = self.long_polls.saturating_sub(1);
            }
            Lane::Attach => self.attaching = self.attaching.saturating_sub(1),
            Lane::Stream => self.streaming = self.streaming.saturating_sub(1),
        }
    }
    fn running(&self) -> usize {
        self.ordered + self.shared + self.attaching + self.streaming
    }
}

/// Received requests and the slots they run in. `push`, `finished` and the rest of
/// what names no target are for a connection with only the one, `Local`.
pub struct Lanes<T> {
    queue: VecDeque<(Target, Lane, T)>,
    /// Only the targets that hold a slot, so that a phone naming a new session in every
    /// request, a real one or not, grows nothing.
    slots: HashMap<Target, Slots>,
}

impl<T> Default for Lanes<T> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            slots: HashMap::new(),
        }
    }
}

impl<T> Lanes<T> {
    pub fn push(&mut self, lane: Lane, item: T) {
        self.push_for(Target::Local, lane, item);
    }

    pub fn push_for(&mut self, target: Target, lane: Lane, item: T) {
        self.queue.push_back((target, lane, item));
    }

    /// Received but not started, whatever the target: this is what a connection stops
    /// reading for.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Started and not finished.
    pub fn running(&self) -> usize {
        self.slots.values().map(Slots::running).sum()
    }

    fn admits(&self, target: &Target, lane: Lane) -> bool {
        self.slots
            .get(target)
            .is_none_or(|slots| slots.admits(lane))
    }

    /// The oldest queued request that has a slot, which it now holds until
    /// `finished`. Because every `Ordered` request of a target needs the same single
    /// slot, the oldest of them that fits is also the oldest one there is.
    pub fn next_ready(&mut self) -> Option<(Lane, T)> {
        let index = self
            .queue
            .iter()
            .position(|(target, lane, _)| self.admits(target, *lane))?;
        let (target, lane, item) = self.queue.remove(index)?;
        self.slots.entry(target).or_default().take(lane);
        Some((lane, item))
    }

    /// The slot of a request that ended, however it ended.
    pub fn finished(&mut self, lane: Lane) {
        self.finished_for(&Target::Local, lane);
    }

    /// `finished`, for the target the request was pushed for.
    pub fn finished_for(&mut self, target: &Target, lane: Lane) {
        if let Some(slots) = self.slots.get_mut(target) {
            slots.give_back(lane);
            if slots.running() == 0 {
                self.slots.remove(target);
            }
        }
    }

    /// Forget the queued requests that may be dropped (see `Lane::cancellable`).
    pub fn drop_queued_cancellable(&mut self) {
        self.queue.retain(|(_, lane, _)| !lane.cancellable());
    }

    /// Takes the queued requests `late` picks out of the queue, oldest first, without
    /// their ever having held a slot. With an `Ordered` one go the `Ordered` ones of its
    /// target queued after it, late or not: they must not reach that target ahead of it,
    /// and its sender sends it again.
    pub fn take_queued(&mut self, mut late: impl FnMut(&T) -> bool) -> Vec<T> {
        let mut taken = Vec::new();
        let mut cut: Vec<Target> = Vec::new();
        for (target, lane, item) in std::mem::take(&mut self.queue) {
            let behind = lane == Lane::Ordered && cut.contains(&target);
            if behind || late(&item) {
                if lane == Lane::Ordered && !behind {
                    cut.push(target);
                }
                taken.push(item);
            } else {
                self.queue.push_back((target, lane, item));
            }
        }
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(method: &str, params: Value) -> Value {
        json!({"v":1,"type":"request","id":"x","method":method,"params":params})
    }

    #[test]
    fn gateway_uses_one_inner_lane_and_rejects_recursive_and_stream_methods() {
        for (method, params, expected) in [
            ("shell.input", json!({}), Lane::Ordered),
            (
                "native.invoke",
                json!({"module":"Store","method":"renameProject","args":{}}),
                Lane::Ordered,
            ),
            (
                "shell.output",
                json!({"if_changed":"hash","wait_ms":5000}),
                Lane::LongPoll,
            ),
            ("chat.events", json!({"wait_ms":25000}), Lane::LongPoll),
            ("state.watch", json!({"wait_ms":25000}), Lane::LongPoll),
            ("projects.list", json!({}), Lane::Read),
            ("upload.chunk", json!({}), Lane::Read),
            (
                "hosts.call",
                json!({"method":"shell.input","params":{}}),
                Lane::Read,
            ),
            ("pty.open", json!({}), Lane::Read),
            ("link.configure", json!({}), Lane::Read),
        ] {
            assert_eq!(
                classify(&request(
                    "hosts.call",
                    json!({"session":"token","method":method,"params":params})
                )),
                expected,
                "{method}"
            );
        }
        // Ordered, which says opens of one route take turns: `target_of` says who with.
        assert_eq!(classify(&request("hosts.open", json!({}))), Lane::Ordered);
        assert_eq!(classify(&request("hosts.close", json!({}))), Lane::Read);
    }

    #[test]
    fn a_forwarded_call_counts_against_its_session_and_a_dial_against_its_route() {
        let (session, route) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        let call = |session: Value, method: Value| {
            request(
                "hosts.call",
                json!({"session":session,"request_id":"r","method":method,"params":{}}),
            )
        };
        for method in [
            "shell.keys",
            "projects.list",
            "chat.events",
            "native.invoke",
        ] {
            assert_eq!(
                target_of(&call(json!(session), json!(method))),
                Target::Session(session.clone()),
                "{method}"
            );
        }
        assert_eq!(
            target_of(&request(
                "hosts.open",
                json!({"route_id":route,"host_id":"h"})
            )),
            Target::Route(route)
        );
        // Everything else is this machine's, and so is whatever will only be refused.
        for local in [
            request("shell.keys", json!({"session":session})),
            request("projects.list", json!({})),
            request("hosts.list", json!({})),
            request("hosts.close", json!({"session":session})),
            request("hosts.open", json!({})),
            request("hosts.open", json!({"route_id":7})),
            request("hosts.open", json!({"route_id":"not a uuid"})),
            request("hosts.open", json!(null)),
            request("hosts.call", json!(null)),
            request("hosts.call", json!({"method":"shell.keys"})),
            call(json!(7), json!("shell.keys")),
            call(json!("not a uuid"), json!("shell.keys")),
            call(json!(session.to_uppercase()), json!("shell.keys")),
            call(json!(session), json!(7)),
            call(json!(session), json!("hosts.call")),
            call(json!(session), json!("pty.open")),
            call(json!(session), json!("link.configure")),
            json!({"method":"hosts.call"}),
            json!({"method":7}),
            Value::Null,
        ] {
            assert_eq!(target_of(&local), Target::Local, "{local}");
        }
    }

    fn session(name: &str) -> Target {
        Target::Session(name.into())
    }

    #[test]
    fn each_target_has_its_own_slots_in_one_arrival_ordered_queue() {
        let mut lanes = Lanes::default();
        // Each of them typing: one at a time each, and side by side.
        for (n, target) in [
            session("a"),
            session("a"),
            session("b"),
            Target::Local,
            Target::Route("r".into()),
            Target::Route("s".into()),
            Target::Local,
            Target::Route("r".into()),
            session("b"),
        ]
        .into_iter()
        .enumerate()
        {
            lanes.push_for(target, Lane::Ordered, n);
        }
        assert_eq!(
            std::iter::from_fn(|| lanes.next_ready())
                .map(|(_, n)| n)
                .collect::<Vec<_>>(),
            vec![0, 2, 3, 4, 5]
        );
        assert_eq!((lanes.running(), lanes.queued()), (5, 4));
        // A slot that frees up lets in the same target's next one, and only that.
        lanes.finished_for(&session("b"), Lane::Ordered);
        assert_eq!(lanes.next_ready(), Some((Lane::Ordered, 8)));
        assert_eq!(lanes.next_ready(), None);
        lanes.finished_for(&session("a"), Lane::Ordered);
        assert_eq!(lanes.next_ready(), Some((Lane::Ordered, 1)));
        assert_eq!(lanes.next_ready(), None);
        lanes.finished_for(&Target::Route("r".into()), Lane::Ordered);
        assert_eq!(lanes.next_ready(), Some((Lane::Ordered, 7)));
        lanes.finished(Lane::Ordered);
        assert_eq!(lanes.next_ready(), Some((Lane::Ordered, 6)));
        assert_eq!((lanes.running(), lanes.queued()), (5, 0));
    }

    #[test]
    fn a_target_is_held_to_the_limits_that_the_connection_was_held_to() {
        let mut lanes = Lanes::default();
        for n in 0..4 {
            lanes.push_for(session("a"), Lane::LongPoll, n);
        }
        lanes.push_for(session("b"), Lane::LongPoll, 4);
        for n in 5..8 {
            lanes.push_for(session("a"), Lane::Read, n);
        }
        lanes.push_for(Target::Local, Lane::Read, 8);
        // Two polls and a read for `a`; everything `b` and this machine asked for.
        assert_eq!(
            std::iter::from_fn(|| lanes.next_ready())
                .map(|(_, n)| n)
                .collect::<Vec<_>>(),
            vec![0, 1, 4, 5, 8]
        );
        assert_eq!(lanes.queued(), 4);
    }

    #[test]
    fn a_target_takes_no_room_once_nothing_of_it_runs() {
        let mut lanes = Lanes::default();
        for n in 0..1000 {
            let target = session(&format!("nobody-{n}"));
            lanes.push_for(target.clone(), Lane::Read, n);
            assert!(lanes.next_ready().is_some());
            lanes.finished_for(&target, Lane::Read);
        }
        assert!(lanes.slots.is_empty());
        assert_eq!(lanes.running(), 0);
        // One that finishes more than it started, or never started, grows nothing either.
        lanes.finished_for(&session("nobody"), Lane::Ordered);
        lanes.finished(Lane::Read);
        assert!(lanes.slots.is_empty());
        // Two kinds of slot held, one given back: it stays until the other is too.
        for lane in [Lane::Read, Lane::Ordered] {
            lanes.push_for(session("a"), lane, 0);
            assert!(lanes.next_ready().is_some());
        }
        lanes.finished_for(&session("a"), Lane::Read);
        assert_eq!(lanes.slots.len(), 1);
        lanes.finished_for(&session("a"), Lane::Ordered);
        assert!(lanes.slots.is_empty());
    }

    #[test]
    fn a_late_queued_request_is_taken_out_and_the_rest_keep_their_order() {
        let mut lanes = Lanes::default();
        lanes.push_for(session("a"), Lane::Ordered, 0);
        assert_eq!(lanes.next_ready(), Some((Lane::Ordered, 0)));
        for n in 1..6 {
            lanes.push_for(session("a"), Lane::LongPoll, n);
        }
        assert_eq!(
            start_all(&mut lanes),
            vec![(Lane::LongPoll, 1), (Lane::LongPoll, 2)]
        );
        assert_eq!(lanes.take_queued(|n| *n % 2 == 0), vec![4]);
        // Having held no slot, it gives none back: the ones that run still have their own.
        assert_eq!((lanes.running(), lanes.queued()), (3, 2));
        assert_eq!(lanes.next_ready(), None);
        lanes.finished_for(&session("a"), Lane::LongPoll);
        assert_eq!(lanes.next_ready(), Some((Lane::LongPoll, 3)));
        assert!(lanes.take_queued(|_| false).is_empty());
        assert_eq!(lanes.queued(), 1);
    }

    #[test]
    fn a_late_ordered_request_takes_what_its_target_ordered_after_it_along() {
        let mut lanes = Lanes::default();
        lanes.push_for(session("a"), Lane::Ordered, 0);
        assert_eq!(lanes.next_ready(), Some((Lane::Ordered, 0)));
        // Typing that waits too long, then a paste, which is not late yet: run, the paste
        // would reach the shell ahead of what is typed when the phone sends it again.
        lanes.push_for(session("a"), Lane::Ordered, 1);
        lanes.push_for(session("b"), Lane::Ordered, 2);
        lanes.push_for(Target::Local, Lane::Ordered, 3);
        lanes.push_for(session("a"), Lane::Read, 4);
        lanes.push_for(session("a"), Lane::Ordered, 5);
        assert_eq!(lanes.take_queued(|n| *n == 1), vec![1, 5]);
        // Other targets and what needs no order are left as they were.
        assert_eq!(
            start_all(&mut lanes),
            vec![(Lane::Ordered, 2), (Lane::Ordered, 3), (Lane::Read, 4)]
        );
        assert_eq!(lanes.queued(), 0);
        // What came before a late one is not taken with it.
        for n in 6..9 {
            lanes.push_for(session("b"), Lane::Ordered, n);
        }
        assert_eq!(lanes.take_queued(|n| *n == 7), vec![7, 8]);
        assert_eq!(lanes.queued(), 1);
    }

    #[test]
    fn chat_changes_are_ordered_and_never_cut_short_and_chat_reads_are_shared() {
        // Whatever the params look like, even none: validation answers it.
        for method in ["chat.create", "chat.command", "chat.stop"] {
            for params in [
                json!({"chat_id":"c"}),
                json!({"chat_id":"c","wait_ms":5000}),
                json!(null),
            ] {
                let lane = classify(&request(method, params));
                assert_eq!(lane, Lane::Ordered, "{method}");
                assert!(!lane.cancellable(), "{method}");
            }
        }
        // The list is a plain read.
        for params in [json!({}), json!({"project_id":"p"}), json!(null)] {
            assert_eq!(classify(&request("chats.list", params)), Lane::Read);
        }
        // Events wait only when asked to: any `wait_ms` above 0, no hash needed.
        let events = |params| classify(&request("chat.events", params));
        assert_eq!(
            events(json!({"chat_id":"c","since":0,"wait_ms":1})),
            Lane::LongPoll
        );
        assert_eq!(
            events(json!({"chat_id":"c","since":9,"wait_ms":25000,"max_events":10})),
            Lane::LongPoll
        );
        assert_eq!(
            events(json!({"chat_id":"c","since":0,"wait_ms":0})),
            Lane::Read
        );
        // Nonsense is a read: it fails validation at once.
        for params in [
            json!({"chat_id":"c","since":0}),
            json!({"chat_id":"c","since":0,"wait_ms":-1}),
            json!({"chat_id":"c","since":0,"wait_ms":"5000"}),
            json!({"chat_id":"c","since":0,"wait_ms":5000.5}),
            json!({"chat_id":"c","since":0,"wait_ms":null}),
            json!(null),
        ] {
            assert_eq!(events(params.clone()), Lane::Read, "{params}");
        }
        assert!(Lane::LongPoll.cancellable());
        // Only the exact names: look-alikes are plain reads.
        for method in [
            "chat",
            "chats",
            "chat.creates",
            "Chat.stop",
            "chat.stop.",
            "chats.create",
        ] {
            assert_eq!(
                classify(&request(method, json!({}))),
                Lane::Read,
                "{method}"
            );
        }
        // A wait for events takes a poll slot, so two of them leave a slot for reads, and
        // typing and a message for a chat queue behind each other in arrival order.
        let mut lanes = Lanes::default();
        for n in 0..3 {
            lanes.push(Lane::LongPoll, n);
        }
        lanes.push(Lane::Ordered, 10);
        lanes.push(Lane::Ordered, 11);
        lanes.push(Lane::Read, 12);
        assert_eq!(
            start_all(&mut lanes),
            vec![
                (Lane::LongPoll, 0),
                (Lane::LongPoll, 1),
                (Lane::Ordered, 10),
                (Lane::Read, 12)
            ]
        );
        lanes.finished(Lane::Ordered);
        assert_eq!(start_all(&mut lanes), vec![(Lane::Ordered, 11)]);
    }

    #[test]
    fn requests_are_classified_by_what_they_change_and_how_long_they_last() {
        let ordered = [
            "shell.keys",
            "shell.input",
            "shell.resize",
            "shell.resize.clear",
            "shell.create",
            "shell.close",
            "project.create",
            "project.memo.set",
            "orchestrator.create",
            "chat.create",
            "chat.command",
            "chat.stop",
            "shell.paste",
        ];
        for method in ordered {
            assert_eq!(
                classify(&request(method, json!({}))),
                Lane::Ordered,
                "{method}"
            );
        }
        for method in [
            "projects.list",
            "shells.list",
            "appearance.get",
            "project.memo.get",
            "chats.list",
            "chat.models",
            "chat.suggestions",
            "upload.begin",
            "upload.chunk",
            "upload.finish",
            "upload.cancel",
            "nope",
            "execute",
        ] {
            assert_eq!(
                classify(&request(method, json!({}))),
                Lane::Read,
                "{method}"
            );
        }
        // A scrollback page never waits, whatever it is sent with: it is a
        // plain read, so it can never hold up typing or take a poll slot.
        for params in [
            json!({"shell_id":"s","end":0,"lines":100}),
            json!({"shell_id":"s","end":0,"lines":1000,"styled":true}),
            json!({"if_changed":"h","wait_ms":5000}),
            json!(null),
        ] {
            assert_eq!(
                classify(&request("shell.history", params)),
                Lane::Read,
                "shell.history"
            );
        }
        // A desktop's terminal streams: opening is the attach lane, reading is
        // parked; writing, resizing and closing are answered by the connection loop,
        // so to the lanes (which they never reach for a desktop) they are plain reads.
        for params in [
            json!({}),
            json!(null),
            json!({"stream":"s","wait_ms":25000}),
        ] {
            assert_eq!(classify(&request("pty.open", params.clone())), Lane::Attach);
            assert_eq!(classify(&request("pty.read", params.clone())), Lane::Stream);
            for method in [
                "pty.write",
                "pty.resize",
                "pty.close",
                "pty.reads",
                "Pty.open",
            ] {
                assert_eq!(
                    classify(&request(method, params.clone())),
                    Lane::Read,
                    "{method}"
                );
            }
        }
        let output = |params| classify(&request("shell.output", params));
        assert_eq!(output(json!({"shell_id":"s"})), Lane::Read);
        assert_eq!(output(json!({"if_changed":"h"})), Lane::Read);
        assert_eq!(output(json!({"if_changed":"h","wait_ms":0})), Lane::Read);
        assert_eq!(output(json!({"wait_ms":5000})), Lane::Read);
        assert_eq!(
            output(json!({"if_changed":"h","wait_ms":1})),
            Lane::LongPoll
        );
        assert_eq!(
            output(json!({"if_changed":"h","wait_ms":10000})),
            Lane::LongPoll
        );
        // Nonsense is a read: it fails validation at once.
        assert_eq!(output(json!({"if_changed":7,"wait_ms":5000})), Lane::Read);
        assert_eq!(
            output(json!({"if_changed":"h","wait_ms":"5000"})),
            Lane::Read
        );
        assert_eq!(output(json!({"if_changed":"h","wait_ms":-5})), Lane::Read);
        assert_eq!(output(json!(null)), Lane::Read);
        assert_eq!(classify(&Value::Null), Lane::Read);
        assert_eq!(classify(&json!({"method":7})), Lane::Read);
    }

    fn start_all(lanes: &mut Lanes<u32>) -> Vec<(Lane, u32)> {
        std::iter::from_fn(|| lanes.next_ready()).collect()
    }

    #[test]
    fn at_most_four_run_and_the_rest_wait_in_order() {
        let mut lanes = Lanes::default();
        for n in 0..6 {
            lanes.push(Lane::Read, n);
        }
        for n in 6..9 {
            lanes.push(Lane::Ordered, n);
        }
        // Three reads and the first ordered request.
        let started = start_all(&mut lanes);
        assert_eq!(
            started,
            vec![
                (Lane::Read, 0),
                (Lane::Read, 1),
                (Lane::Read, 2),
                (Lane::Ordered, 6)
            ]
        );
        assert_eq!((lanes.running(), lanes.queued()), (4, 5));
        assert_eq!(lanes.next_ready(), None);
        // A read that ends lets the next read in, not a later ordered one.
        lanes.finished(Lane::Read);
        assert_eq!(start_all(&mut lanes), vec![(Lane::Read, 3)]);
        // Ordered requests run one at a time, oldest first.
        lanes.finished(Lane::Ordered);
        assert_eq!(start_all(&mut lanes), vec![(Lane::Ordered, 7)]);
        lanes.finished(Lane::Ordered);
        assert_eq!(start_all(&mut lanes), vec![(Lane::Ordered, 8)]);
        assert_eq!(lanes.running(), 4);
        assert_eq!(lanes.queued(), 2);
    }

    #[test]
    fn waits_never_take_the_slot_of_typing_or_the_last_shared_slot() {
        let mut lanes = Lanes::default();
        for n in 0..4 {
            lanes.push(Lane::LongPoll, n);
        }
        // Two polls run; the others wait for a poll slot, not for a read one.
        assert_eq!(
            start_all(&mut lanes),
            vec![(Lane::LongPoll, 0), (Lane::LongPoll, 1)]
        );
        // Typing and a resize still get their own slot at once, and a plain
        // read gets the third shared slot, with polls queued ahead of them.
        lanes.push(Lane::Ordered, 10);
        lanes.push(Lane::Read, 11);
        lanes.push(Lane::Ordered, 12);
        assert_eq!(
            start_all(&mut lanes),
            vec![(Lane::Ordered, 10), (Lane::Read, 11)]
        );
        assert_eq!((lanes.running(), lanes.queued()), (4, 3));
        // A poll that ends hands its slot to the next poll.
        lanes.finished(Lane::LongPoll);
        assert_eq!(start_all(&mut lanes), vec![(Lane::LongPoll, 2)]);
    }

    #[test]
    fn a_request_that_must_wait_does_not_hold_up_one_it_does_not_compete_with() {
        let mut lanes = Lanes::default();
        lanes.push(Lane::Ordered, 0);
        lanes.push(Lane::Ordered, 1);
        lanes.push(Lane::Read, 2);
        assert_eq!(
            start_all(&mut lanes),
            vec![(Lane::Ordered, 0), (Lane::Read, 2)]
        );
        assert_eq!(lanes.queued(), 1);
    }

    #[test]
    fn queued_requests_that_may_be_dropped_are_dropped_and_typing_stays() {
        let mut lanes = Lanes::default();
        lanes.push(Lane::Ordered, 0);
        lanes.push(Lane::Ordered, 1);
        lanes.push(Lane::Read, 2);
        lanes.push(Lane::LongPoll, 3);
        assert_eq!(lanes.next_ready(), Some((Lane::Ordered, 0)));
        lanes.drop_queued_cancellable();
        assert_eq!(lanes.queued(), 1);
        lanes.finished(Lane::Ordered);
        assert_eq!(start_all(&mut lanes), vec![(Lane::Ordered, 1)]);
    }

    #[test]
    fn terminal_streams_have_slots_of_their_own_and_never_take_a_phones() {
        let mut lanes = Lanes::default();
        // The shared slots, the ordered slot and a poll are all taken.
        for n in 0..3 {
            lanes.push(Lane::Read, n);
        }
        lanes.push(Lane::Ordered, 3);
        assert_eq!(start_all(&mut lanes).len(), 4);
        // Opens run one at a time, whatever else is running.
        for n in 10..13 {
            lanes.push(Lane::Attach, n);
        }
        assert_eq!(start_all(&mut lanes), vec![(Lane::Attach, 10)]);
        assert_eq!(lanes.queued(), 2);
        // Reads park up to the announced number, then wait their turn in order.
        for n in 20..20 + STREAM_SLOTS as u32 + 3 {
            lanes.push(Lane::Stream, n);
        }
        let parked = start_all(&mut lanes);
        assert_eq!(parked.len(), STREAM_SLOTS);
        assert!(parked.iter().all(|(lane, _)| *lane == Lane::Stream));
        assert_eq!(parked[0].1, 20);
        assert_eq!(lanes.queued(), 2 + 3);
        assert_eq!(lanes.running(), 4 + 1 + STREAM_SLOTS);
        // A parked read that ends hands its slot to the oldest waiting one, and an
        // open that ends to the next open: each lane is its own queue.
        lanes.finished(Lane::Stream);
        assert_eq!(
            start_all(&mut lanes),
            vec![(Lane::Stream, 20 + STREAM_SLOTS as u32)]
        );
        lanes.finished(Lane::Attach);
        assert_eq!(start_all(&mut lanes), vec![(Lane::Attach, 11)]);
        // A phone's read still waits only for a phone's slot, and a stream's read never
        // waits for one: here every shared slot is taken and a read for a stream runs anyway.
        lanes.finished(Lane::Stream);
        lanes.push(Lane::Read, 99);
        assert!(
            start_all(&mut lanes)
                .iter()
                .all(|(lane, _)| *lane == Lane::Stream)
        );
        // Both are cut short with their session; typing is not.
        assert!(Lane::Attach.cancellable() && Lane::Stream.cancellable());
        lanes.drop_queued_cancellable();
        assert_eq!(lanes.queued(), 0);
    }

    #[test]
    fn the_limits_add_up() {
        let mut lanes = Lanes::default();
        // Every kind at once never runs more than four.
        for n in 0..40u32 {
            lanes.push(
                [Lane::Ordered, Lane::LongPoll, Lane::Read][n as usize % 3],
                n,
            );
        }
        assert!(start_all(&mut lanes).len() <= 4);
        assert!(lanes.running() <= 4);
        // Finishing more than started never underflows.
        for lane in [
            Lane::Ordered,
            Lane::Read,
            Lane::LongPoll,
            Lane::Attach,
            Lane::Stream,
        ] {
            for _ in 0..8 {
                lanes.finished(lane);
            }
        }
        assert_eq!(lanes.running(), 0);
    }
}
