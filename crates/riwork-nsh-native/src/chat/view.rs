//! One chat's view model on its worker thread: the event fold, the engine
//! projection, the flattened virtual-list rows and their bodies, and the patch
//! that the next delivery carries.
//!
//! Structural guards (D-PERF-1): token deltas never reproject (they become
//! `append`/`splice` ops on a body already sent, or mark it stale); a batch of
//! structural events reprojects once; bodies are materialized only for rows near
//! the viewport, so a 2,200-call disclosed turn builds fewer than 100 bodies per
//! viewport. Everything here is synchronous and free of IO; the worker owns the
//! clock, the channel and the 16 ms coalescing.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use riwork_engine::chat::{
    lineage,
    model::{
        Approval, ApprovalKind, ApprovalMode, ChatEvent, ChatOrigin, ChatState, Decision, Delta, Item, ItemBody,
        ItemStatus, LineageContext, LineageKind, NoticeLevel, Provider, Question, SourceKind,
        StepStatus, Transcript, TurnOutcome,
    },
    projection::{
        self, DisclosureState, DisplayMode, ProjectionCache, ProjectionOptions, Row, RowKind,
    },
    wire::Envelope,
};
use serde_json::Value;

use super::{
    activity_summary,
    dto::{
        AppendDto, ApprovalDto, BodyDto, ChatDelivery, ChatPanelDto, ChoiceDto, LineageRowDto,
        NoticeDto, OptionDto, OriginDto, PickRowDto, PromptDto, QuestionDto, RowDto, SpliceDto,
        StepDto, ToolbarDto, decision_name, wire_name,
    },
    feed::Link,
    markdown::StreamingDocument,
    toolbar,
};

/// Rows kept materialized beyond each end of the reported viewport.
pub const OVERSCAN: usize = 16;
/// Before the List reports a viewport, the tail is what is on screen.
pub const TAIL_WINDOW: usize = 40;
/// A command body carries at most this much of the end of its output.
pub const OUTPUT_TAIL: usize = 32 * 1024;
/// Detail bodies (tool input/output) are cut to this many bytes.
pub const DETAIL_LIMIT: usize = 32 * 1024;

/// What a batch of events did (carried from GPUI `chat_view/state.rs`).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub appended: usize,
    pub touched: BTreeSet<usize>,
    pub events: usize,
    pub projection_changed: bool,
}

/// The transcript fold with stable item positions and turn outcomes.
#[derive(Default)]
pub struct Fold {
    pub transcript: Transcript,
    pub last_seq: u64,
    pub turn_finished: bool,
    pub completed: Vec<(String, TurnOutcome, usize)>,
    positions: HashMap<String, usize>,
    /// Item ids whose content changed in the last batch (deltas included).
    pub changed: BTreeSet<String>,
    /// Items replaced whole (started, completed, or closed by their turn) in the
    /// last batch: their bodies are rebuilt, not appended to.
    pub replaced: BTreeSet<String>,
    /// Text and output deltas of the last batch, per item, in order.
    pub deltas: HashMap<String, String>,
}

impl Fold {
    /// Fold envelopes in, oldest first; an envelope at or before the last one
    /// applied is a repeat and changes nothing (CHATT-003).
    pub fn apply(&mut self, envelopes: &[Envelope]) -> Applied {
        let before = self.transcript.items.len();
        let mut touched = BTreeSet::new();
        let mut events = 0;
        let mut projection_changed = false;
        self.changed.clear();
        self.replaced.clear();
        self.deltas.clear();
        for envelope in envelopes {
            if envelope.seq <= self.last_seq {
                continue;
            }
            self.last_seq = envelope.seq;
            events += 1;
            projection_changed |= matches!(
                envelope.event,
                ChatEvent::ItemStarted { .. }
                    | ChatEvent::ItemCompleted { .. }
                    | ChatEvent::TurnStarted { .. }
                    | ChatEvent::ApprovalResolved { .. }
                    | ChatEvent::QuestionResolved { .. }
                    | ChatEvent::TurnCompleted { .. }
                    | ChatEvent::State { .. }
                    | ChatEvent::QuestionRequested { .. }
                    | ChatEvent::ApprovalRequested { .. }
            );
            match &envelope.event {
                ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } => {
                    if let Some(&at) = self.positions.get(&item.id) {
                        touched.insert(at);
                    }
                    self.changed.insert(item.id.clone());
                    self.replaced.insert(item.id.clone());
                }
                ChatEvent::ItemDelta { item_id, delta } => {
                    if let Some(&at) = self.positions.get(item_id) {
                        touched.insert(at);
                    }
                    self.changed.insert(item_id.clone());
                    let (Delta::Text(text) | Delta::Output(text)) = delta;
                    self.deltas
                        .entry(item_id.clone())
                        .or_default()
                        .push_str(text);
                }
                ChatEvent::TurnCompleted {
                    turn_id, outcome, ..
                } => {
                    self.completed.push((
                        turn_id.clone(),
                        outcome.clone(),
                        self.transcript.items.len(),
                    ));
                    self.turn_finished = true;
                    for (at, item) in self.transcript.items.iter().enumerate() {
                        if item.status == ItemStatus::InProgress {
                            touched.insert(at);
                            self.changed.insert(item.id.clone());
                            self.replaced.insert(item.id.clone());
                        }
                    }
                }
                _ => {}
            }
            self.transcript
                .apply_sequenced(envelope.seq, &envelope.event);
            for (at, item) in self
                .transcript
                .items
                .iter()
                .enumerate()
                .skip(self.positions.len())
            {
                self.positions.insert(item.id.clone(), at);
            }
        }
        touched.retain(|at| *at < before);
        Applied {
            appended: self.transcript.items.len() - before,
            touched,
            events,
            projection_changed,
        }
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.positions
            .get(id)
            .and_then(|&at| self.transcript.items.get(at))
            .filter(|item| item.id == id)
    }
}

/// Counters the structural guards assert.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub projection_runs: u64,
    pub bodies_built: u64,
    pub events: u64,
    pub deliveries: u64,
}

/// Body ops waiting for the next delivery, compacted per key so a burst of
/// deltas between two deliveries becomes one op (contracts §1.3 coalescing).
#[derive(Default)]
struct Pending {
    upsert: BTreeMap<String, RowDto>,
    remove: BTreeSet<String>,
    order: bool,
    bodies: BTreeMap<String, BodyDto>,
    splices: BTreeMap<String, SpliceDto>,
    appends: BTreeMap<String, String>,
    drop: BTreeSet<String>,
    panel: bool,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.upsert.is_empty()
            && self.remove.is_empty()
            && !self.order
            && self.bodies.is_empty()
            && self.splices.is_empty()
            && self.appends.is_empty()
            && self.drop.is_empty()
            && !self.panel
    }

    fn set(&mut self, key: &str, body: BodyDto) {
        self.splices.remove(key);
        self.appends.remove(key);
        self.drop.remove(key);
        self.bodies.insert(key.to_owned(), body);
    }

    fn splice(&mut self, key: &str, from: usize, blocks: Vec<Value>, len: usize) {
        if let Some(BodyDto::Markdown { blocks: whole }) = self.bodies.get_mut(key) {
            whole.truncate(from);
            whole.extend(blocks);
            return;
        }
        let composed = match self.splices.remove(key) {
            Some(earlier) if from > earlier.from => {
                let mut kept = earlier.blocks;
                kept.truncate(from - earlier.from);
                kept.extend(blocks);
                SpliceDto {
                    key: key.to_owned(),
                    from: earlier.from,
                    len,
                    blocks: kept,
                }
            }
            _ => SpliceDto {
                key: key.to_owned(),
                from,
                len,
                blocks,
            },
        };
        self.splices.insert(key.to_owned(), composed);
    }

    fn append(&mut self, key: &str, text: &str) {
        match self.bodies.get_mut(key) {
            Some(BodyDto::Text { text: whole, .. }) => whole.push_str(text),
            Some(BodyDto::Command { output, .. }) => output.push_str(text),
            Some(_) => {}
            None => self
                .appends
                .entry(key.to_owned())
                .or_default()
                .push_str(text),
        }
    }

    fn forget(&mut self, key: &str) {
        self.bodies.remove(key);
        self.splices.remove(key);
        self.appends.remove(key);
        self.drop.insert(key.to_owned());
    }
}

