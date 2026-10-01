//! Which of one device's requests may run at the same time.
//!
//! The phone keeps a long `shell.output` poll open while it types, so requests
//! of one device cannot be answered strictly one after the other. But not all
//! of them can run side by side either: typing batches and resizes change the
//! shell and its geometry, and they must reach it in the order they were sent.
//!
//! - `Ordered`: `shell.keys`, `shell.input`, `shell.resize`,
//!   `shell.resize.clear`, `shell.create` and `shell.close`. One at a time, in
//!   arrival order. This is what keeps the batch ledger, the viewport and the
//!   order of typed text intact. Creating and closing a terminal change what
//!   exists, and a request that ends half way (a session started but not yet
//!   written down, or one killed but still listed) is worse than a slow one,
//!   so a session ending does not cut them short either; typing waits behind
//!   them. (A creation also keeps its CLI alive when the connection itself is
//!   torn down: see `Rpc::create`.)
//! - `LongPoll`: a `shell.output` that waits for a change. At most two.
//! - `Read`: everything else, including a `shell.output` that does not wait and
//!   `shell.history`, a page of scrollback that never waits. It changes
//!   nothing, so it needs no order, and it takes one of the three shared
//!   slots, never the `Ordered` one.
//!
//! At most four requests run at once: the one `Ordered` slot and three shared
//! by `LongPoll` and `Read`. So a wait, or three, can never keep a typing
//! batch or a resize from starting (it has a slot of its own), and two long
//! polls leave a slot for reads. Requests that find no slot wait in a queue in
//! arrival order; a request that has to wait does not hold up a later one it
//! does not compete with.
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

/// Requests that are received but not started. A device that queues more is
/// not read from until some have started.
pub const MAX_QUEUED: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Ordered,
    LongPoll,
    Read,
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
            | "shell.close",
        ) => Lane::Ordered,
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
}

impl<T> Default for Lanes<T> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            ordered: 0,
            shared: 0,
            long_polls: 0,
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
        self.ordered + self.shared
    }

    fn admits(&self, lane: Lane) -> bool {
        match lane {
            Lane::Ordered => self.ordered < ORDERED_SLOTS,
            Lane::Read => self.shared < SHARED_SLOTS,
            Lane::LongPoll => self.shared < SHARED_SLOTS && self.long_polls < LONG_POLL_SLOTS,
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
    fn requests_are_classified_by_what_they_change_and_how_long_they_last() {
        let ordered = [
            "shell.keys",
            "shell.input",
            "shell.resize",
            "shell.resize.clear",
            "shell.create",
            "shell.close",
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
        for lane in [Lane::Ordered, Lane::Read, Lane::LongPoll] {
            for _ in 0..8 {
                lanes.finished(lane);
            }
        }
        assert_eq!(lanes.running(), 0);
    }
}
