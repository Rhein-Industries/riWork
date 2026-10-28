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
        Arc, mpsc,
        mpsc::{RecvTimeoutError, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use riwork_engine::chat::{projection::DisplayMode, wire::Envelope};

use super::{
    dto::ChatDelivery,
    feed::{FeedMsg, Link},
    view::{ChatView, Stats},
};

pub const COALESCE: Duration = Duration::from_millis(16);
pub const CLOCK: Duration = Duration::from_secs(1);
pub const SUBSCRIBER_CAPACITY: usize = 64;

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
        let mut messages: Vec<WorkerMsg> = first.into_iter().collect();
        loop {
            match inbox.try_recv() {
                Ok(message) => messages.push(message),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        let mut events: Vec<Envelope> = Vec::new();
        for message in messages {
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

/// The worker's inbox as a feed sink.
pub fn sink(inbox: mpsc::Sender<WorkerMsg>) -> super::feed::Sink {
    Arc::new(move |message| inbox.send(WorkerMsg::Feed(message)).is_ok())
}