pub type Clock = std::sync::Arc<dyn Fn() -> Option<u64> + Send + Sync>;

pub fn system_clock() -> Clock {
    std::sync::Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|elapsed| elapsed.as_millis() as u64)
    })
}

pub struct ChatView {
    chat_id: String,
    pub fold: Fold,
    link: Link,
    mode: DisplayMode,
    disclosure: DisclosureState,
    cache: ProjectionCache,
    visible: Vec<Row>,
    rows: Vec<RowDto>,
    worker_rows: Vec<super::workers::ChatWorker>,
    /// The first message of a chat made from a handoff document (its instructions and the
    /// document), which shows folded.
    handoff_message: Option<String>,
    file_counts: super::review::FileCounts,
    search_index: super::search::SearchIndex,
    /// Stable transcript positions of notices; streaming messages never enter
    /// this index, so banners and billing do not traverse message history.
    notices: BTreeSet<usize>,
    billing: super::billing::Projection,
    index: HashMap<String, usize>,
    /// Reported viewport `[first, last]`, or `None` (the tail is shown).
    viewport: Option<(usize, usize)>,
    /// Whether the reported viewport reached the last row: appended rows then
    /// join the window without waiting for the List's next report.
    tail: bool,
    /// Keys whose body the subscriber holds, and whether it is now stale.
    sent: HashMap<String, bool>,
    streams: HashMap<String, StreamingDocument>,
    pending: Pending,
    panel: Option<ChatPanelDto>,
    failures: Vec<String>,
    rev: u64,
    clock: Clock,
    pub stats: Stats,
}

impl ChatView {
    pub fn new(chat_id: impl Into<String>, mode: DisplayMode, clock: Clock) -> Self {
        Self {
            chat_id: chat_id.into(),
            fold: Fold::default(),
            link: Link::Connecting,
            mode,
            disclosure: DisclosureState::default(),
            cache: ProjectionCache::default(),
            visible: Vec::new(),
            rows: Vec::new(),
            worker_rows: Vec::new(),
            handoff_message: None,
            file_counts: super::review::FileCounts::default(),
            search_index: super::search::SearchIndex::default(),
            notices: BTreeSet::new(),
            billing: super::billing::Projection::default(),
            index: HashMap::new(),
            viewport: None,
            tail: true,
            sent: HashMap::new(),
            streams: HashMap::new(),
            pending: Pending::default(),
            panel: None,
            failures: Vec::new(),
            rev: 0,
            clock,
            stats: Stats::default(),
        }
    }

    pub fn rows(&self) -> &[RowDto] {
        &self.rows
    }

    pub fn link(&self) -> Link {
        self.link
    }

    pub fn mode(&self) -> DisplayMode {
        self.mode
    }

    /// A turn runs: the worker keeps the 1 s activity clock (CHATT-034).
    pub fn turn_running(&self) -> bool {
        self.fold.transcript.turn_id.is_some()
    }

    pub fn last_seq(&self) -> u64 {
        self.fold.last_seq
    }

    /// Fold a batch of events: reproject at most once, and only for structural
    /// events; text and output deltas touch bodies already sent.
    pub fn apply(&mut self, envelopes: &[Envelope]) -> Applied {
        let since = self.fold.last_seq;
        let applied = self.fold.apply(envelopes);
        self.stats.events += applied.events as u64;
        if applied.events == 0 {
            return applied;
        }
        self.search_index.invalidate(applied.touched.iter().copied());
        // Whole-item replacements include additions and turn-finalized items.
        // Removing the old contribution first also handles a changed turn,
        // status, file list, or a file/notice replaced with another item kind.
        let mut billing_changed = envelopes.iter().any(|envelope| {
            envelope.seq > since && matches!(&envelope.event, ChatEvent::Info { .. } | ChatEvent::Account { .. })
        });
        for id in &self.fold.replaced {
            let item = self.fold.item(id);
            self.file_counts.replace(id, item);
            if let Some(&at) = self.fold.positions.get(id) {
                if item.is_some_and(|item| matches!(&item.body, ItemBody::Notice { .. })) {
                    self.notices.insert(at);
                    billing_changed = true;
                } else if self.notices.remove(&at) {
                    billing_changed = true;
                }
            }
        }
        if billing_changed {
            let transcript = &self.fold.transcript;
            self.billing.refresh(transcript, self.notices.iter().rev().filter_map(|&at| transcript.items.get(at)));
        }
        self.pending.panel = true;
        if applied.projection_changed || applied.appended > 0 {
            let len = self.fold.transcript.items.len();
            let invalidated: HashSet<String> = applied
                .touched
                .iter()
                .copied()
                .chain(len.saturating_sub(applied.appended)..len)
                .filter_map(|at| self.fold.transcript.items.get(at))
                .map(|item| {
                    item.turn_id
                        .clone()
                        .unwrap_or_else(|| format!("legacy:{}", item.id))
                })
                .collect();
            self.reproject(&invalidated);
        }
        let changed: Vec<String> = self.fold.changed.iter().cloned().collect();
        for id in changed {
            self.refresh_body(&id);
        }
        applied
    }

    /// A deleted chat drops its rows' bodies; its panel says so.
    pub fn set_link(&mut self, link: Link) {
        if self.link != link {
            self.link = link;
            self.pending.panel = true;
        }
    }

    pub fn command_failed(&mut self, error: String) {
        self.failures
            .push(format!("Could not reach the chat: {error}"));
        self.pending.panel = true;
    }

    pub fn set_viewport(&mut self, first: usize, last: usize) {
        let last = last.max(first);
        self.viewport = Some((first, last));
        self.tail = last + 1 >= self.rows.len();
        self.sync_bodies();
    }

    pub fn toggle(&mut self, id: &str) -> bool {
        let Some(row) = find(&self.visible, id).filter(|row| is_disclosure(row.kind)) else {
            return false;
        };
        let open = row.open;
        self.disclosure.toggle(id, open);
        self.reproject(&HashSet::new());
        true
    }

    pub fn expand_all(&mut self, open: bool) {
        self.disclosure
            .reset_bulk_for_rows(open, self.mode, &self.visible);
        self.reproject(&HashSet::new());
    }

    pub fn set_mode(&mut self, mode: DisplayMode) {
        if self.mode != mode {
            self.mode = mode;
            self.disclosure.reset_mode();
            self.pending.panel = true;
            self.reproject(&HashSet::new());
        }
    }

