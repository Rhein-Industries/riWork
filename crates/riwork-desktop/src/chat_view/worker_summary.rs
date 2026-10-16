//! Coalesced, background descriptions of observed worker output. The model never
//! supplies lifecycle state and never receives a command in the parent's conversation.
use super::*;
use crate::chat::worker_summary::{self as backend, Snapshot};
use std::collections::BTreeMap;

const MIN_INTERVAL: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_secs(2);
const BATCH: usize = backend::MAX_WORKERS;

#[derive(Default)]
struct Queue {
    identity: String,
    sources: BTreeMap<String, Snapshot>,
    attempted: BTreeMap<String, Snapshot>,
    attempt_order: BTreeMap<String, u64>,
    serial: u64,
    summaries: BTreeMap<String, String>,
    last_attempt: Option<Instant>,
}

impl Queue {
    fn update(&mut self, identity: String, sources: Vec<Snapshot>) {
        if self.identity != identity {
            *self = Self {
                identity,
                ..Self::default()
            };
        }
        let sources: BTreeMap<_, _> = sources
            .into_iter()
            .filter_map(|mut source| {
                source.output = backend::compress_output(&source.output);
                if source.output.is_empty() {
                    return None;
                }
                Some((source.id.clone(), source))
            })
            .collect();
        // A new assignment must never inherit the previous assignment's description.
        self.summaries.retain(|id, _| {
            sources
                .get(id)
                .is_some_and(|now| self.sources.get(id).is_some_and(|before| before == now))
        });
        self.attempted.retain(|id, _| sources.contains_key(id));
        self.attempt_order.retain(|id, _| sources.contains_key(id));
        self.sources = sources;
    }

    fn pending(&self) -> bool {
        self.sources
            .iter()
            .any(|(id, source)| self.attempted.get(id) != Some(source))
    }

    fn delay(&self, now: Instant) -> Option<Duration> {
        self.pending().then(|| {
            self.last_attempt.map_or(SETTLE, |last| {
                MIN_INTERVAL
                    .saturating_sub(now.saturating_duration_since(last))
                    .max(SETTLE)
            })
        })
    }

    fn begin(&mut self, now: Instant) -> Vec<Snapshot> {
        let mut changed: Vec<_> = self
            .sources
            .iter()
            .filter(|(id, source)| self.attempted.get(*id) != Some(*source))
            .collect();
        changed.sort_by_key(|(id, _)| self.attempt_order.get(*id).copied().unwrap_or(0));
        let batch: Vec<_> = changed
            .into_iter()
            .take(BATCH)
            .map(|(_, source)| source.clone())
            .collect();
        if !batch.is_empty() {
            self.last_attempt = Some(now);
            for source in &batch {
                self.serial = self.serial.saturating_add(1);
                self.attempt_order.insert(source.id.clone(), self.serial);
                self.attempted.insert(source.id.clone(), source.clone());
            }
        }
        batch
    }

    fn finish(
        &mut self,
        identity: &str,
        batch: &[Snapshot],
        result: Result<BTreeMap<String, String>, String>,
    ) {
        if self.identity != identity {
            return;
        }
        let Ok(summaries) = result else {
            return;
        };
        for source in batch {
            if self
                .sources
                .get(&source.id)
                .is_some_and(|now| now == source)
                && let Some(text) = summaries
                    .get(&source.id)
                    .filter(|text| !text.trim().is_empty())
            {
                self.summaries.insert(source.id.clone(), text.clone());
            }
        }
    }
}

#[derive(Default)]
pub(super) struct SummaryState {
    active: bool,
    queue: Queue,
    task: Option<Task<()>>,
    in_flight: bool,
}

fn identity(info: &ChatInfo) -> String {
    serde_json::to_string(&(
        &info.id,
        info.provider,
        &info.model,
        &info.codex_account_id,
        info.claude_billing,
        &info.cwd,
    ))
    .unwrap_or_default()
}

