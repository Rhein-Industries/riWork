//! A chat handle's worker thread: it owns the [`ChatView`], folds the feed's
//! events in batches, applies the UI's disclosure/viewport requests, and pushes
//! coalesced deliveries to the JS subscriber (contracts §1.3):
//!
//! - the first change after a quiet period is delivered at once; later ones are
//!   merged and delivered on the trailing edge, at most one per 16 ms;
//! - a batch of waiting events is folded with one reprojection;
//! - the only timer is the 1 s activity clock, and only while a turn runs; a quiet
//!   chat blocks on its channel and delivers nothing;
//! - the subscriber queue is bounded; on overflow it is cleared and one `reset`
//!   with `resync: true` replaces what was queued.

use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
        mpsc::{RecvTimeoutError, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use riwork_engine::chat::{projection::DisplayMode, wire::Envelope};

use super::{
    dto::{ChatDelivery, SearchHitDto},
    feed::{FeedMsg, Link},
    view::{ChatView, Stats},
};

pub const COALESCE: Duration = Duration::from_millis(16);
pub const CLOCK: Duration = Duration::from_secs(1);
pub const SUBSCRIBER_CAPACITY: usize = 64;
// Stop draining a busy producer so folding and delivery get a turn. Transport
// batches remain intact, so the final message may cross this event budget.
const INBOX_BATCH_BUDGET: usize = 1024;

/// One Find consumer per handle. Queued obsolete searches retain no query text
/// and settle immediately; their lightweight tickets preserve inbox ordering.
#[derive(Clone, Default)]
pub struct LatestSearch {
    state: Arc<SearchState>,
}

#[derive(Default)]
struct SearchState {
    generation: AtomicU64,
    pending: Mutex<Option<PendingSearch>>,
}

struct PendingSearch {
    generation: u64,
    query: String,
    reply: async_channel::Sender<Option<Vec<SearchHitDto>>>,
}

pub struct SearchTicket {
    state: Arc<SearchState>,
    generation: u64,
}

impl LatestSearch {
    pub fn request(
        &self,
        query: String,
    ) -> (
        SearchTicket,
        async_channel::Receiver<Option<Vec<SearchHitDto>>>,
    ) {
        let (reply, result) = async_channel::bounded(1);
        let mut pending = self
            .state
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Publish cancellation before the caller enqueues this request, allowing
        // the currently executing search to stop without draining the inbox.
        let generation = self
            .state
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        if let Some(previous) = pending.replace(PendingSearch {
            generation,
            query,
            reply,
        }) {
            let _ = previous.reply.try_send(None);
        }
        (
            SearchTicket {
                state: self.state.clone(),
                generation,
            },
            result,
        )
    }

    pub fn cancel(&self) {
        let mut pending = self
            .state
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.state.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(previous) = pending.take() {
            let _ = previous.reply.try_send(None);
        }
    }
}

impl SearchTicket {
    fn cancelled(&self) -> bool {
        self.state.generation.load(Ordering::Acquire) != self.generation
    }

    fn run(self, view: &mut ChatView) {
        let pending = {
            let mut slot = self
                .state
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if slot
                .as_ref()
                .is_some_and(|pending| pending.generation == self.generation)
            {
                slot.take()
            } else {
                None
            }
        };
        if let Some(pending) = pending {
            let current = || !self.cancelled() && !pending.reply.is_closed();
            let result = view.search_while(&pending.query, current);
            // Publish under the same lock as request/cancel so a cancellation
            // cannot win and then receive the completed, superseded result.
            let _guard = self
                .state
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let result = if self.cancelled() { None } else { result };
            let _ = pending.reply.try_send(result);
        }
    }
}

impl Drop for SearchTicket {
    fn drop(&mut self) {
        // A closed/panicked worker must also settle its pending search, even
        // when the handle still exists. Never clear a newer generation's slot.
        let mut slot = self
            .state
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot
            .as_ref()
            .is_some_and(|pending| pending.generation == self.generation)
        {
            let _ = slot.take().unwrap().reply.try_send(None);
        }
    }
}

pub enum WorkerMsg {
    Feed(FeedMsg),
    Viewport(usize, usize),
    Toggle(String),
    ExpandAll(bool),
    Mode(DisplayMode),
    Subscribe(Subscriber),
    /// Work that must stay in order with the handle's other requests (draft
    /// saves), run on this thread, never on the engine thread.
    Run(Box<dyn FnOnce(&mut ChatView) + Send>),
    /// Opt-in Find work; unlike Run, superseded tickets may be skipped.
    Search(SearchTicket),
    /// Test seam: report the view's counters.
    Stats(mpsc::Sender<Stats>),
    Close,
}

/// The JS side of one subscription: a bounded queue the engine task awaits.
pub struct Subscriber {
    pub sender: async_channel::Sender<ChatDelivery>,
    /// Kept to clear the queue on overflow.
    pub drain: async_channel::Receiver<ChatDelivery>,
}

impl Subscriber {
    pub fn pair() -> (Self, async_channel::Receiver<ChatDelivery>) {
        let (sender, receiver) = async_channel::bounded(SUBSCRIBER_CAPACITY);
        (
            Self {
                sender,
                drain: receiver.clone(),
            },
            receiver,
        )
    }
}

/// Hooks the worker calls outside the view (draft forgetting on deletion).
pub type OnLink = Arc<dyn Fn(Link) + Send + Sync>;

/// `init` runs first on the worker (it may read settings); `on_close` runs last
/// (it stops the feed).
pub fn spawn(
    mut view: ChatView,
    inbox: mpsc::Receiver<WorkerMsg>,
    init: Box<dyn FnOnce(&mut ChatView) + Send>,
    on_link: OnLink,
    on_close: Box<dyn FnOnce() + Send>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("riwork-chat-view".into())
        .spawn(move || {
            init(&mut view);
            run(&mut view, &inbox, &on_link);
            on_close();
        })
        .expect("spawn chat view worker")
}

fn run(view: &mut ChatView, inbox: &mpsc::Receiver<WorkerMsg>, on_link: &OnLink) {
    let mut subscriber: Option<Subscriber> = None;
    let mut last_delivery: Option<Instant> = None;
    let mut next_tick: Option<Instant> = None;
    let mut waiting = false;
    loop {
        let now = Instant::now();
        let deadline = [
            waiting.then(|| last_delivery.map_or(now, |at| at + COALESCE)),
            next_tick,
        ]
        .into_iter()
        .flatten()
        .min();
        let first = match deadline {
            Some(deadline) => match inbox.recv_timeout(deadline.saturating_duration_since(now)) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match inbox.recv() {
                Ok(message) => Some(message),
                Err(_) => return,
            },
        };
        let messages = drain_batch(first, inbox);
        let mut events: Vec<Envelope> = Vec::new();
        for message in messages {
            // Cancelled tickets are not fold barriers: an obsolete Find must
            // not split a contiguous batch of streaming events into projections.
            if matches!(&message, WorkerMsg::Search(ticket) if ticket.cancelled()) {
                continue;
            }
            if !matches!(message, WorkerMsg::Feed(FeedMsg::Events(_))) && !events.is_empty() {
                view.apply(&std::mem::take(&mut events));
            }
            match message {
                WorkerMsg::Feed(FeedMsg::Events(batch)) => events.extend(batch),
                WorkerMsg::Feed(FeedMsg::Link(link)) => {
                    view.set_link(link);
                    on_link(link);
                }
                WorkerMsg::Feed(FeedMsg::CommandFailed { error, .. }) => view.command_failed(error),
                WorkerMsg::Viewport(first, last) => view.set_viewport(first, last),
                WorkerMsg::Toggle(id) => {
                    view.toggle(&id);
                }
                WorkerMsg::ExpandAll(open) => view.expand_all(open),
                WorkerMsg::Mode(mode) => view.set_mode(mode),
                WorkerMsg::Subscribe(next) => {
                    let reset = view.reset();
                    if next.sender.try_send(reset).is_ok() {
                        subscriber = Some(next);
                        last_delivery = Some(Instant::now());
                    }
                }
                WorkerMsg::Run(work) => work(view),
                WorkerMsg::Search(ticket) => ticket.run(view),
                WorkerMsg::Stats(reply) => {
                    let _ = reply.send(view.stats);
                }
                WorkerMsg::Close => return,
            }
        }
        if !events.is_empty() {
            view.apply(&events);
        }
        let now = Instant::now();
        if view.turn_running() {
            match next_tick {
                Some(at) if at <= now => {
                    view.tick();
                    next_tick = Some(now + CLOCK);
                }
                None => next_tick = Some(now + CLOCK),
                _ => {}
            }
        } else {
            next_tick = None;
        }
        let due = last_delivery.is_none_or(|at| now >= at + COALESCE);
        if !due {
            waiting = true;
            continue;
        }
        waiting = false;
        let Some(patch) = view.take_patch() else {
            continue;
        };
        let Some(active) = subscriber.as_ref() else {
            // Nobody listens: the next subscriber starts from a reset.
            continue;
        };
        last_delivery = Some(now);
        match active.sender.try_send(patch) {
            Ok(()) => {}
            Err(async_channel::TrySendError::Full(_)) => {
                while active.drain.try_recv().is_ok() {}
                let mut reset = view.reset();
                reset.resync = true;
                let _ = active.sender.try_send(reset);
            }
            Err(async_channel::TrySendError::Closed(_)) => subscriber = None,
        }
    }
}

fn drain_batch(first: Option<WorkerMsg>, inbox: &mpsc::Receiver<WorkerMsg>) -> Vec<WorkerMsg> {
    fn weight(message: &WorkerMsg) -> usize {
        match message {
            WorkerMsg::Feed(FeedMsg::Events(events)) => events.len().max(1),
            _ => 1,
        }
    }
    let mut messages: Vec<_> = first.into_iter().collect();
    let mut work = messages.first().map_or(0, weight);
    while work < INBOX_BATCH_BUDGET {
        match inbox.try_recv() {
            Ok(message) => {
                work = work.saturating_add(weight(&message));
                messages.push(message);
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    messages
}

/// The worker's inbox as a feed sink.
pub fn sink(inbox: mpsc::Sender<WorkerMsg>) -> super::feed::Sink {
    Arc::new(move |message| inbox.send(WorkerMsg::Feed(message)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::view::system_clock;
    use riwork_engine::chat::model::{ChatEvent, Item, ItemBody, ItemStatus};

    fn event(seq: u64, text: &str) -> Envelope {
        Envelope {
            chat_id: "search-worker".into(),
            seq,
            event: ChatEvent::ItemCompleted {
                item: Item {
                    id: seq.to_string(),
                    turn_id: None,
                    status: ItemStatus::Completed,
                    presentation: Default::default(),
                    started_at: None,
                    ended_at: None,
                    body: ItemBody::AgentMessage { text: text.into() },
                },
            },
        }
    }

    fn view() -> ChatView {
        ChatView::new("search-worker", DisplayMode::Normal, system_clock())
    }

    fn start(view: ChatView, inbox: mpsc::Receiver<WorkerMsg>) -> thread::JoinHandle<()> {
        spawn(
            view,
            inbox,
            Box::new(|_| {}),
            Arc::new(|_| {}),
            Box::new(|| {}),
        )
    }

    #[test]
    fn superseded_queries_settle_before_the_worker_runs() {
        let latest = LatestSearch::default();
        let (first, result) = latest.request("old".into());
        let (second, newest) = latest.request("new".into());
        assert_eq!(result.try_recv().unwrap(), None);
        assert!(first.cancelled());
        drop(first);
        assert_eq!(
            latest.state.pending.lock().unwrap().as_ref().unwrap().query,
            "new"
        );
        latest.cancel();
        assert_eq!(newest.try_recv().unwrap(), None);
        drop(second);
        assert!(latest.state.pending.lock().unwrap().is_none());
    }

    #[test]
    fn cancelled_search_preserves_event_batching_and_ordinary_query_order() {
        let latest = LatestSearch::default();
        let (sender, inbox) = mpsc::channel();
        let (ordinary, ordinary_result) = mpsc::channel();
        let (stats, stats_result) = mpsc::channel();
        sender
            .send(WorkerMsg::Feed(FeedMsg::Events(vec![event(1, "before")])))
            .unwrap();
        let (old, old_result) = latest.request("obsolete".into());
        sender.send(WorkerMsg::Search(old)).unwrap();
        sender
            .send(WorkerMsg::Feed(FeedMsg::Events(vec![event(2, "middle")])))
            .unwrap();
        sender
            .send(WorkerMsg::Run(Box::new(move |view| {
                ordinary
                    .send((view.last_seq(), view.search("before")))
                    .unwrap();
            })))
            .unwrap();
        sender.send(WorkerMsg::Stats(stats)).unwrap();
        sender
            .send(WorkerMsg::Feed(FeedMsg::Events(vec![event(3, "after")])))
            .unwrap();
        let (new, new_result) = latest.request("after".into());
        sender.send(WorkerMsg::Search(new)).unwrap();
        sender.send(WorkerMsg::Close).unwrap();
        let thread = start(view(), inbox);
        thread.join().unwrap();
        assert_eq!(old_result.try_recv().unwrap(), None);
        let (seq, hits) = ordinary_result.recv().unwrap();
        assert_eq!(seq, 2);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "1");
        let stats = stats_result.recv().unwrap();
        assert_eq!(stats.events, 2);
        assert_eq!(
            stats.projection_runs, 1,
            "obsolete tickets must not split event batches"
        );
        let hits = new_result.try_recv().unwrap().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].id, "3",
            "the new search must run at its own inbox position"
        );
    }

    #[test]
    fn search_consumers_are_independent_and_empty_queries_complete() {
        let first = LatestSearch::default();
        let other = LatestSearch::default();
        let (first_ticket, first_result) = first.request("before".into());
        let (other_ticket, other_result) = other.request("before".into());
        first.cancel();
        let mut view = view();
        view.apply(&[event(1, "before")]);
        first_ticket.run(&mut view);
        other_ticket.run(&mut view);
        assert_eq!(first_result.try_recv().unwrap(), None);
        assert_eq!(other_result.try_recv().unwrap().unwrap().len(), 1);
        let (empty, empty_result) = other.request("  ".into());
        empty.run(&mut view);
        assert_eq!(empty_result.try_recv().unwrap(), Some(vec![]));
    }

    #[test]
    fn worker_close_and_failed_send_settle_pending_searches() {
        let latest = LatestSearch::default();
        let (sender, inbox) = mpsc::channel();
        sender.send(WorkerMsg::Close).unwrap();
        let (ticket, result) = latest.request("queued after close".into());
        sender.send(WorkerMsg::Search(ticket)).unwrap();
        start(view(), inbox).join().unwrap();
        assert_eq!(result.try_recv().unwrap(), None);
        let (ticket, result) = latest.request("worker is gone".into());
        drop(sender.send(WorkerMsg::Search(ticket)).unwrap_err());
        assert_eq!(result.try_recv().unwrap(), None);
        assert!(latest.state.pending.lock().unwrap().is_none());
    }

    #[test]
    fn dropped_search_consumer_does_no_work_and_retains_no_state() {
        let latest = LatestSearch::default();
        let weak = Arc::downgrade(&latest.state);
        let (ticket, result) = latest.request("unused".into());
        drop(result);
        ticket.run(&mut view());
        assert!(latest.state.pending.lock().unwrap().is_none());
        drop(latest);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn draining_has_a_budget_and_preserves_fifo_order() {
        let (sender, inbox) = mpsc::channel();
        for index in 0..INBOX_BATCH_BUDGET + 2 {
            sender.send(WorkerMsg::Viewport(index, index)).unwrap();
        }
        let batch = drain_batch(Some(inbox.recv().unwrap()), &inbox);
        assert_eq!(batch.len(), INBOX_BATCH_BUDGET);
        for (index, message) in batch.into_iter().enumerate() {
            assert!(
                matches!(message, WorkerMsg::Viewport(first, last) if first == index && last == index)
            );
        }
        let batch = drain_batch(None, &inbox);
        assert_eq!(batch.len(), 2);
        assert!(matches!(
            batch[0],
            WorkerMsg::Viewport(INBOX_BATCH_BUDGET, INBOX_BATCH_BUDGET)
        ));
    }

    #[test]
    fn draining_counts_events_without_splitting_a_transport_batch() {
        let (sender, inbox) = mpsc::channel();
        sender
            .send(WorkerMsg::Feed(FeedMsg::Events(vec![
                event(1, "batch");
                INBOX_BATCH_BUDGET + 1
            ])))
            .unwrap();
        sender.send(WorkerMsg::Viewport(1, 2)).unwrap();
        let batch = drain_batch(None, &inbox);
        assert_eq!(batch.len(), 1);
        assert!(
            matches!(&batch[0], WorkerMsg::Feed(FeedMsg::Events(events)) if events.len() == INBOX_BATCH_BUDGET + 1)
        );
        assert!(matches!(
            inbox.try_recv().unwrap(),
            WorkerMsg::Viewport(1, 2)
        ));
    }

    #[test]
    #[ignore = "manual paired search-queue and event-fold latency measurement"]
    fn queued_search_worker_benchmark() {
        fn sample(coalesce: bool) -> (Duration, Duration) {
            let mut view = view();
            // Exceed the normalization budget: the uncached tail makes each
            // obsolete query expensive, even after the first query warmed it.
            let body = "ordinary transcript text without a match. ".repeat(1200);
            view.apply(&(1..=300).map(|seq| event(seq, &body)).collect::<Vec<_>>());
            let (sender, inbox) = mpsc::channel();
            let latest = LatestSearch::default();
            let mut cancelled = Vec::new();
            for index in 0..16 {
                let query = format!("missing-{index}");
                if coalesce {
                    let (ticket, result) = latest.request(query);
                    sender.send(WorkerMsg::Search(ticket)).unwrap();
                    cancelled.push(result);
                } else {
                    sender
                        .send(WorkerMsg::Run(Box::new(move |view| {
                            assert!(view.search(&query).is_empty());
                        })))
                        .unwrap();
                }
            }
            sender
                .send(WorkerMsg::Feed(FeedMsg::Events(vec![event(
                    301,
                    "final needle",
                )])))
                .unwrap();
            let (folded, fold_result) = mpsc::channel();
            sender
                .send(WorkerMsg::Run(Box::new(move |view| {
                    assert_eq!(view.last_seq(), 301);
                    folded.send(Instant::now()).unwrap();
                })))
                .unwrap();
            let (finished, finish_result) = mpsc::channel();
            let latest_result = if coalesce {
                let (ticket, result) = latest.request("needle".into());
                sender.send(WorkerMsg::Search(ticket)).unwrap();
                Some(result)
            } else {
                sender
                    .send(WorkerMsg::Run(Box::new(move |view| {
                        finished.send(view.search("needle")).unwrap();
                    })))
                    .unwrap();
                None
            };
            sender.send(WorkerMsg::Close).unwrap();
            let start = Instant::now();
            let thread = start_worker(view, inbox);
            let folded = fold_result.recv().unwrap().duration_since(start);
            thread.join().unwrap();
            let elapsed = start.elapsed();
            let hits = if let Some(result) = latest_result {
                result.try_recv().unwrap().unwrap()
            } else {
                finish_result.recv().unwrap()
            };
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].id, "301");
            for result in cancelled {
                assert_eq!(result.try_recv().unwrap(), None);
            }
            (folded, elapsed)
        }
        // Keep timing outside fixture construction and use the real worker.
        fn start_worker(
            view: ChatView,
            inbox: mpsc::Receiver<WorkerMsg>,
        ) -> thread::JoinHandle<()> {
            start(view, inbox)
        }
        let mut ordinary = Vec::new();
        let mut latest = Vec::new();
        for _ in 0..3 {
            ordinary.push(sample(false));
            latest.push(sample(true));
        }
        ordinary.sort();
        latest.sort();
        eprintln!(
            "300 × 49 KiB items, 16 obsolete queries, actual worker median of 3: ordinary fold {:?}, total {:?}; latest fold {:?}, total {:?}",
            ordinary[1].0, ordinary[1].1, latest[1].0, latest[1].1
        );
    }
}