    /// Advance running rows' clocks in place; never regroups (CHATT-034).
    pub fn tick(&mut self) {
        let now = (self.clock)();
        if !projection::refresh_timing(&mut self.visible, &self.fold.transcript, now) {
            return;
        }
        let mut updates = Vec::new();
        for (at, row) in self.rows.iter().enumerate() {
            if row.running
                && let Some(tree) = find(&self.visible, &row.id)
            {
                let next = self.row_dto(tree, row.depth);
                if next != *row {
                    updates.push((at, next));
                }
            }
        }
        for (at, next) in updates {
            self.pending.upsert.insert(next.key.clone(), next.clone());
            self.rows[at] = next;
        }
    }

    /// The full state, for a first delivery or after a resync. Bodies are only
    /// those of the window; everything already pending is folded in.
    pub fn reset(&mut self) -> ChatDelivery {
        self.pending = Pending::default();
        self.sent.clear();
        self.streams.clear();
        self.sync_bodies();
        let bodies = std::mem::take(&mut self.pending.bodies);
        self.pending = Pending::default();
        self.rev += 1;
        self.stats.deliveries += 1;
        let panel = self.panel_dto();
        self.panel = Some(panel.clone());
        ChatDelivery {
            kind: "reset".into(),
            rev: self.rev,
            seq: self.fold.last_seq,
            panel: Some(panel),
            rows: Some(self.rows.clone()),
            bodies,
            ..Default::default()
        }
    }

    /// What changed since the last delivery, if anything did.
    pub fn take_patch(&mut self) -> Option<ChatDelivery> {
        if self.pending.is_empty() {
            return None;
        }
        let pending = std::mem::take(&mut self.pending);
        let panel = if pending.panel {
            let panel = self.panel_dto();
            self.failures.clear();
            if self.panel.as_ref() == Some(&panel) {
                None
            } else {
                self.panel = Some(panel.clone());
                Some(panel)
            }
        } else {
            None
        };
        let delivery = ChatDelivery {
            kind: "patch".into(),
            rev: self.rev + 1,
            seq: self.fold.last_seq,
            panel,
            rows: None,
            upsert: pending
                .upsert
                .into_iter()
                .filter(|(key, _)| self.index.contains_key(key))
                .map(|(_, row)| row)
                .collect(),
            remove: pending.remove.into_iter().collect(),
            order: pending
                .order
                .then(|| self.rows.iter().map(|row| row.key.clone()).collect()),
            bodies: pending.bodies,
            drop: pending.drop.into_iter().collect(),
            splice: pending.splices.into_values().collect(),
            append: pending
                .appends
                .into_iter()
                .map(|(key, text)| AppendDto { key, text })
                .collect(),
            resync: false,
        };
        let empty = delivery.panel.is_none()
            && delivery.upsert.is_empty()
            && delivery.remove.is_empty()
            && delivery.order.is_none()
            && delivery.bodies.is_empty()
            && delivery.drop.is_empty()
            && delivery.splice.is_empty()
            && delivery.append.is_empty();
        if empty {
            return None;
        }
        self.rev += 1;
        self.stats.deliveries += 1;
        Some(delivery)
    }

    fn options(&self) -> ProjectionOptions {
        let mut disclosure = self.disclosure.clone();
        // A plan reads as a document: it starts open in both modes until a toggle
        // or a bulk command decides (CHATT-028).
        if disclosure.bulk.is_none() {
            for item in &self.fold.transcript.items {
                if matches!(item.body, ItemBody::Plan { .. }) {
                    disclosure.overrides.entry(item.id.clone()).or_insert(true);
                }
            }
        }
        ProjectionOptions {
            mode: self.mode,
            now_ms: (self.clock)(),
            disclosure,
            ..ProjectionOptions::default()
        }
    }

    fn reproject(&mut self, invalidated: &HashSet<String>) {
        self.worker_rows = super::workers::reported(&self.fold.transcript);
        self.handoff_message = handoff_message(&self.fold.transcript);
        self.stats.projection_runs += 1;
        let options = self.options();
        let mut rows = self
            .cache
            .project(&self.fold.transcript, &options, invalidated);
        let remapped = projection::remap_disclosures(&self.disclosure, &self.visible, &rows);
        if remapped.overrides != self.disclosure.overrides {
            self.disclosure = remapped;
            let options = self.options();
            rows = self
                .cache
                .project(&self.fold.transcript, &options, &HashSet::new());
        }
        let mut flat = Vec::with_capacity(self.rows.len());
        self.flatten(&rows, 0, &mut flat);
        self.visible = rows;
        let mut index = HashMap::with_capacity(flat.len());
        for (at, row) in flat.iter().enumerate() {
            index.insert(row.key.clone(), at);
        }
        for row in &flat {
            match self.index.get(&row.key).and_then(|&at| self.rows.get(at)) {
                Some(old) if old == row => {}
                _ => {
                    self.pending.upsert.insert(row.key.clone(), row.clone());
                }
            }
        }
        for old in &self.rows {
            if !index.contains_key(&old.key) {
                self.pending.upsert.remove(&old.key);
                self.pending.remove.insert(old.key.clone());
                if self.sent.remove(&old.key).is_some() {
                    self.pending.forget(&old.key);
                }
                self.streams.remove(&old.key);
            }
        }
        for row in &flat {
            self.pending.remove.remove(&row.key);
        }
        let same_order = self.rows.len() == flat.len()
            && self.rows.iter().zip(&flat).all(|(a, b)| a.key == b.key);
        if !same_order {
            self.pending.order = true;
        }
        self.rows = flat;
        self.index = index;
        self.sync_bodies();
    }

    fn flatten(&self, rows: &[Row], depth: u32, out: &mut Vec<RowDto>) {
        for row in rows {
            out.push(self.row_dto(row, depth));
            if row.open {
                self.flatten(&row.children, depth + 1, out);
            }
            self.flatten(&row.comments, depth, out);
        }
    }