impl ChatView {
    /// The workspace names visible tabs; covered/background chats spend no model calls.
    pub(crate) fn set_worker_summaries_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.worker_summaries.active = active;
        if !active && !self.worker_summaries.in_flight {
            self.worker_summaries.task = None;
        }
        if active {
            self.schedule_worker_summaries(cx);
        }
    }

    pub(super) fn schedule_worker_summaries(&mut self, cx: &mut Context<Self>) {
        let Some(parent) = self.info().cloned() else {
            return;
        };
        let sources = self.worker_summary_sources();
        self.worker_summaries
            .queue
            .update(identity(&parent), sources);
        self.workers.summaries = self.worker_summaries.queue.summaries.clone();
        if !self.worker_summaries.active
            || self.catalog_home.is_none()
            || self.worker_summaries.task.is_some()
            || self.deleted()
        {
            return;
        }
        let Some(delay) = self.worker_summaries.queue.delay(Instant::now()) else {
            return;
        };
        self.worker_summaries.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let job = this
                .update(cx, |view, _| {
                    if !view.worker_summaries.active || view.deleted() {
                        return None;
                    }
                    let parent = view.info()?.clone();
                    let home = view.catalog_home.clone()?;
                    let sources = view.worker_summary_sources();
                    view.worker_summaries
                        .queue
                        .update(identity(&parent), sources);
                    let batch = view.worker_summaries.queue.begin(Instant::now());
                    if batch.is_empty() {
                        return None;
                    }
                    view.worker_summaries.in_flight = true;
                    Some((
                        home,
                        parent,
                        batch,
                        view.worker_summaries.queue.identity.clone(),
                    ))
                })
                .ok()
                .flatten();
            let Some((home, parent, batch, identity)) = job else {
                let _ = this.update(cx, |view, _| view.worker_summaries.task = None);
                return;
            };
            let request_batch = batch.clone();
            let result = cx
                .background_executor()
                .spawn(async move { backend::summarize(&home, &parent, &request_batch) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.worker_summaries.task = None;
                view.worker_summaries.in_flight = false;
                view.worker_summaries
                    .queue
                    .finish(&identity, &batch, result);
                view.workers.summaries = view.worker_summaries.queue.summaries.clone();
                cx.notify();
                view.schedule_worker_summaries(cx);
            });
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(id: &str, assignment: &str, output: &str) -> Snapshot {
        Snapshot {
            id: id.into(),
            assignment: Some(assignment.into()),
            output: output.into(),
        }
    }

    #[test]
    fn output_changes_coalesce_and_unchanged_failures_spend_no_more_calls() {
        let mut queue = Queue::default();
        let start = Instant::now();
        let old = source("worker:a", "Fix polling", "Reading poller");
        queue.update("model-a".into(), vec![old]);
        assert_eq!(queue.delay(start), Some(SETTLE));
        let latest = source("worker:a", "Fix polling", "Testing backoff");
        queue.update("model-a".into(), vec![latest.clone()]);
        let batch = queue.begin(start);
        assert_eq!(batch, vec![latest.clone()]);
        queue.finish("model-a", &batch, Err("offline".into()));
        queue.update("model-a".into(), vec![latest]);
        assert!(!queue.pending());
        queue.update(
            "model-a".into(),
            vec![source("worker:a", "Fix polling", "Tests pass")],
        );
        assert_eq!(
            queue.delay(start + Duration::from_secs(5)),
            Some(Duration::from_secs(25))
        );
    }

    #[test]
    fn late_results_cannot_describe_a_new_assignment_or_a_different_model() {
        let mut queue = Queue::default();
        queue.update("model-a".into(), vec![source("a", "First job", "Reading")]);
        let batch = queue.begin(Instant::now());
        queue.update("model-a".into(), vec![source("a", "Second job", "Testing")]);
        queue.finish(
            "model-a",
            &batch,
            Ok(BTreeMap::from([("a".into(), "First job done".into())])),
        );
        assert!(queue.summaries.is_empty());
        queue.update("model-b".into(), vec![source("a", "First job", "Reading")]);
        queue.finish(
            "model-a",
            &batch,
            Ok(BTreeMap::from([("a".into(), "Old model".into())])),
        );
        assert!(queue.summaries.is_empty());
    }

    #[test]
    fn changed_output_retires_old_descriptions_and_batches_do_not_starve_other_workers() {
        let mut queue = Queue::default();
        let sources: Vec<_> = (0..12)
            .map(|at| source(&format!("{at:02}"), "Job", "Reading"))
            .collect();
        queue.update("model".into(), sources.clone());
        let batch = queue.begin(Instant::now());
        assert_eq!(batch.len(), BATCH);
        queue.finish(
            "model",
            &batch,
            Ok(BTreeMap::from([(
                "00".into(),
                "Inspecting the retry path".into(),
            )])),
        );
        let mut newer = sources;
        for source in &mut newer {
            source.output = "Testing".into();
        }
        queue.update("model".into(), newer);
        assert!(
            queue.summaries.is_empty(),
            "new evidence retires old descriptions"
        );
        let next = queue.begin(Instant::now() + MIN_INTERVAL);
        assert!(next.iter().any(|source| source.id == "11"));
        queue.update("model".into(), Vec::new());
        assert!(queue.summaries.is_empty());
    }

    #[test]
    fn a_blocker_arriving_during_inference_rejects_the_old_description() {
        let mut queue = Queue::default();
        queue.update("model".into(), vec![source("a", "Job", "Running tests")]);
        let batch = queue.begin(Instant::now());
        queue.update(
            "model".into(),
            vec![source("a", "Job", "A blocker appeared")],
        );
        queue.finish(
            "model",
            &batch,
            Ok(BTreeMap::from([(
                "a".into(),
                "Old optimistic result".into(),
            )])),
        );
        assert!(queue.summaries.is_empty());
    }
}
