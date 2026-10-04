//! Which of one device's requests may run at the same time.
//!
//! The phone keeps a long `shell.output` poll open while it types, so requests
//! of one device cannot be answered strictly one after the other. But not all
//! of them can run side by side either: typing batches and resizes change the
//! shell and its geometry, and they must reach it in the order they were sent.
//!
//! - `Ordered`: `shell.keys`, `shell.input`, `shell.resize`,
//!   `shell.resize.clear`, `shell.create`, `shell.close`, `project.create`,
//!   `chat.create`, `chat.command` and `chat.stop`.
//!   One at a time, in arrival order. This is what keeps the batch ledger, the
//!   viewport and the order of typed text intact. Creating and closing a
//!   terminal, and creating a project, change what exists, and a request that
//!   ends half way (a session started but not yet written down, one killed but
//!   still listed, or a project folder made but not registered) is worse than a
//!   slow one, so a session ending does not cut them short either; typing waits
//!   behind them. (A creation also keeps its CLI alive when the connection
//!   itself is torn down: see `Rpc::create`, `Rpc::create_project` and
//!   `Rpc::chat_create`.) A message, an approval or a stop for a chat is typing
//!   of a kind: it is carried out in the order it was sent.
//! - `LongPoll`: a `shell.output` that waits for a change, and a `chat.events`
//!   that waits for an event (any `wait_ms` above 0). At most two.
//! - `Read`: everything else, including a `shell.output` that does not wait,
//!   `shell.history`, a page of scrollback that never waits, `chats.list` and a
//!   `chat.events` with `wait_ms` 0. It changes nothing, so it needs no order,
//!   and it takes one of the three shared slots, never the `Ordered` one.
//!
//! At most four of those run at once: the one `Ordered` slot and three shared
//! by `LongPoll` and `Read`. So a wait, or three, can never keep a typing
//! batch or a resize from starting (it has a slot of its own), and two long
//! polls leave a slot for reads. Requests that find no slot wait in a queue in
//! arrival order; a request that has to wait does not hold up a later one it
//! does not compete with.
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
use serde_json::Value;
use std::collections::VecDeque;

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

/// The lane of a decoded request. Anything unrecognizable is a `Read`: it is
/// answered with an error at once.
pub fn classify(request: &Value) -> Lane {
    let params = request.get("params");
    match request.get("method").and_then(Value::as_str) {
        Some(
            "shell.keys" | "shell.input" | "shell.resize" | "shell.resize.clear" | "shell.create"
            | "shell.close" | "project.create" | "chat.create" | "chat.command" | "chat.stop",
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
        Some("chat.events")
            if params
                .and_then(|p| p.get("wait_ms"))
                .and_then(Value::as_i64)
                .is_some_and(|ms| ms > 0) =>
        {
            Lane::LongPoll
        }
        // `shell.history` and every other method: shared, never waiting.
        _ => Lane::Read,
    }
}

/// Received requests and the slots they run in.
pub struct Lanes<T> {
    queue: VecDeque<(Lane, T)>,
    ordered: usize,
    shared: usize,
    long_polls: usize,
    attaching: usize,
    streaming: usize,
}

impl<T> Default for Lanes<T> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            ordered: 0,
            shared: 0,
            long_polls: 0,
            attaching: 0,
            streaming: 0,
        }
    }
}

impl<T> Lanes<T> {
    pub fn push(&mut self, lane: Lane, item: T) {
        self.queue.push_back((lane, item));
    }

    /// Received but not started.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Started and not finished.
    pub fn running(&self) -> usize {
        self.ordered + self.shared + self.attaching + self.streaming
    }

    fn admits(&self, lane: Lane) -> bool {
        match lane {
            Lane::Ordered => self.ordered < ORDERED_SLOTS,
            Lane::Read => self.shared < SHARED_SLOTS,
            Lane::LongPoll => self.shared < SHARED_SLOTS && self.long_polls < LONG_POLL_SLOTS,
            Lane::Attach => self.attaching < ATTACH_SLOTS,
            Lane::Stream => self.streaming < STREAM_SLOTS,
        }
    }

    /// The oldest queued request that has a slot, which it now holds until
    /// `finished`. Because every `Ordered` request needs the same single
    /// slot, the oldest one that fits is also the oldest one there is.
    pub fn next_ready(&mut self) -> Option<(Lane, T)> {
        let index = self.queue.iter().position(|(lane, _)| self.admits(*lane))?;
        let (lane, item) = self.queue.remove(index)?;
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
        Some((lane, item))
    }

    /// The slot of a request that ended, however it ended.
    pub fn finished(&mut self, lane: Lane) {
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

    /// Forget the queued requests that may be dropped (see `Lane::cancellable`).
    pub fn drop_queued_cancellable(&mut self) {
        self.queue.retain(|(lane, _)| !lane.cancellable());
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
    fn creating_and_closing_a_terminal_are_ordered_and_never_cut_short() {
        for method in ["shell.create", "shell.close"] {
            // Whatever the params look like, even none: validation answers it.
            for params in [
                json!({"project_id":"p","kind":"shell"}),
                json!({"if_changed":"h","wait_ms":5000}),
                json!(null),
            ] {
                let lane = classify(&request(method, params));
                assert_eq!(lane, Lane::Ordered, "{method}");
                assert!(!lane.cancellable(), "{method}");
            }
        }
        // Only the exact names: look-alikes are plain reads.
        for method in ["shell.creates", "shell", "shell.create.", "Shell.create"] {
            assert_eq!(
                classify(&request(method, json!({}))),
                Lane::Read,
                "{method}"
            );
        }
    }

    #[test]
    fn creating_a_project_is_ordered_and_never_cut_short() {
        // Whatever the params look like, even none: validation answers it.
        for params in [
            json!({"name":"Fresh"}),
            json!({"name":"Fresh","git":false}),
            json!({"if_changed":"h","wait_ms":5000}),
            json!(null),
        ] {
            let lane = classify(&request("project.create", params));
            assert_eq!(lane, Lane::Ordered);
            assert!(!lane.cancellable());
        }
        // Only the exact name: look-alikes are plain reads.
        for method in [
            "project.creates",
            "project",
            "project.create.",
            "Project.create",
            "projects.create",
            "project.add",
        ] {
            assert_eq!(
                classify(&request(method, json!({}))),
                Lane::Read,
                "{method}"
            );
        }
        // Queued behind typing and terminal creation, as one at a time.
        let mut lanes = Lanes::default();
        lanes.push(Lane::Ordered, "shell.keys");
        lanes.push(Lane::Ordered, "project.create");
        lanes.push(Lane::Ordered, "shell.create");
        assert_eq!(lanes.next_ready().map(|(_, m)| m), Some("shell.keys"));
        assert!(
            lanes.next_ready().is_none(),
            "one ordered request at a time"
        );
        lanes.finished(Lane::Ordered);
        assert_eq!(lanes.next_ready().map(|(_, m)| m), Some("project.create"));
        // A phone that goes away does not drop it from the queue.
        lanes.drop_queued_cancellable();
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
            "chat.create",
            "chat.command",
            "chat.stop",
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
            "chats.list",
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