    fn row_dto(&self, row: &Row, depth: u32) -> RowDto {
        let item = self.fold.item(&row.id);
        let title = if row.kind == RowKind::Activity && !row.running {
            self.activity_title(row)
        } else {
            first_line(&row.title)
        };
        let target = row
            .target
            .as_deref()
            .filter(|target| !target.trim().is_empty())
            .map(first_line)
            .filter(|target| target.trim() != title.trim());
        RowDto {
            artifacts: row.artifacts.iter().map(|artifact| super::dto::ArtifactDto {
                item_id: artifact.item_id.clone(),
                label: artifact.path.as_ref().map(|path| path.path.clone()).or_else(|| self.fold.item(&artifact.item_id).and_then(|item| item.presentation.images.get(artifact.image_index?)).map(|image| image.label.clone())).unwrap_or_else(|| "Image".into()),
                path: artifact.path.as_ref().map(|path| path.path.clone()),
                line: artifact.path.as_ref().and_then(|path| path.line), image_index: artifact.image_index,
                inline_image: self.fold.item(&artifact.item_id)
                    .and_then(|item| item.presentation.images.get(artifact.image_index?))
                    .is_some_and(|image| matches!(image.source,
                        riwork_engine::chat::model::ImageSource::Data { .. }
                        | riwork_engine::chat::model::ImageSource::Unavailable { .. })),
            }).collect(),
            file_count: item.map(|item| self.file_counts.count_item(item)).unwrap_or_default(),
            attachments: item.map(|item| item.presentation.attachments.iter().cloned().map(super::dto::AttachmentDto).collect()).unwrap_or_default(),
            key: row.id.clone(),
            id: row.id.clone(),
            kind: wire_name(&row.kind),
            depth,
            title,
            target,
            meta: item.and_then(|item| meta(row, item)),
            open: row.open,
            disclosure: is_disclosure(row.kind),
            failed_count: row.failed_count,
            running: row.running,
            duration_ms: row.duration_ms,
            current_label: row.current_label.clone(),
            provisional: row.provisional,
            request_id: row.request_id.clone(),
            status: item.map(|item| wire_name(&item.status)),
            has_body: has_body(row, item),
            lineage: item.and_then(lineage::of).map(|mark| LineageRowDto {
                kind: wire_name(&mark.kind),
                chat_id: mark.target_id.clone(),
                title: mark.target_title.clone(),
                provider: mark.provider.clone(),
                model: mark.model.clone(),
            }),
            handoff_bytes: item
                .filter(|item| self.handoff_message.as_deref() == Some(item.id.as_str()))
                .and_then(|item| match &item.body {
                    ItemBody::UserMessage { text } => Some(text.len() as u64),
                    _ => None,
                }),
        }
    }

    /// A finished turn's activity line states its work factually and says how
    /// it ended (CHATT-026), e.g. "Worked · Ran 1 command · Interrupted".
    fn activity_title(&self, row: &Row) -> String {
        let Some(turn) = row.id.strip_prefix("activity:") else {
            return first_line(&row.title);
        };
        let completed = &self.fold.completed;
        let summary = activity_summary::for_turn(&self.fold.transcript, completed, turn);
        let mut title = if summary == activity_summary::Summary::default() {
            row.title.clone()
        } else {
            let worked = row.title.split(" · ").next().unwrap_or(&row.title);
            format!("{worked} · {}", summary.label())
        };
        match completed
            .iter()
            .find(|(id, _, _)| id == turn)
            .map(|(_, outcome, _)| outcome)
        {
            Some(TurnOutcome::Interrupted) => title.push_str(" · Interrupted"),
            Some(TurnOutcome::Failed { message }) if !message.trim().is_empty() => {
                title.push_str(&format!(" · Failed: {}", message.trim()));
            }
            Some(TurnOutcome::Failed { .. }) => title.push_str(" · Failed"),
            Some(TurnOutcome::Completed) | None => {}
        }
        first_line(&title)
    }

    fn window(&self) -> (usize, usize) {
        let len = self.rows.len();
        if len == 0 {
            return (0, 0);
        }
        let (first, last) = match self.viewport {
            // Following the tail keeps the viewport's height at the new end.
            Some((first, last)) if self.tail => {
                let height = last - first;
                (len - 1 - height.min(len - 1), len - 1)
            }
            Some((first, last)) => (first.min(len - 1), last.min(len - 1)),
            None => (len.saturating_sub(TAIL_WINDOW), len - 1),
        };
        (
            first.saturating_sub(OVERSCAN),
            (last + OVERSCAN + 1).min(len),
        )
    }

    /// Materialize bodies entering the window and drop those far outside it.
    fn sync_bodies(&mut self) {
        let (start, end) = self.window();
        let keep_start = start.saturating_sub(2 * OVERSCAN);
        let keep_end = (end + 2 * OVERSCAN).min(self.rows.len());
        let gone: Vec<String> = self
            .sent
            .keys()
            .filter(|key| match self.index.get(*key) {
                Some(&at) => at < keep_start || at >= keep_end || !self.rows[at].has_body,
                None => true,
            })
            .cloned()
            .collect();
        for key in gone {
            self.sent.remove(&key);
            self.streams.remove(&key);
            self.pending.forget(&key);
        }
        for at in start..end {
            let row = &self.rows[at];
            if !row.has_body || self.sent.get(&row.key) == Some(&false) {
                continue;
            }
            let key = row.key.clone();
            if let Some(body) = self.build_body(&key) {
                self.pending.set(&key, body);
                self.sent.insert(key, false);
            }
        }
    }

    fn build_body(&mut self, key: &str) -> Option<BodyDto> {
        let at = *self.index.get(key)?;
        let item = self.fold.item(&self.rows[at].id)?;
        self.stats.bodies_built += 1;
        Some(match &item.body {
            ItemBody::AgentMessage { text } => {
                let mut stream = StreamingDocument::default();
                stream.update(text);
                let blocks = stream.blocks().to_vec();
                self.streams.insert(key.to_owned(), stream);
                BodyDto::Markdown { blocks }
            }
            ItemBody::UserMessage { text } => BodyDto::Text {
                text: typed_text(text),
                tone: "user".into(),
            },
            ItemBody::Reasoning { text } => BodyDto::Text {
                text: text.trim().to_owned(),
                tone: "reasoning".into(),
            },
            ItemBody::Notice { level, text, .. } => BodyDto::Text {
                text: text.clone(),
                tone: level_name(*level).into(),
            },
            ItemBody::Command {
                command,
                cwd,
                output,
                exit_code,
            } => {
                let (output, omitted) = tail(output, OUTPUT_TAIL);
                BodyDto::Command {
                    command: command.clone(),
                    cwd: cwd.clone(),
                    output: output.to_owned(),
                    omitted,
                    exit_code: *exit_code,
                    running: item.status == ItemStatus::InProgress,
                }
            }
            ItemBody::Plan { explanation, steps } => BodyDto::Steps {
                explanation: explanation.clone().filter(|text| !text.trim().is_empty()),
                steps: steps.iter().map(step).collect(),
            },
            ItemBody::Todo { items } => BodyDto::Steps {
                explanation: None,
                steps: items.iter().map(step).collect(),
            },
            ItemBody::FileChange { .. } => BodyDto::Changes { files: super::review::files(&self.fold.transcript, Some(&item.id)) },
            ItemBody::ToolCall { tool, input, output, .. } => {
                let structured = item.presentation.tool.clone().or_else(|| output.as_deref().and_then(|output| riwork_engine::chat::tool_output::parse(tool, input, output)));
                let mut text = String::new();
                if !input.is_null() {
                    text.push_str(&serde_json::to_string_pretty(input).unwrap_or_default());
                }
                if let Some(output) = output.as_deref().filter(|text| !text.is_empty()) {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(tail(output, DETAIL_LIMIT).0);
                }
                if let Some(structured) = structured {
                    BodyDto::Tool { view: serde_json::to_value(structured).unwrap_or(Value::Null), raw: cut(&text, DETAIL_LIMIT).to_owned() }
                } else {
                    BodyDto::Detail { text: cut(&text, DETAIL_LIMIT).to_owned() }
                }
            }
            ItemBody::WebSearch { query } => BodyDto::Detail {
                text: query.clone(),
            },
            ItemBody::Compaction => return None,
        })
    }

    /// A changed item's body, if the subscriber holds it: deltas become
    /// O(delta) ops; whole-item replacements rebuild only that body.
    fn refresh_body(&mut self, id: &str) {
        let Some(&at) = self.index.get(id) else {
            return;
        };
        let key = self.rows[at].key.clone();
        match self.sent.get(&key) {
            None | Some(true) => return,
            Some(false) => {}
        }
        let (start, end) = self.window();
        if !(start..end).contains(&at) {
            // Kept, but rebuilt only when it is near the viewport again.
            self.sent.insert(key, true);
            return;
        }
        let replaced = self.fold.replaced.contains(id);
        let Some(item) = self.fold.item(id) else {
            return;
        };
        match &item.body {
            ItemBody::AgentMessage { text } if self.streams.contains_key(&key) => {
                let stream = self.streams.get_mut(&key).expect("checked");
                let before = stream.blocks().len();
                let splice = stream.update(text);
                // Unchanged text is a no-op; a shrink is a truncating splice.
                if !(splice.blocks.is_empty() && splice.from == splice.len && splice.len == before)
                {
                    self.pending
                        .splice(&key, splice.from, splice.blocks, splice.len);
                }
            }
            ItemBody::Reasoning { .. } | ItemBody::Command { .. } if !replaced => {
                if let Some(text) = self.fold.deltas.get(id) {
                    let text = text.clone();
                    self.pending.append(&key, &text);
                }
            }
            _ => {
                if let Some(body) = self.build_body(&key) {
                    self.pending.set(&key, body);
                }
            }
        }
    }

    /// Copy and search the source transcript, including bodies outside the viewport.
    pub fn read_text(&self, id: Option<&str>) -> String {
        let items = &self.fold.transcript.items;
        if let Some(id) = id {
            if let Some(item) = self.fold.item(id) { return item_text(item); }
            fn collect(row: &Row, ids: &mut HashSet<String>) {
                ids.insert(row.id.clone());
                for child in row.children.iter().chain(&row.comments) { collect(child, ids); }
            }
            let mut ids = HashSet::new();
            if let Some(row) = find(&self.visible, id) { collect(row, &mut ids); }
            return items.iter().filter(|item| ids.contains(&item.id)).map(item_text).filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n\n");
        }
        items.iter().map(|item| {
            let text = item_text(item);
            if text.is_empty() { String::new() } else { format!("## {}\n\n{}", item_label(item), text) }
        }).filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n\n")
    }

    pub fn navigation(&self) -> Vec<super::dto::SearchHitDto> {
        self.fold.transcript.items.iter().filter(|item| matches!(item.body, ItemBody::UserMessage { .. }))
            .enumerate().map(|(index, item)| super::dto::SearchHitDto {
                id: item.id.clone(), label: format!("Message {}", index + 1),
                preview: item_text(item).chars().take(140).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" "),
            }).collect()
    }

    pub fn search(&mut self, query: &str) -> Vec<super::dto::SearchHitDto> {
        self.search_index.search(&self.fold.transcript.items, query)
    }

    pub(super) fn search_while(&mut self, query: &str, current: impl FnMut() -> bool) -> Option<Vec<super::dto::SearchHitDto>> {
        self.search_index.search_while(&self.fold.transcript.items, query, current)
    }

    pub fn reveal_item(&mut self, id: &str) -> Option<u32> {
        fn path(rows: &[Row], id: &str, found: &mut Vec<String>) -> bool {
            for row in rows {
                if row.id == id || path(&row.children, id, found) || path(&row.comments, id, found) {
                    found.push(row.id.clone());
                    return true;
                }
            }
            false
        }
        let mut ancestors = Vec::new();
        if !path(&self.visible, id, &mut ancestors) {
            self.set_mode(DisplayMode::Verbose);
            path(&self.visible, id, &mut ancestors);
        }
        for ancestor in ancestors { self.disclosure.overrides.insert(ancestor, true); }
        self.reproject(&HashSet::new());
        let at = *self.index.get(id)?;
        self.set_viewport(at, at.saturating_add(12));
        Some(at as u32)
    }

    fn panel_dto(&mut self) -> ChatPanelDto {
        let billing = self.billing.snapshot();
        let transcript = &self.fold.transcript;
        let info = transcript.info.as_ref();
        let provider = info.map(|info| info.provider);
        let (state, message) = state_name(&transcript.state);
        let running = matches!(transcript.state, ChatState::Running | ChatState::Waiting);
        let empty = transcript.items.is_empty();
        let status = match (self.link, &transcript.state) {
            (Link::Reconnecting, _) => Some("Reconnecting…"),
            (Link::Connecting, _) if empty => Some("Connecting…"),
            (_, ChatState::Starting) => Some("Starting…"),
            (_, ChatState::Running) => Some("Working…"),
            (_, ChatState::Waiting) => Some("Waiting for you"),
            _ => None,
        };
        ChatPanelDto {
            chat_id: self.chat_id.clone(),
            title: title(transcript),
            provider: provider.map(|provider| wire_name(&provider)),
            cwd: info.map(|info| info.cwd.to_string_lossy().into_owned()),
            project_id: info.and_then(|info| info.project_id.clone().or_else(|| {
                info.orchestrator.as_ref().and_then(|scope| scope.project_id().map(str::to_owned))
            })),
            worktree_id: info.and_then(|info| info.worktree_id.clone()),
            state: state.into(),
            state_message: message,
            link: self.link.name().into(),
            running,
            empty,
            status: status.map(str::to_owned),
            approvals: transcript.approvals.iter().map(approval_dto).collect(),
            questions: transcript.questions.iter().map(question_dto).collect(),
            toolbar: info.map(|info| toolbar_dto(transcript, info)),
            notices: if self.mode == DisplayMode::Normal {
                banners(transcript, self.notices.iter().rev().filter_map(|&at| transcript.items.get(at)))
            } else {
                Vec::new()
            },
            billing,
            file_changes: self.file_counts.total(),
            workers: self.worker_rows.clone(),
            mode: wire_name(&self.mode),
            failures: self.failures.clone(),
            session_id: info.and_then(|info| info.provider_thread_id.clone()),
            usage_label: transcript.usage.as_ref().map(|usage| {
                match (usage.context_used, usage.context_window.filter(|window| *window > 0)) {
                    (Some(used), Some(window)) => format!("{}% · {} / {}", ((used as f64 / window as f64).clamp(0.0, 1.0) * 100.0).round() as u32, toolbar::tokens(used), toolbar::tokens(window)),
                    _ => format!("{} in · {} out", toolbar::tokens(usage.input_tokens), toolbar::tokens(usage.output_tokens)),
                }
            }),
            usage_detail: transcript.usage.as_ref().map(|usage| {
                let mut text = format!("{} input · {} output · {} cached", toolbar::tokens(usage.input_tokens), toolbar::tokens(usage.output_tokens), toolbar::tokens(usage.cached_input_tokens));
                if let Some(cost) = usage.cost_usd { text.push_str(&format!(" · estimated ${cost:.3}")); }
                text
            }),
            origin: info.and_then(|info| info.origin.as_ref()).and_then(origin_dto),
        }
    }
}

/// The first message of a chat made from a handoff document: what `handoff` sends first,
/// the instructions and the document. A chat forked natively, or made otherwise, has none.
fn handoff_message(transcript: &Transcript) -> Option<String> {
    let origin = transcript.info.as_ref()?.origin.as_ref()?;
    if origin.document.is_none() || origin.native.is_some() {
        return None;
    }
    transcript
        .items
        .iter()
        .find(|item| matches!(item.body, ItemBody::UserMessage { .. }))
        .map(|item| item.id.clone())
}

/// What a chat's header says of where it came from.
fn origin_dto(origin: &ChatOrigin) -> Option<OriginDto> {
    let kind = match origin.kind {
        LineageKind::Handoff => "handoff",
        LineageKind::Fork => "fork",
        LineageKind::Unknown => return None,
    };
    let context = match origin.context {
        LineageContext::Transcript => "transcript",
        LineageContext::Summary => "summary",
        LineageContext::Native => "native",
        LineageContext::Unknown => "transcript",
    };
    let title = lineage::quoted(&origin.source_title);
    let shell = origin.source_kind == SourceKind::Shell;
    let agent = lineage::agent_label(origin.provider.as_deref(), origin.model.as_deref());
    let label = match (origin.kind, shell, agent) {
        (LineageKind::Fork, ..) if origin.context == LineageContext::Native => {
            format!("Forked from {title}")
        }
        (LineageKind::Fork, _, Some(agent)) => format!("Forked from {title} · {agent} · {context}"),
        (LineageKind::Fork, _, None) => format!("Forked from {title} · {context}"),
        (_, true, _) => format!("Continued from {} · {context}", origin.source_title.trim()),
        (_, false, Some(agent)) => format!("Continued from {agent} — {title} · {context}"),
        (_, false, None) => format!("Continued from {title} · {context}"),
    };
    Some(OriginDto {
        kind: kind.into(),
        source_id: origin.source_id.clone(),
        source_kind: if shell { "shell" } else { "chat" }.into(),
        source_title: origin.source_title.clone(),
        label,
        context: context.into(),
        document: origin
            .document
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
    })
}

pub(super) fn item_label(item: &Item) -> &'static str {
    match &item.body {
        ItemBody::UserMessage { .. } => "You",
        ItemBody::AgentMessage { .. } => "Assistant",
        ItemBody::Reasoning { .. } => "Reasoning",
        ItemBody::Command { .. } => "Command",
        ItemBody::FileChange { .. } => "File changes",
        ItemBody::Plan { .. } | ItemBody::Todo { .. } => "Plan",
        ItemBody::Notice { .. } => "Notice",
        ItemBody::WebSearch { .. } => "Search",
        ItemBody::ToolCall { .. } => "Tool",
        ItemBody::Compaction => "Context compacted",
    }
}

// A small ordinary input stays on the canonical serializer path. The bounded
// walk avoids visiting an entire large tree just to choose its preview path.
fn costly_json(value: &Value, remaining: &mut usize) -> bool {
    if *remaining == 0 {
        return true;
    }
    *remaining -= 1;
    match value {
        Value::String(text) => text.len() > 1024,
        Value::Array(values) => values.iter().any(|value| costly_json(value, remaining)),
        Value::Object(values) => values
            .iter()
            .any(|(key, value)| key.len() > 1024 || costly_json(value, remaining)),
        _ => false,
    }
}

struct Excerpt {
    skip: usize,
    left: usize,
    text: String,
    space: bool,
}

impl Excerpt {
    fn new(skip: usize, left: usize) -> Self {
        Self {
            skip,
            left,
            text: String::new(),
            space: false,
        }
    }
    fn done(&self) -> bool {
        self.left == 0
    }

    fn push(&mut self, mut text: &str) {
        if self.done() {
            return;
        }
        if self.skip != 0 {
            if text.len() <= self.skip {
                self.skip -= text.chars().count();
                return;
            }
            if self.skip >= 256 && text.as_bytes()[..self.skip].is_ascii() {
                text = &text[self.skip..];
                self.skip = 0;
            } else {
                // Chars::nth already uses optimized UTF-8 skipping. Counting
                // small chunks first regresses optimized builds on Unicode.
                if self.skip != 0 {
                    let mut characters = text.chars();
                    if characters.nth(self.skip - 1).is_none() {
                        self.skip -= text.chars().count();
                        return;
                    }
                    text = characters.as_str();
                    self.skip = 0;
                }
            }
        }
        for character in text.chars().take(self.left) {
            self.left -= 1;
            if character.is_whitespace() {
                self.space = !self.text.is_empty();
            } else {
                if self.space {
                    self.text.push(' ');
                }
                self.text.push(character);
                self.space = false;
            }
        }
    }
}

impl std::io::Write for Excerpt {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.done() {
            return Err(std::io::Error::other("excerpt complete"));
        }
        // Our formatter writes ASCII punctuation/numbers and whole UTF-8 string
        // chunks. Validate that invariant; an unexpected error uses the canonical
        // full serializer, never a partially decoded preview.
        let text = std::str::from_utf8(bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        self.push(text);
        if self.done() {
            Err(std::io::Error::other("excerpt complete"))
        } else {
            Ok(bytes.len())
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Drive serde's pretty formatter while limiting individual string escaping to
/// UTF-8 chunks. serde's normal serializer scans a whole string before writing
/// its first fragment, even if the writer only wants its first 200 characters.
pub(super) fn write_json_prefix<W: std::io::Write>(
    value: &Value,
    formatter: &mut serde_json::ser::PrettyFormatter<'_>,
    writer: &mut W,
) -> std::io::Result<()> {
    use serde_json::ser::Formatter;
    fn string<W: std::io::Write>(
        mut text: &str,
        formatter: &mut serde_json::ser::PrettyFormatter<'_>,
        writer: &mut W,
    ) -> std::io::Result<()> {
        formatter.begin_string(writer)?;
        while !text.is_empty() {
            let end = char_floor(text, 256);
            // Escaping remains serde's job; no hand-maintained JSON escape table.
            let escaped = serde_json::to_string(&text[..end]).map_err(std::io::Error::other)?;
            writer.write_all(escaped[1..escaped.len() - 1].as_bytes())?;
            text = &text[end..];
        }
        formatter.end_string(writer)
    }
    match value {
        Value::String(text) => string(text, formatter, writer),
        Value::Array(values) => {
            formatter.begin_array(writer)?;
            for (index, value) in values.iter().enumerate() {
                formatter.begin_array_value(writer, index == 0)?;
                write_json_prefix(value, formatter, writer)?;
                formatter.end_array_value(writer)?;
            }
            formatter.end_array(writer)
        }
        Value::Object(values) => {
            formatter.begin_object(writer)?;
            for (index, (key, value)) in values.iter().enumerate() {
                formatter.begin_object_key(writer, index == 0)?;
                string(key, formatter, writer)?;
                formatter.end_object_key(writer)?;
                formatter.begin_object_value(writer)?;
                write_json_prefix(value, formatter, writer)?;
                formatter.end_object_value(writer)?;
            }
            formatter.end_object(writer)
        }
        _ => serde_json::to_writer(writer, value).map_err(std::io::Error::other),
    }
}

/// Source text kept as borrowed payloads where formatting only adds separators.
/// Search can normalize once and extract a bounded preview without copying a
/// complete command/tool output again on every cache hit.
pub(super) struct ItemText<'a> {
    first: std::borrow::Cow<'a, str>,
    rest: Option<[&'a str; 3]>,
    // When present, first is empty until prepared; rest follows the JSON.
    json: Option<&'a Value>,
}

impl<'a> ItemText<'a> {
    fn single(text: impl Into<std::borrow::Cow<'a, str>>) -> Self {
        Self {
            first: text.into(),
            rest: None,
            json: None,
        }
    }

    #[cfg(test)]
    fn parts(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.first.as_ref()).chain(self.rest.iter().flatten().copied())
    }

    fn joined(&self) -> String {
        if self.json.is_some() {
            return self.materialized().joined();
        }
        let mut text = String::with_capacity(
            self.first.len()
                + self
                    .rest
                    .iter()
                    .flatten()
                    .map(|part| part.len())
                    .sum::<usize>(),
        );
        text.push_str(&self.first);
        for part in self.rest.iter().flatten() {
            text.push_str(part);
        }
        text
    }

    pub(super) fn into_owned(self) -> String {
        if self.rest.is_none() {
            self.first.into_owned()
        } else {
            self.joined()
        }
    }

    pub(super) fn lowercase(&self) -> String {
        if self.rest.is_none() {
            return self.first.to_lowercase();
        }
        // Keep the whole logical string for Unicode's contextual sigma and
        // expanding mappings. std's lowercase also has an efficient ASCII path.
        self.joined().to_lowercase()
    }

    pub(super) fn excerpt(&self, offset: usize, limit: usize) -> String {
        let mut excerpt = Excerpt::new(offset, limit);
        if let Some(input) = self.json {
            // Far into the input, normal serialization is cheaper than escaping
            // many small chunks. Cold searches already prepare this text once.
            if offset > 8192 {
                return self.materialized().excerpt(offset, limit);
            }
            if limit != 0 {
                let result = write_json_prefix(
                    input,
                    &mut serde_json::ser::PrettyFormatter::new(),
                    &mut excerpt,
                );
                if result.is_err() && !excerpt.done() {
                    // Only reaching the requested window is an expected error.
                    return self.materialized().excerpt(offset, limit);
                }
            }
        } else {
            excerpt.push(&self.first);
        }
        for part in self.rest.iter().flatten() {
            excerpt.push(part);
        }
        excerpt.text
    }

    /// Cold search/copy use the canonical serializer once; warm previews can
    /// leave a large tool input lazy and stop at the requested character window.
    pub(super) fn prepared(mut self) -> Self {
        if let Some(input) = self.json.take() {
            self.first = serde_json::to_string_pretty(input)
                .unwrap_or_default()
                .into();
        }
        self
    }

    fn materialized(&self) -> Self {
        Self {
            first: serde_json::to_string_pretty(self.json.unwrap())
                .unwrap_or_default()
                .into(),
            rest: self.rest,
            json: None,
        }
    }

    #[cfg(test)]
    pub(super) fn excerpt_before(&self, mut offset: usize, mut limit: usize) -> String {
        let mut preview = String::new();
        let mut space = false;
        for part in self.parts() {
            if limit == 0 {
                break;
            }
            let mut characters = part.chars();
            if offset != 0 {
                // Delegate long skips to str's character iterator; flattening
                // segments first would walk every skipped character in Rust.
                if part.len() <= offset || characters.nth(offset - 1).is_none() {
                    offset -= part.chars().count();
                    continue;
                }
                offset = 0;
            }
            for character in characters.take(limit) {
                limit -= 1;
                if character.is_whitespace() {
                    space = !preview.is_empty();
                } else {
                    if space {
                        preview.push(' ');
                    }
                    preview.push(character);
                    space = false;
                }
            }
        }
        preview
    }
}

pub(super) fn item_text(item: &Item) -> String {
    item_source(item).into_owned()
}

pub(super) fn item_source(item: &Item) -> ItemText<'_> {
    if let Some(mark) = lineage::of(item) {
        return ItemText::single(mark.label());
    }
    match &item.body {
        ItemBody::UserMessage { text } => ItemText::single(typed_text_source(text)),
        ItemBody::AgentMessage { text }
        | ItemBody::Reasoning { text }
        | ItemBody::Notice { text, .. }
        | ItemBody::WebSearch { query: text } => ItemText::single(text.as_str()),
        ItemBody::Command {
            command, output, ..
        } => ItemText {
            first: "$ ".into(),
            rest: Some([command, "\n\n", output]),
            json: None,
        },
        ItemBody::FileChange { changes } => ItemText::single(
            changes
                .iter()
                .map(|change| {
                    format!(
                        "{} {}\n{}",
                        wire_name(&change.kind),
                        change.path,
                        change.diff.as_deref().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        ItemBody::Plan { explanation, steps } => ItemText::single(format!(
            "{}\n{}",
            explanation.as_deref().unwrap_or_default(),
            steps
                .iter()
                .map(|step| format!("[{}] {}", wire_name(&step.status), step.text))
                .collect::<Vec<_>>()
                .join("\n")
        )),
        ItemBody::Todo { items } => ItemText::single(
            items
                .iter()
                .map(|step| format!("[{}] {}", wire_name(&step.status), step.text))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        ItemBody::ToolCall { input, output, .. } => {
            let lazy = costly_json(input, &mut 64);
            ItemText {
                first: if lazy {
                    "".into()
                } else {
                    serde_json::to_string_pretty(input)
                        .unwrap_or_default()
                        .into()
                },
                rest: Some(["\n\n", output.as_deref().unwrap_or_default(), ""]),
                json: lazy.then_some(input),
            }
        }
        ItemBody::Compaction => ItemText::single(""),
    }
}

pub fn find<'a>(rows: &'a [Row], id: &str) -> Option<&'a Row> {
    rows.iter().find_map(|row| {
        if row.id == id {
            Some(row)
        } else {
            find(&row.children, id).or_else(|| find(&row.comments, id))
        }
    })
}

pub fn is_disclosure(kind: RowKind) -> bool {
    !matches!(
        kind,
        RowKind::User
            | RowKind::Final
            | RowKind::Commentary
            | RowKind::Compaction
            | RowKind::Notice
            | RowKind::Request
            | RowKind::Lineage
    )
}

fn has_body(row: &Row, item: Option<&Item>) -> bool {
    let Some(item) = item else {
        return false;
    };
    match row.kind {
        RowKind::User
        | RowKind::Final
        | RowKind::Commentary
        | RowKind::Reasoning
        | RowKind::Notice => true,
        RowKind::Compaction | RowKind::Request | RowKind::Group | RowKind::Activity => false,
        RowKind::Worker | RowKind::Subagent | RowKind::Lineage => false,
        _ => row.open && !matches!(item.body, ItemBody::Compaction),
    }
}

/// A header is always one line (CHATT-052).
pub fn first_line(text: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("").trim_end();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

fn duration_label(ms: u64) -> String {
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

fn meta(row: &Row, item: &Item) -> Option<String> {
    let mut parts = Vec::new();
    if row.provisional {
        parts.push("Partial turn".to_owned());
    }
    if let ItemBody::Command {
        exit_code: Some(code),
        ..
    } = item.body
        && code != 0
    {
        parts.push(format!("exit {code}"));
    }
    if row.kind != RowKind::Activity
        && !row.running
        && let Some(ms) = row.duration_ms
    {
        parts.push(duration_label(ms));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn step(step: &riwork_engine::chat::model::Step) -> StepDto {
    StepDto {
        text: step.text.clone(),
        status: match step.status {
            StepStatus::Pending => "pending",
            StepStatus::InProgress => "in_progress",
            StepStatus::Completed => "completed",
        }
        .into(),
    }
}

fn level_name(level: NoticeLevel) -> &'static str {
    match level {
        NoticeLevel::Info => "info",
        NoticeLevel::Warning => "warning",
        NoticeLevel::Error => "error",
    }
}

/// The typed text of a user message, without the attachment lines the host
/// adds (`[Attached: <name> (<bytes> bytes)]`, CHATT-053).
pub fn typed_text(text: &str) -> String {
    typed_text_source(text).into_owned()
}

fn typed_text_source(text: &str) -> std::borrow::Cow<'_, str> {
    fn attachment(line: &str) -> bool {
        let line = line.trim();
        line.starts_with("[Attached: ") && line.ends_with(" bytes)]")
    }
    // Without removed lines or carriage returns, lines().join("\n").trim()
    // is exactly the original trimmed slice (including a final line ending).
    if !text.contains('\r') && !text.lines().any(attachment) {
        return std::borrow::Cow::Borrowed(text.trim());
    }
    text.lines()
        .filter(|line| !attachment(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
        .into()
}

fn char_floor(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The last `limit` bytes of `text` on a char boundary, and how many came before.
fn tail(text: &str, limit: usize) -> (&str, usize) {
    if text.len() <= limit {
        return (text, 0);
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (&text[start..], start)
}

fn cut(text: &str, limit: usize) -> &str {
    &text[..char_floor(text, limit)]
}

pub fn state_name(state: &ChatState) -> (&'static str, Option<String>) {
    match state {
        ChatState::Starting => ("starting", None),
        ChatState::Idle => ("idle", None),
        ChatState::Running => ("running", None),
        ChatState::Waiting => ("waiting", None),
        ChatState::Stopped => ("stopped", None),
        ChatState::Failed { message } => ("failed", Some(message.clone())),
    }
}

pub fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
        Provider::Muse => "Muse",
        Provider::Acp => "ACP",
    }
}

/// The host title, else "<Provider> chat", else "Chat" (CHATT-016).
pub fn title(transcript: &Transcript) -> String {
    match &transcript.info {
        None => "Chat".to_owned(),
        Some(info) if info.title.trim().is_empty() => {
            format!("{} chat", provider_name(info.provider))
        }
        Some(info) => info.title.clone(),
    }
}

pub fn decision_label(decision: Decision) -> &'static str {
    match decision {
        Decision::Accept => "Allow",
        Decision::AcceptForSession => "Allow for session",
        Decision::Decline => "Deny",
        Decision::Cancel => "Stop",
    }
}

pub fn kind_label(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::Command => "Run a command?",
        ApprovalKind::FileChange => "Change files?",
        ApprovalKind::Permissions => "Grant more access?",
        ApprovalKind::Tool => "Use a tool?",
    }
}

fn approval_dto(approval: &Approval) -> ApprovalDto {
    ApprovalDto {
        request_id: approval.request_id.clone(),
        kind: wire_name(&approval.kind),
        heading: kind_label(approval.kind).into(),
        title: approval.title.clone(),
        detail: approval.detail.clone(),
        choices: approval
            .choices
            .iter()
            .map(|decision| ChoiceDto {
                decision: decision_name(*decision).into(),
                label: decision_label(*decision).into(),
                // There is never a hint on "Allow for session" (CHATT-205/CHATC-025).
                key: match decision {
                    Decision::Accept => Some("⏎".into()),
                    Decision::Decline => Some("⎋".into()),
                    Decision::Cancel if !approval.choices.contains(&Decision::Decline) => {
                        Some("⎋".into())
                    }
                    _ => None,
                },
            })
            .collect(),
    }
}

fn question_dto(question: &Question) -> QuestionDto {
    QuestionDto {
        request_id: question.request_id.clone(),
        prompts: question
            .questions
            .iter()
            .map(|prompt| PromptDto {
                header: prompt.header.clone(),
                question: prompt.question.clone(),
                options: prompt
                    .options
                    .iter()
                    .map(|option| OptionDto {
                        label: option.label.clone(),
                        description: option.description.clone(),
                    })
                    .collect(),
                multi_select: prompt.multi_select,
            })
            .collect(),
    }
}

fn pick(id: String, label: String, detail: Option<String>, current: bool) -> PickRowDto {
    PickRowDto {
        id,
        label,
        detail,
        current,
    }
}

fn toolbar_dto(transcript: &Transcript, info: &riwork_engine::chat::model::ChatInfo) -> ToolbarDto {
    let models = &transcript.models;
    let chosen = info.model.as_deref();
    let provider = info.provider;
    ToolbarDto {
        provider: wire_name(&provider),
        provider_label: provider_name(provider).into(),
        model_label: toolbar::model_label(models, chosen),
        model: info.model.clone(),
        models: toolbar::model_rows(models, chosen)
            .into_iter()
            .map(|row| pick(row.id, row.label, row.detail, row.current))
            .collect(),
        model_suggestions: toolbar::model_suggestions(provider)
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        effort_label: toolbar::effort_label(models, chosen, info.effort.as_deref(), provider),
        effort: info.effort.clone(),
        efforts: toolbar::effort_rows(models, chosen, info.effort.as_deref(), provider)
            .into_iter()
            .map(|row| pick(row.effort, row.label, None, row.current))
            .collect(),
        effort_available: toolbar::effort_available(models, chosen, provider),
        fast_available: toolbar::fast_available(models, chosen),
        fast: info.fast,
        approval_mode: wire_name(&info.approval_mode),
        mode_label: toolbar::mode_label(info.approval_mode).into(),
        modes: toolbar::MODES
            .iter()
            .filter(|(mode, _, _)| info.provider != Provider::Acp || *mode != ApprovalMode::Plan)
            .map(|(mode, label, description)| {
                pick(
                    wire_name(mode),
                    (*label).into(),
                    Some((*description).into()),
                    *mode == info.approval_mode,
                )
            })
            .collect(),
        contributor_mode: (provider == Provider::Muse).then(|| info.reported_contributor_mode().map(|mode| wire_name(&mode))).flatten(),
        contributor_label: (provider == Provider::Muse).then(|| info.reported_contributor_mode().map(|mode| toolbar::contributor_label(!mode.is_off()).to_owned()).unwrap_or_else(|| toolbar::CONTRIBUTOR_SETTINGS_LABEL.to_owned())),
        providers: [Provider::Codex, Provider::Claude, Provider::Muse]
            .into_iter()
            .filter(|other| *other != provider)
            .map(|other| {
                pick(
                    wire_name(&other),
                    format!("{} default", provider_name(other)),
                    None,
                    false,
                )
            })
            .collect(),
    }
}

/// Open notices as banners, newest per kind (or text), newest first, at most
/// three. The caller supplies the notice index, not the entire transcript.
fn banners<'a>(transcript: &Transcript, notices: impl Iterator<Item = &'a Item>) -> Vec<NoticeDto> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for item in notices {
        if let ItemBody::Notice {
            level,
            text,
            kind,
            resolved,
            dismissed,
            ..
        } = &item.body
        {
            if *resolved || *dismissed || super::billing::hide_notice(transcript, &item.body) {
                continue;
            }
            let key = kind.clone().unwrap_or_else(|| text.clone());
            if seen.insert(key) {
                result.push(NoticeDto {
                    id: item.id.clone(),
                    level: level_name(*level).into(),
                    text: text.clone(),
                });
            }
            if result.len() == 3 {
                break;
            }
        }
    }
    result
}
