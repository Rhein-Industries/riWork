//! Transcript grouping specification v2 (`spec/transcript-grouping/v2/*.json`).
//!
//! A run contains Read/Search/List/WebSearch/WebFetch/Mcp/Other, Reasoning and
//! exploratory shell calls; failure and streaming never split it. Shell membership
//! requires the conservative read-only parser and no approval for that item.
//! Commentary travels beside a run without contributing to its counts. Edits,
//! requests, final/user text, subagents, plans/todos, compaction and turn boundaries
//! split runs. A singleton stays a call; groups use `g:<turn>:<first member>`.
//! Buckets have the mockups' fixed order, at most three plus `+N more`.
//!
//! Normal emits one `activity:<turn>` between the user and answer. It counts
//! unique item IDs (including known descendants once), unique edited paths, then
//! commands, reads and MCP/other calls, at most four nonzero buckets. Requests and
//! produced artifacts and worker/subagent cards stay visible outside its disclosure.
//! Cards precede the answer; Verbose keeps them in place. Commentary stays hidden.
//! Timing is supplied, or falls back to known item durations; absent is not zero.
//! Every disclosure starts closed. Mode/bulk resets replace manual overrides;
//! closing an activity preserves its children's manual overrides. Anchor IDs name
//! items, and are resolved after projection to the visible containing row.
//!
//! Pages extend to turn boundaries unless a hard item cap requires a provisional
//! partial turn. Reprojection resolves the same item anchor to its new group ID.
//! Providers' output is deliberately absent here: collapsed rows have no preview.

use super::model::{
    Item, ItemBody, ItemStatus, MessagePhase, PathRef, Provider, RequestKind, RequestRecord,
    ToolKind, Transcript,
};
use super::semantics::{classify_body, is_read_only_shell};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

pub const SPEC_VERSION: u8 = 2;

#[cfg(test)]
mod upstream_grouping_tests;
#[cfg(test)]
mod upstream_presentation_tests;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    #[default]
    Normal,
    Verbose,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    User,
    Final,
    Commentary,
    Reasoning,
    Shell,
    Read,
    Search,
    List,
    WebSearch,
    WebFetch,
    Mcp,
    Worker,
    Subagent,
    #[default]
    Other,
    Edit,
    Plan,
    Todo,
    Compaction,
    Notice,
    Request,
    Group,
    Activity,
    /// Where the conversation went on: a handoff or fork marker (`lineage`). Shown right
    /// after the item it was cut at, never inside an activity or a group.
    Lineage,
}

/// Reference to an artifact on its source item; never a guess from output text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtifactRef {
    pub item_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_index: Option<usize>,
}

/// A render-independent header. Look up a leaf's `id` in the original items for
/// its body, or `request_id` in request history. Children remain present while
/// closed, allowing clients to preserve manual state without rebuilding bodies.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Row {
    pub id: String,
    pub kind: RowKind,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub open: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Row>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<Row>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,
    pub failed_count: usize,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub provisional: bool,
    /// IDs with response-only relay elision metadata. Native bodies stay intact
    /// in storage; projection counts these placeholders without showing a final.
    pub elided: bool,
}

/// UI-owned state; these pure transitions implement the shared reset rules.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DisclosureState {
    pub overrides: HashMap<String, bool>,
    pub bulk: Option<bool>,
}

impl DisclosureState {
    pub fn toggle(&mut self, id: &str, currently_open: bool) {
        self.overrides.insert(id.into(), !currently_open);
    }

    pub fn reset_mode(&mut self) {
        self.overrides.clear();
        self.bulk = None;
    }

    /// Legacy blanket default for serialized fixture/client compatibility.
    /// Interactive commands should use `reset_bulk_for_rows` so later arrivals
    /// do not inherit an earlier Expand all command.
    pub fn reset_bulk(&mut self, open: bool) {
        self.overrides.clear();
        self.bulk = Some(open);
    }

    /// Apply Expand/Collapse all to a snapshot of the current rows. Unlike the
    /// legacy blanket `bulk` state, this never opens later streaming arrivals.
    /// Normal affects activity lines only; Verbose affects all call headers,
    /// including children currently hidden under a closed group or worker.
    pub fn reset_bulk_for_rows(&mut self, open: bool, mode: DisplayMode, rows: &[Row]) {
        fn visit(state: &mut DisclosureState, rows: &[Row], mode: DisplayMode, open: bool) {
            for row in rows {
                let affected = match mode {
                    DisplayMode::Normal => row.kind == RowKind::Activity,
                    DisplayMode::Verbose => !matches!(
                        row.kind,
                        RowKind::User
                            | RowKind::Final
                            | RowKind::Commentary
                            | RowKind::Compaction
                            | RowKind::Notice
                            | RowKind::Request
                    ),
                };
                if affected {
                    state.overrides.insert(row.id.clone(), open);
                }
                visit(state, &row.children, mode, open);
                visit(state, &row.comments, mode, open);
            }
        }
        self.reset_mode();
        visit(self, rows, mode, open);
    }

    fn is_open(&self, id: &str, mode: DisplayMode, activity: bool) -> bool {
        self.overrides.get(id).copied().unwrap_or_else(|| {
            if activity || mode == DisplayMode::Verbose {
                self.bulk.unwrap_or(false)
            } else {
                false
            }
        })
    }
}

pub use super::model::TurnTiming;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectionOptions {
    pub mode: DisplayMode,
    /// Resolve exact legacy delegation tools when presentation metadata is absent.
    pub provider: Option<Provider>,
    pub now_ms: Option<u64>,
    pub running_turn_id: Option<String>,
    pub turn_timing: HashMap<String, TurnTiming>,
    pub disclosure: DisclosureState,
    /// A caller that has only part of an oversized turn sets this on that page.
    pub provisional: bool,
    /// Populate from snapshot/item-elided response metadata; not durable hints.
    pub elided_items: HashSet<String>,
}

/// Per-turn projection rows, without retained copies of Item bodies or outputs.
/// Callers invalidate turns whose item headers/structure changed. Identity,
/// order, request and projection-option changes are additionally checked here.
#[derive(Default)]
pub struct ProjectionCache {
    // Keep both modes: switching presentation must not discard the other mode's
    // historical headers or revisit large source outputs and inline images.
    turns: HashMap<(String, DisplayMode), CachedTurn>,
    #[cfg(test)]
    projected_turns: usize,
}

struct CachedTurn {
    turn_id: String,
    identity: Vec<(String, Option<u64>, bool)>,
    requests: Vec<RequestRecord>,
    mode: DisplayMode,
    provider: Option<Provider>,
    provisional: bool,
    turn_clock: Option<(Option<u64>, Option<u64>)>,
    running_turn: bool,
    now_ms: Option<u64>,
    live: bool,
    rows: Vec<Row>,
}

impl ProjectionCache {
    pub fn clear(&mut self) {
        self.turns.clear();
    }

    /// Use the same grouping and global request-assignment path as `project`.
    /// Unchanged historical turns reuse both modes' rows; only dirty turns
    /// revisit item headers. Invalidate all relevant turn IDs for header/status changes,
    /// or clear the cache when a caller cannot identify their owning turn.
    pub fn project(
        &mut self,
        transcript: &Transcript,
        options: &ProjectionOptions,
        invalidated_turns: &HashSet<String>,
    ) -> Vec<Row> {
        // A dirty turn invalidates both presentation variants, including the
        // variant that is currently hidden. Never revive stale status on switch.
        self.turns
            .retain(|_, cached| !invalidated_turns.contains(&cached.turn_id));
        // Transcript's event fold guarantees unique identities. Preserve the
        // public projection's deduplication contract for synthetic callers too.
        let mut identities = HashSet::new();
        if transcript
            .items
            .iter()
            .any(|item| !identities.insert(item.id.as_str()))
        {
            self.clear();
            return project_transcript(transcript, options);
        }
        let orders = transcript
            .items
            .iter()
            .filter_map(|item| {
                transcript
                    .item_seq(&item.id)
                    .map(|seq| (item.id.clone(), seq))
            })
            .collect();
        let mut options = options.clone();
        options.provider = options
            .provider
            .or_else(|| transcript.info.as_ref().map(|info| info.provider));
        for (turn, timing) in &transcript.turn_timing {
            options.turn_timing.entry(turn.clone()).or_insert(*timing);
        }
        if options.running_turn_id.is_none() {
            options.running_turn_id = transcript.turn_id.clone();
        }
        let items: Vec<_> = transcript.items.iter().map(Cow::Borrowed).collect();
        let rows = project_rows(
            &items,
            &orders,
            &transcript.requests,
            &options,
            Some((&mut *self, invalidated_turns)),
        );
        let block_starts: HashSet<_> = transcript
            .items
            .iter()
            .enumerate()
            .filter(|(at, item)| *at == 0 || !same_turn(&transcript.items[*at - 1], item))
            .map(|(_, item)| item.id.as_str())
            .collect();
        self.turns
            .retain(|(first, _), _| block_starts.contains(first.as_str()));
        rows
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageWindow {
    pub start: usize,
    pub end: usize,
    pub provisional: bool,
}

/// Expand an item-index page to whole turns, then apply an optional hard cap.
/// Unknown turn IDs are separate legacy items, never one invented giant turn.
pub fn turn_aligned_page(
    items: &[Item],
    start: usize,
    end: usize,
    max_items: Option<usize>,
) -> PageWindow {
    let mut start = start.min(items.len());
    let mut end = end.min(items.len()).max(start);
    if start == end {
        return PageWindow {
            start,
            end,
            provisional: false,
        };
    }
    while start > 0 && same_turn(&items[start - 1], &items[start]) {
        start -= 1;
    }
    while end < items.len() && same_turn(&items[end - 1], &items[end]) {
        end += 1;
    }
    if let Some(limit) = max_items {
        start = start.max(end.saturating_sub(limit));
    }
    let provisional = start < end
        && ((start > 0 && same_turn(&items[start - 1], &items[start]))
            || (end < items.len() && same_turn(&items[end - 1], &items[end])));
    PageWindow {
        start,
        end,
        provisional,
    }
}

fn same_turn(a: &Item, b: &Item) -> bool {
    a.turn_id.is_some() && a.turn_id == b.turn_id
}

/// Convenient adapter retaining real event order for request boundaries.
pub fn project_transcript(transcript: &Transcript, options: &ProjectionOptions) -> Vec<Row> {
    let orders = transcript
        .items
        .iter()
        .filter_map(|item| {
            transcript
                .item_seq(&item.id)
                .map(|seq| (item.id.clone(), seq))
        })
        .collect();
    let mut options = options.clone();
    options.provider = options
        .provider
        .or_else(|| transcript.info.as_ref().map(|info| info.provider));
    for (turn, timing) in &transcript.turn_timing {
        options.turn_timing.entry(turn.clone()).or_insert(*timing);
    }
    if options.running_turn_id.is_none() {
        options.running_turn_id = transcript.turn_id.clone();
    }
    project(&transcript.items, &orders, &transcript.requests, &options)
}

/// Advance a projected transcript's live clock without cloning item bodies or
/// rebuilding groups. Only running subtrees can change. Callers must reproject
/// for item/header/status changes; this helper uses the transcript's turn clock
/// rather than caller-specific timing overrides. Returns whether a row changed.
pub fn refresh_timing(rows: &mut [Row], transcript: &Transcript, now_ms: Option<u64>) -> bool {
    fn visit(
        rows: &mut [Row],
        items: &HashMap<&str, &Item>,
        turn_timing: &HashMap<String, TurnTiming>,
        now_ms: Option<u64>,
    ) -> bool {
        let mut changed = false;
        for row in rows {
            // A worker may complete before its captured children finish.
            if !row.running && row.kind != RowKind::Subagent {
                continue;
            }
            changed |= visit(&mut row.children, items, turn_timing, now_ms);
            changed |= visit(&mut row.comments, items, turn_timing, now_ms);
            if !row.running {
                continue;
            }
            let duration = match row.kind {
                RowKind::Activity => {
                    // Inline cards live outside this disclosure in Normal, but
                    // their captured descendant work still contributes timing.
                    let turn = row.id.strip_prefix("activity:");
                    let legacy_item = row.id.strip_prefix("activity:legacy:");
                    let work: Vec<_> = items
                        .values()
                        .copied()
                        .filter(|item| {
                            (item.turn_id.as_deref() == turn
                                || (item.turn_id.is_none()
                                    && legacy_item == Some(item.id.as_str())))
                                && is_work(item)
                        })
                        .collect();
                    activity_duration_at(
                        row.id
                            .strip_prefix("activity:")
                            .and_then(|turn| turn_timing.get(turn)),
                        &work,
                        true,
                        now_ms,
                    )
                }
                RowKind::Group => sum_durations(row.children.iter().map(|row| row.duration_ms)),
                _ => items
                    .get(row.id.as_str())
                    .and_then(|item| item_duration(item, now_ms)),
            };
            if row.duration_ms != duration {
                row.duration_ms = duration;
                changed = true;
            }
            if row.kind == RowKind::Activity {
                let title = activity_title(&[], true, duration, row.current_label.as_deref());
                if row.title != title {
                    row.title = title;
                    changed = true;
                }
            }
        }
        changed
    }
    let items = transcript
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    visit(rows, &items, &transcript.turn_timing, now_ms)
}

/// Shared fixture entry point. `item_sequences` is first arrival order, not the
/// latest update's sequence. Omit it only when no control-event ordering is known.
pub fn project(
    items: &[Item],
    item_sequences: &HashMap<String, u64>,
    requests: &[RequestRecord],
    options: &ProjectionOptions,
) -> Vec<Row> {
    let unique = prepare_items(items.iter(), options);
    project_rows(&unique, item_sequences, requests, options, None)
}

fn prepare_items<'a>(
    items: impl IntoIterator<Item = &'a Item>,
    options: &ProjectionOptions,
) -> Vec<Cow<'a, Item>> {
    // An update is one identity. Preserve first arrival position and latest form.
    let mut unique: Vec<Cow<'a, Item>> = Vec::new();
    let mut index = HashMap::new();
    for item in items {
        // Transcript folds already attach semantics. Borrow those items, along
        // with messages and unclassified tools: projection only needs headers,
        // never owned copies of output strings or base64 image payloads.
        let mut item = Cow::Borrowed(item);
        if options.elided_items.contains(&item.id)
            && matches!(item.body, ItemBody::AgentMessage { .. })
        {
            item.to_mut().body = ItemBody::ToolCall {
                server: None,
                tool: "elided".into(),
                input: serde_json::Value::Null,
                output: None,
            };
        }
        if item.presentation.semantics.is_none()
            && let Some(semantics) =
                classify_body(&item.body).filter(|semantics| semantics.kind != ToolKind::Other)
        {
            item.to_mut().presentation.semantics = Some(semantics);
        }
        if let Some(&at) = index.get(&item.id) {
            unique[at] = item;
        } else {
            index.insert(item.id.clone(), unique.len());
            unique.push(item);
        }
    }
    unique
}

/// Index request ownership once, rather than searching the entire history for
/// every request and then searching every request again for every turn.
#[derive(Default)]
struct RequestIndex<'a> {
    by_turn: HashMap<&'a str, Vec<(usize, &'a RequestRecord)>>,
    by_item: HashMap<&'a str, Vec<(usize, &'a RequestRecord)>>,
    unbound: Vec<(usize, &'a RequestRecord)>,
    unplaced: Vec<&'a RequestRecord>,
}

impl<'a> RequestIndex<'a> {
    fn new(items: &[Cow<'_, Item>], requests: &'a [RequestRecord]) -> Self {
        let mut index = Self::default();
        if requests.is_empty() {
            return index;
        }
        let item_ids: HashSet<_> = items.iter().map(|item| item.id.as_str()).collect();
        let turn_ids: HashSet<_> = items
            .iter()
            .filter_map(|item| item.turn_id.as_deref())
            .collect();
        for (at, record) in requests.iter().enumerate() {
            if !record
                .turn_id
                .as_deref()
                .is_some_and(|id| turn_ids.contains(id))
                && !record
                    .item_id
                    .as_deref()
                    .is_some_and(|id| item_ids.contains(id))
            {
                index.unplaced.push(record);
            }
            // Explicit turn ownership takes precedence over an item reference.
            if let Some(turn) = record.turn_id.as_deref() {
                index.by_turn.entry(turn).or_default().push((at, record));
            } else if let Some(item) = record.item_id.as_deref() {
                index.by_item.entry(item).or_default().push((at, record));
            } else {
                index.unbound.push((at, record));
            }
        }
        index.unplaced.sort_by_key(|record| record.requested_seq);
        index
            .unbound
            .sort_by_key(|(_, record)| record.requested_seq);
        index
    }

    fn for_turn(
        &self,
        turn: &str,
        items: &[Cow<'_, Item>],
        first_seq: u64,
        next_seq: u64,
        seen: &HashSet<String>,
    ) -> Vec<&'a RequestRecord> {
        let mut matches = self.by_turn.get(turn).cloned().unwrap_or_default();
        if !self.by_item.is_empty() {
            for item in items {
                if let Some(requests) = self.by_item.get(item.id.as_str()) {
                    matches.extend_from_slice(requests);
                }
            }
        }
        let start = self
            .unbound
            .partition_point(|(_, record)| record.requested_seq < first_seq);
        matches.extend(
            self.unbound[start..]
                .iter()
                .take_while(|(_, record)| record.requested_seq < next_seq)
                .filter(|(_, record)| !seen.contains(&record.request_id))
                .copied(),
        );
        // Preserve input order for equal event sequences and cache identities,
        // regardless of which ownership index supplied each request.
        matches.sort_unstable_by_key(|(at, _)| *at);
        matches.into_iter().map(|(_, record)| record).collect()
    }
}

fn project_rows(
    unique: &[Cow<'_, Item>],
    item_sequences: &HashMap<String, u64>,
    requests: &[RequestRecord],
    options: &ProjectionOptions,
    mut cache: Option<(&mut ProjectionCache, &HashSet<String>)>,
) -> Vec<Row> {
    // Lineage markers have no turn of their own: each shows right after the item it was cut
    // at, or at the end of the turn written before it, so it never splits a turn, joins a
    // turn's work or leaves its place when the turn goes on.
    let markers = place_markers(unique);
    let conversation;
    let unique: &[Cow<'_, Item>] = if markers.is_empty() {
        unique
    } else {
        conversation = unique
            .iter()
            .filter(|item| !super::lineage::is_marker(item))
            .cloned()
            .collect::<Vec<_>>();
        &conversation
    };
    let mut rows: Vec<Row> = markers
        .iter()
        .filter(|(place, _)| matches!(place, Place::Start))
        .map(|(_, marker)| item_row(marker, options))
        .collect();
    let mut seen_requests = HashSet::new();
    let request_index = RequestIndex::new(unique, requests);
    let mut unplaced = request_index.unplaced.iter().peekable();
    let mut start = 0;
    while start < unique.len() {
        let turn = unique[start]
            .turn_id
            .clone()
            .unwrap_or_else(|| format!("legacy:{}", unique[start].id));
        let mut end = start + 1;
        while end < unique.len() && same_turn(&unique[start], &unique[end]) {
            end += 1;
        }
        let turn_items = &unique[start..end];
        let first_seq = item_sequences
            .get(&unique[start].id)
            .copied()
            .unwrap_or(start as u64 * 2 + 1);
        let next_seq = unique
            .get(end)
            .and_then(|item| item_sequences.get(&item.id))
            .copied()
            .unwrap_or(u64::MAX);
        // Advance through chronological orphan controls only once. Earlier
        // controls stay seen even if a later block has an older event sequence.
        while unplaced
            .peek()
            .is_some_and(|record| record.requested_seq < first_seq)
        {
            let record = unplaced.next().unwrap();
            if seen_requests.insert(record.request_id.clone()) {
                rows.push(request_row(record));
            }
        }
        let turn_requests =
            request_index.for_turn(&turn, turn_items, first_seq, next_seq, &seen_requests);
        for request in &turn_requests {
            seen_requests.insert(request.request_id.clone());
        }
        let clock = options
            .turn_timing
            .get(&turn)
            .map(|timing| (timing.started_at, timing.ended_at));
        let running_turn = options.running_turn_id.as_deref() == Some(&turn);
        let identity = || {
            turn_items.iter().map(|item| {
                (
                    item.id.as_str(),
                    item_sequences.get(&item.id).copied(),
                    options.elided_items.contains(&item.id),
                )
            })
        };
        let reused = cache.as_ref().and_then(|(cache, invalidated)| {
            cache
                .turns
                .get(&(turn_items[0].id.clone(), options.mode))
                .filter(|cached| {
                    !invalidated.contains(&turn)
                        && cached
                            .identity
                            .iter()
                            .map(|(id, seq, elided)| (id.as_str(), *seq, *elided))
                            .eq(identity())
                        && cached.requests.iter().eq(turn_requests.iter().copied())
                        && cached.mode == options.mode
                        && cached.provider == options.provider
                        && cached.provisional == options.provisional
                        && cached.turn_clock == clock
                        && cached.running_turn == running_turn
                        && (!cached.live || cached.now_ms == options.now_ms)
                })
                .map(|cached| cached.rows.clone())
        });
        let mut turn_rows = if let Some(rows) = reused {
            rows
        } else {
            let prepared;
            let turn_items = if cache.is_some() {
                prepared = prepare_items(turn_items.iter().map(Cow::as_ref), options);
                &prepared
            } else {
                turn_items
            };
            let projected =
                project_turn(turn_items, item_sequences, &turn_requests, &turn, options);
            let identity = identity()
                .map(|(id, seq, elided)| (id.to_owned(), seq, elided))
                .collect();
            if let Some((cache, _)) = cache.as_mut() {
                #[cfg(test)]
                {
                    cache.projected_turns += 1;
                }
                cache.turns.insert(
                    (turn_items[0].id.clone(), options.mode),
                    CachedTurn {
                        turn_id: turn.clone(),
                        identity,
                        requests: turn_requests
                            .iter()
                            .map(|record| (**record).clone())
                            .collect(),
                        mode: options.mode,
                        provider: options.provider,
                        provisional: options.provisional,
                        turn_clock: clock,
                        running_turn,
                        now_ms: options.now_ms,
                        live: has_running_rows(&projected),
                        rows: projected.clone(),
                    },
                );
            }
            projected
        };
        // Disclosure changes never require cloning the source item bodies.
        apply_disclosures(&mut turn_rows, options);
        for (place, marker) in &markers {
            match place {
                Place::After(at) if turn_items.iter().any(|item| item.id == *at) => {
                    let mut index = turn_rows
                        .iter()
                        .position(|row| holds(row, at))
                        .map_or(turn_rows.len(), |index| index + 1);
                    while turn_rows
                        .get(index)
                        .is_some_and(|row| row.kind == RowKind::Lineage)
                    {
                        index += 1;
                    }
                    turn_rows.insert(index, item_row(marker, options));
                }
                Place::EndOfTurnOf(id) if turn_items.iter().any(|item| item.id == *id) => {
                    turn_rows.push(item_row(marker, options));
                }
                _ => {}
            }
        }
        rows.extend(turn_rows);
        start = end;
    }
    // A control-only page still has its records; never silently drop one.
    let mut remaining: Vec<_> = requests
        .iter()
        .filter(|record| !seen_requests.contains(&record.request_id))
        .collect();
    remaining.sort_by_key(|record| record.requested_seq);
    rows.extend(remaining.into_iter().map(request_row));
    rows
}

/// Where a lineage marker shows (`project_rows`).
enum Place<'a> {
    /// Before everything: nothing was written before it.
    Start,
    /// Right after the item it was cut at.
    After(&'a str),
    /// At the end of the turn of the item written before it.
    EndOfTurnOf(&'a str),
}

fn place_markers<'a>(items: &'a [Cow<'_, Item>]) -> Vec<(Place<'a>, &'a Item)> {
    if !items.iter().any(|item| super::lineage::is_marker(item)) {
        return Vec::new();
    }
    let said: HashSet<&str> = items
        .iter()
        .filter(|item| !super::lineage::is_marker(item))
        .map(|item| item.id.as_str())
        .collect();
    let mut previous: Option<&str> = None;
    let mut placed = Vec::new();
    for item in items {
        if super::lineage::is_marker(item) {
            let at = super::lineage::of(item)
                .and_then(|mark| mark.at.as_deref())
                .filter(|at| said.contains(at));
            let place = match (at, previous) {
                (Some(at), _) => Place::After(at),
                (None, Some(previous)) => Place::EndOfTurnOf(previous),
                (None, None) => Place::Start,
            };
            placed.push((place, item.as_ref()));
        } else {
            previous = Some(item.id.as_str());
        }
    }
    placed
}

/// Whether `row` is the row of item `id`, or holds it.
fn holds(row: &Row, id: &str) -> bool {
    row.id == id
        || row.children.iter().any(|child| holds(child, id))
        || row.comments.iter().any(|comment| holds(comment, id))
}

fn project_turn(
    turn_items: &[Cow<'_, Item>],
    item_sequences: &HashMap<String, u64>,
    turn_requests: &[&RequestRecord],
    turn: &str,
    options: &ProjectionOptions,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let verbose = verbose_rows(turn_items, item_sequences, turn_requests, turn, options);
    if options.mode == DisplayMode::Verbose {
        rows.extend(verbose);
    } else {
        rows.extend(
            verbose
                .iter()
                .filter(|row| row.kind == RowKind::User)
                .cloned(),
        );
        let work: Vec<_> = turn_items
            .iter()
            .map(Cow::as_ref)
            .filter(|item| is_work(item))
            .collect();
        if turn_items[0].turn_id.is_some()
            || !work.is_empty()
            || options.running_turn_id.as_deref() == Some(turn)
        {
            let id = format!("activity:{turn}");
            let running = options.running_turn_id.as_deref() == Some(turn)
                || work
                    .iter()
                    .any(|item| item.status == ItemStatus::InProgress);
            let current = work
                .iter()
                .rev()
                .find(|item| item.status == ItemStatus::InProgress);
            let duration = activity_duration(turn, &work, running, options);
            let current_label = current.map(|item| short_label(item));
            let title = activity_title(&work, running, duration, current_label.as_deref());
            let artifacts = turn_items
                .iter()
                .filter(|item| !matches!(row_kind(item), RowKind::User | RowKind::Final))
                .flat_map(|item| artifacts(item))
                .collect();
            let mut children: Vec<_> = verbose
                .iter()
                .filter(|row| {
                    !matches!(
                        row.kind,
                        RowKind::User
                            | RowKind::Final
                            | RowKind::Request
                            | RowKind::Compaction
                            | RowKind::Commentary
                            | RowKind::Notice
                            | RowKind::Worker
                            | RowKind::Subagent
                            | RowKind::Lineage
                    )
                })
                .cloned()
                .collect();
            strip_normal_details(&mut children);
            rows.push(Row {
                id: id.clone(),
                kind: RowKind::Activity,
                title,
                open: options.disclosure.is_open(&id, options.mode, true),
                children,
                artifacts,
                failed_count: work.iter().filter(|item| failed(item)).count(),
                running,
                duration_ms: duration,
                current_label,
                provisional: options.provisional,
                ..Row::default()
            });
        }
        // Cards and records remain visible even while activity is closed. Lift
        // nested cards too, removing them from their former parent so each has
        // exactly one presentation path.
        fn outside(rows: Vec<Row>, output: &mut Vec<Row>) {
            for mut row in rows {
                if matches!(
                    row.kind,
                    RowKind::Request
                        | RowKind::Compaction
                        | RowKind::Final
                        | RowKind::Worker
                        | RowKind::Subagent
                        | RowKind::Lineage
                ) {
                    let mut lifted = Vec::new();
                    retain_card_details(&mut row.children, &mut lifted);
                    strip_normal_details(&mut row.children);
                    // Final media stays on its answer, rather than the activity
                    // strip used for tool/card artifacts.
                    if row.kind != RowKind::Final {
                        row.artifacts.clear();
                    }
                    row.comments.clear();
                    output.push(row);
                    output.extend(lifted);
                } else {
                    outside(row.children, output);
                }
            }
        }
        fn retain_card_details(children: &mut Vec<Row>, lifted: &mut Vec<Row>) {
            let mut retained = Vec::new();
            for mut child in children.drain(..) {
                if matches!(
                    child.kind,
                    RowKind::Worker
                        | RowKind::Subagent
                        | RowKind::Request
                        | RowKind::Compaction
                        | RowKind::Final
                        | RowKind::Lineage
                ) {
                    outside(vec![child], lifted);
                } else {
                    retain_card_details(&mut child.children, lifted);
                    retained.push(child);
                }
            }
            *children = retained;
        }
        let mut inline = Vec::new();
        outside(verbose, &mut inline);
        // Host markers may arrive after the answer. They must not hide it or
        // fall below it in Normal's user/activity/cards/answer layout.
        rows.extend(
            inline
                .iter()
                .filter(|row| row.kind != RowKind::Final)
                .cloned(),
        );
        rows.extend(inline.into_iter().filter(|row| row.kind == RowKind::Final));
    }
    rows
}

fn apply_disclosures(rows: &mut [Row], options: &ProjectionOptions) {
    for row in rows {
        if !matches!(
            row.kind,
            RowKind::User
                | RowKind::Final
                | RowKind::Commentary
                | RowKind::Compaction
                | RowKind::Notice
                | RowKind::Request
                | RowKind::Lineage
        ) {
            row.open =
                options
                    .disclosure
                    .is_open(&row.id, options.mode, row.kind == RowKind::Activity);
        }
        apply_disclosures(&mut row.children, options);
        apply_disclosures(&mut row.comments, options);
    }
}

fn has_running_rows(rows: &[Row]) -> bool {
    rows.iter().any(|row| {
        row.running || has_running_rows(&row.children) || has_running_rows(&row.comments)
    })
}

fn strip_normal_details(rows: &mut [Row]) {
    for row in rows {
        row.artifacts.clear();
        row.comments.clear();
        row.children.retain(|child| {
            !matches!(
                child.kind,
                RowKind::Commentary
                    | RowKind::Request
                    | RowKind::Compaction
                    | RowKind::Final
                    | RowKind::Worker
                    | RowKind::Subagent
                    | RowKind::Lineage
            )
        });
        strip_normal_details(&mut row.children);
    }
}

/// Effective nesting after validating the complete raw parent graph. A raw
/// cycle invalidates every item that reaches it, even across non-subagent
/// parents; only the immediate valid subagent owner becomes a displayed edge.
struct ItemNesting<'a> {
    roots: Vec<&'a Item>,
    children: HashMap<&'a str, Vec<&'a Item>>,
    parents: HashMap<&'a str, &'a str>,
}

impl<'a> ItemNesting<'a> {
    fn new(items: &'a [Cow<'_, Item>]) -> Self {
        let mut result = Self {
            roots: Vec::new(),
            children: HashMap::new(),
            parents: HashMap::new(),
        };
        if items
            .iter()
            .all(|item| item.presentation.parent_id.is_none())
        {
            result.roots.extend(items.iter().map(Cow::as_ref));
            return result;
        }
        let by_id: HashMap<_, _> = items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id.as_str(), index))
            .collect();
        let raw_parents: Vec<_> = items
            .iter()
            .map(|item| {
                item.presentation
                    .parent_id
                    .as_deref()
                    .and_then(|id| by_id.get(id).copied())
            })
            .collect();
        // Each path is finalized before starting the next. Reaching a visiting
        // node therefore means a cycle in this path, while a finished path can
        // immediately supply its result to every node leading to it.
        const VISITING: u8 = 1;
        const ACYCLIC: u8 = 2;
        const CYCLIC: u8 = 3;
        let mut state = vec![0; items.len()];
        let mut path = Vec::new();
        for start in 0..items.len() {
            if state[start] != 0 {
                continue;
            }
            let mut next = Some(start);
            let outcome = loop {
                let Some(index) = next else {
                    break ACYCLIC;
                };
                match state[index] {
                    ACYCLIC => break ACYCLIC,
                    VISITING | CYCLIC => break CYCLIC,
                    _ => {
                        state[index] = VISITING;
                        path.push(index);
                        next = raw_parents[index];
                    }
                }
            };
            for index in path.drain(..) {
                state[index] = outcome;
            }
        }
        let subagents: Vec<_> = items
            .iter()
            .map(|item| row_kind(item) == RowKind::Subagent)
            .collect();
        for (index, item) in items.iter().enumerate() {
            if let Some(owner) =
                raw_parents[index].filter(|&owner| state[index] == ACYCLIC && subagents[owner])
            {
                let parent = items[owner].id.as_str();
                result.parents.insert(item.id.as_str(), parent);
                result.children.entry(parent).or_default().push(item);
            } else {
                result.roots.push(item);
            }
        }
        result
    }
}

/// Controls and approval gates are indexed once per turn, instead of scanning
/// all controls and every nested sibling list for each item and nesting level.
#[derive(Default)]
struct TurnRequestIndex<'a> {
    approvals: HashSet<&'a str>,
    roots: Vec<&'a RequestRecord>,
    nested: HashMap<&'a str, Vec<&'a RequestRecord>>,
}

impl<'a> TurnRequestIndex<'a> {
    fn new(requests: &[&'a RequestRecord], nesting: &ItemNesting<'a>) -> Self {
        let mut index = Self::default();
        let root_parent = nesting
            .roots
            .first()
            .and_then(|item| item.presentation.parent_id.as_deref());
        for &record in requests {
            if record.kind == RequestKind::Approval
                && let Some(item) = record.item_id.as_deref()
            {
                index.approvals.insert(item);
            }
            let parent = record
                .item_id
                .as_deref()
                .and_then(|id| nesting.parents.get(id))
                .copied();
            // Preserve the raw first-root parent behavior for malformed/orphan
            // inputs as well as the ordinary unparented root controls.
            if parent.is_none() || parent == root_parent {
                index.roots.push(record);
            }
            if let Some(parent) = parent {
                index.nested.entry(parent).or_default().push(record);
            }
        }
        index
    }

    fn for_items(&self, items: &[&Item], level: usize) -> &[&'a RequestRecord] {
        if level == 0 {
            return &self.roots;
        }
        items
            .first()
            .and_then(|item| item.presentation.parent_id.as_deref())
            .and_then(|parent| self.nested.get(parent))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

fn verbose_rows(
    items: &[Cow<'_, Item>],
    orders: &HashMap<String, u64>,
    requests: &[&RequestRecord],
    turn: &str,
    options: &ProjectionOptions,
) -> Vec<Row> {
    let nesting = ItemNesting::new(items);
    let requests = TurnRequestIndex::new(requests, &nesting);
    struct Context<'a> {
        orders: &'a HashMap<String, u64>,
        requests: &'a TurnRequestIndex<'a>,
        nested: &'a HashMap<&'a str, Vec<&'a Item>>,
        turn: &'a str,
        options: &'a ProjectionOptions,
    }
    fn walk(items: &[&Item], context: &Context<'_>, level: usize) -> Vec<Row> {
        // Iterative provider inputs can be arbitrarily deep; cap projection depth.
        if level > 64 {
            let mut pending = items.to_vec();
            let mut rows = Vec::new();
            while let Some(item) = pending.pop() {
                rows.push(item_row(item, context.options));
                if let Some(children) = context.nested.get(item.id.as_str()) {
                    pending.extend(children.iter().rev().copied());
                }
            }
            return rows;
        }
        let mut entries: Vec<(u64, usize, Row, bool)> = Vec::new();
        for (at, item) in items.iter().enumerate() {
            let mut row = item_row(item, context.options);
            if let Some(children) = context.nested.get(item.id.as_str()) {
                row.children = walk(children, context, level + 1);
            }
            let gated = context.requests.approvals.contains(item.id.as_str());
            let eligible = eligible(item, gated);
            entries.push((
                context
                    .orders
                    .get(&item.id)
                    .copied()
                    .unwrap_or(at as u64 * 2 + 1),
                at,
                row,
                eligible,
            ));
        }
        for record in context.requests.for_items(items, level) {
            entries.push((record.requested_seq, usize::MAX, request_row(record), false));
        }
        entries.sort_by_key(|(seq, at, _, _)| (*seq, *at));
        let mut rows = Vec::new();
        let mut run = Vec::new();
        for (_, _, row, eligible) in entries {
            if eligible || (row.kind == RowKind::Commentary && !run.is_empty()) {
                run.push(row);
            } else {
                flush(&mut rows, &mut run, context.turn, context.options);
                rows.push(row);
            }
        }
        flush(&mut rows, &mut run, context.turn, context.options);
        rows
    }
    walk(
        &nesting.roots,
        &Context {
            orders,
            requests: &requests,
            nested: &nesting.children,
            turn,
            options,
        },
        0,
    )
}

fn flush(rows: &mut Vec<Row>, run: &mut Vec<Row>, turn: &str, options: &ProjectionOptions) {
    let count = run
        .iter()
        .filter(|row| row.kind != RowKind::Commentary)
        .count();
    if count < 2 {
        rows.append(run);
        return;
    }
    let (comments, members): (Vec<_>, Vec<_>) = run
        .drain(..)
        .partition(|row| row.kind == RowKind::Commentary);
    let buckets = [
        (RowKind::Read, "Read", "file", "files"),
        (RowKind::Search, "Searched", "pattern", "patterns"),
        (RowKind::List, "Listed", "directory", "directories"),
        (RowKind::WebSearch, "Searched the web", "time", "times"),
        (RowKind::WebFetch, "Fetched", "page", "pages"),
        (RowKind::Mcp, "Used MCP", "time", "times"),
        (RowKind::Shell, "Ran", "command", "commands"),
        (RowKind::Reasoning, "Thought", "time", "times"),
        (RowKind::Other, "Used", "tool", "tools"),
    ];
    let headers: Vec<_> = buckets
        .iter()
        .filter_map(|(kind, verb, one, many)| {
            let count = members.iter().filter(|row| row.kind == *kind).count();
            (count > 0).then(|| format!("{verb} {count} {}", if count == 1 { one } else { many }))
        })
        .collect();
    let mut title = headers
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ");
    if headers.len() > 3 {
        title.push_str(&format!(" · +{} more", headers.len() - 3));
    }
    let id = format!("g:{turn}:{}", members[0].id);
    let current = members.iter().rev().find(|row| row.running);
    let duration_ms = sum_durations(members.iter().map(|row| row.duration_ms));
    rows.push(Row {
        id: id.clone(),
        kind: RowKind::Group,
        title,
        open: options.disclosure.is_open(&id, options.mode, false),
        failed_count: members.iter().map(|row| row.failed_count).sum(),
        running: current.is_some(),
        duration_ms,
        current_label: current.and_then(|row| row.current_label.clone()),
        provisional: options.provisional,
        children: members,
        comments,
        ..Row::default()
    });
}

fn eligible(item: &Item, approval_gated: bool) -> bool {
    if super::workers::is_marker(item) || super::lineage::is_marker(item) {
        return false;
    }
    // A forward-compatible hint must never bypass the command-body check.
    if let ItemBody::Command { command, .. } = &item.body {
        return !approval_gated && is_read_only_shell(command);
    }
    match row_kind(item) {
        RowKind::Read
        | RowKind::Search
        | RowKind::List
        | RowKind::WebSearch
        | RowKind::WebFetch
        | RowKind::Mcp
        | RowKind::Other
        | RowKind::Reasoning => true,
        RowKind::Shell => {
            !approval_gated
                && match &item.body {
                    ItemBody::Command { command, .. } => is_read_only_shell(command),
                    _ => item
                        .presentation
                        .semantics
                        .as_ref()
                        .and_then(|semantics| semantics.target.as_deref())
                        .is_some_and(is_read_only_shell),
                }
        }
        _ => false,
    }
}

fn row_kind(item: &Item) -> RowKind {
    // Host/provider card metadata takes precedence over generic tool semantics.
    if super::workers::of(item).is_some() {
        return RowKind::Worker;
    }
    if item.presentation.subagent.is_some() {
        return RowKind::Subagent;
    }
    if super::lineage::is_marker(item) {
        return RowKind::Lineage;
    }
    match &item.body {
        ItemBody::UserMessage { .. } => RowKind::User,
        ItemBody::AgentMessage { .. } => {
            if item.presentation.phase == Some(MessagePhase::Commentary) {
                RowKind::Commentary
            } else {
                RowKind::Final
            }
        }
        ItemBody::Reasoning { .. } => RowKind::Reasoning,
        ItemBody::Command { .. } => RowKind::Shell,
        ItemBody::FileChange { .. } => RowKind::Edit,
        ItemBody::Plan { .. } => RowKind::Plan,
        ItemBody::Todo { .. } => RowKind::Todo,
        ItemBody::Compaction => RowKind::Compaction,
        ItemBody::Notice { .. } => RowKind::Notice,
        _ => match item
            .presentation
            .semantics
            .as_ref()
            .map(|semantics| semantics.kind)
            .unwrap_or_default()
        {
            ToolKind::Shell => RowKind::Shell,
            ToolKind::Read => RowKind::Read,
            ToolKind::Search => RowKind::Search,
            ToolKind::List => RowKind::List,
            ToolKind::WebSearch => RowKind::WebSearch,
            ToolKind::WebFetch => RowKind::WebFetch,
            ToolKind::Mcp => RowKind::Mcp,
            ToolKind::Subagent => RowKind::Subagent,
            ToolKind::Other => RowKind::Other,
        },
    }
}

fn item_row(item: &Item, options: &ProjectionOptions) -> Row {
    let kind = row_kind(item);
    let semantics = item.presentation.semantics.as_ref();
    let title = match kind {
        RowKind::User => "User",
        RowKind::Final => "Answer",
        RowKind::Commentary => "Commentary",
        RowKind::Read => "Read",
        RowKind::Search => "Searched",
        RowKind::List => "Listed",
        RowKind::WebSearch => "Searched the web",
        RowKind::WebFetch => "Fetched",
        RowKind::Mcp => "Used MCP",
        RowKind::Shell => "Ran",
        RowKind::Worker => "Worker",
        RowKind::Subagent => "Agent",
        RowKind::Reasoning => "Thought",
        RowKind::Edit => "Edited",
        RowKind::Plan => "Plan",
        RowKind::Todo => "Todo",
        RowKind::Compaction => "Context compacted",
        RowKind::Notice => "Notice",
        RowKind::Lineage => "Continued in another chat",
        _ => "Used tool",
    }
    .to_string();
    let title = super::lineage::of(item).map_or(title, |mark| mark.label_in(options.provider));
    let target = if let ItemBody::Command { command, .. } = &item.body {
        Some(command.clone())
    } else {
        semantics
            .and_then(|semantics| semantics.target.clone())
            .or_else(|| match &item.body {
                ItemBody::Command { command, .. } => Some(command.clone()),
                ItemBody::WebSearch { query } => Some(query.clone()),
                ItemBody::FileChange { changes } if changes.len() == 1 => {
                    Some(changes[0].path.clone())
                }
                _ => None,
            })
    };
    // An operation completing is not evidence that its delegated work finished.
    // Worker inventory/live-tail status is resolved by the native card renderer;
    // Provider subagents use presentation metadata or exact legacy tool input.
    let subagent = super::subagents::of(item, options.provider);
    let running = subagent.as_ref().map_or_else(
        || is_work(item) && item.status == ItemStatus::InProgress,
        |run| run.status == super::subagents::RunStatus::Running,
    );
    let failed = subagent.as_ref().map_or_else(
        || is_work(item) && failed(item),
        |run| run.status == super::subagents::RunStatus::Failed,
    );
    Row {
        id: item.id.clone(),
        kind,
        title,
        target,
        open: !matches!(
            kind,
            RowKind::User
                | RowKind::Final
                | RowKind::Commentary
                | RowKind::Compaction
                | RowKind::Notice
                | RowKind::Lineage
        ) && options.disclosure.is_open(&item.id, options.mode, false),
        failed_count: usize::from(failed),
        running,
        duration_ms: item_duration(item, options.now_ms),
        current_label: running.then(|| short_label(item)),
        artifacts: artifacts(item),
        elided: options.elided_items.contains(&item.id),
        ..Row::default()
    }
}

fn artifacts(item: &Item) -> Vec<ArtifactRef> {
    let mut paths: Vec<_> = item
        .presentation
        .semantics
        .iter()
        .flat_map(|semantics| &semantics.produced)
        .cloned()
        .collect();
    // Native edits are recorded provenance on older hosts too. Explicit refs
    // retain their order and line hints; append only previously absent paths.
    if let ItemBody::FileChange { changes } = &item.body {
        let mut seen: HashSet<_> = paths.iter().map(|path| path.path.clone()).collect();
        for change in changes {
            if !change.path.is_empty() && seen.insert(change.path.clone()) {
                paths.push(PathRef {
                    path: change.path.clone(),
                    line: None,
                });
            }
        }
    }
    let mut refs: Vec<_> = paths
        .into_iter()
        .map(|path| ArtifactRef {
            item_id: item.id.clone(),
            path: Some(path),
            image_index: None,
        })
        .collect();
    refs.extend(
        item.presentation
            .images
            .iter()
            .enumerate()
            .map(|(index, _)| ArtifactRef {
                item_id: item.id.clone(),
                path: None,
                image_index: Some(index),
            }),
    );
    refs
}

fn failed(item: &Item) -> bool {
    item.status == ItemStatus::Failed
        || matches!(item.body, ItemBody::Command { exit_code: Some(code), .. } if code != 0)
}

fn is_work(item: &Item) -> bool {
    !super::workers::is_marker(item)
        && !super::lineage::is_marker(item)
        && !matches!(
            item.body,
            ItemBody::UserMessage { .. }
                | ItemBody::AgentMessage { .. }
                | ItemBody::Notice { .. }
                | ItemBody::Compaction
        )
}

fn item_duration(item: &Item, now_ms: Option<u64>) -> Option<u64> {
    let start = item.started_at?;
    let end = item.ended_at.or_else(|| {
        (item.status == ItemStatus::InProgress)
            .then_some(now_ms)
            .flatten()
    })?;
    end.checked_sub(start)
}

fn sum_durations(durations: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    durations.flatten().fold(None, |total, duration| {
        Some(total.unwrap_or(0u64).saturating_add(duration))
    })
}

fn activity_duration(
    turn: &str,
    work: &[&Item],
    running: bool,
    options: &ProjectionOptions,
) -> Option<u64> {
    activity_duration_at(options.turn_timing.get(turn), work, running, options.now_ms)
}

fn activity_duration_at(
    timing: Option<&TurnTiming>,
    work: &[&Item],
    running: bool,
    now_ms: Option<u64>,
) -> Option<u64> {
    if let Some(timing) = timing
        && let (Some(start), Some(end)) = (
            timing.started_at,
            timing
                .ended_at
                .or_else(|| running.then_some(now_ms).flatten()),
        )
    {
        return end.checked_sub(start);
    }
    // Legacy turns have no wall-clock source. Only sum durations actually known.
    sum_durations(work.iter().map(|item| item_duration(item, now_ms)))
}

fn activity_title(
    work: &[&Item],
    running: bool,
    duration: Option<u64>,
    current: Option<&str>,
) -> String {
    if running {
        let mut title = "Working".to_string();
        if let Some(duration) = duration {
            title.push_str(&format!(" {}", format_duration(duration)));
        }
        if let Some(current) = current {
            title.push_str(&format!(" · {current}"));
        }
        return title;
    }
    let mut title = "Worked".to_string();
    if let Some(duration) = duration {
        title.push_str(&format!(" {}", format_duration(duration)));
    }
    // Small turns avoid an allocation/hash pass; the bounded scan is cheaper
    // there. Larger turns must not scan the whole turn once per subagent.
    let parents: Option<HashSet<_>> = (work.len() > 64).then(|| {
        work.iter()
            .filter_map(|item| item.presentation.parent_id.as_deref())
            .collect()
    });
    let has_children = |id: &str| {
        parents.as_ref().map_or_else(
            || {
                work.iter()
                    .any(|child| child.presentation.parent_id.as_deref() == Some(id))
            },
            |parents| parents.contains(id),
        )
    };
    let mut edits = HashSet::new();
    let (mut commands, mut reads, mut calls) = (0, 0, 0);
    let mut all_mcp = true;
    for item in work {
        if let ItemBody::FileChange { changes } = &item.body {
            edits.extend(changes.iter().map(|change| change.path.as_str()));
        }
        match row_kind(item) {
            RowKind::Shell => commands += 1,
            RowKind::Read
            | RowKind::Search
            | RowKind::List
            | RowKind::WebSearch
            | RowKind::WebFetch => reads += 1,
            RowKind::Mcp => calls += 1,
            // A raw parent with known children is metadata, not another call.
            // This intentionally also includes orphaned/cyclic relationships.
            RowKind::Subagent if has_children(item.id.as_str()) => {}
            RowKind::Subagent | RowKind::Other => {
                calls += 1;
                all_mcp = false;
            }
            _ => {}
        }
    }
    let (call, calls_label) = if all_mcp {
        ("MCP call", "MCP calls")
    } else {
        ("tool call", "tool calls")
    };
    for (count, one, many) in [
        (edits.len(), "file edited", "files edited"),
        (commands, "command", "commands"),
        (reads, "read", "reads"),
        (calls, call, calls_label),
    ] {
        if count > 0 {
            title.push_str(&format!(
                " · {count} {}",
                if count == 1 { one } else { many }
            ));
        }
    }
    title
}

fn short_label(item: &Item) -> String {
    let body_target = match &item.body {
        ItemBody::FileChange { changes } => changes.first().map(|change| change.path.as_str()),
        ItemBody::ToolCall { tool, .. } => Some(tool.as_str()),
        _ => None,
    };
    let target = item
        .presentation
        .semantics
        .as_ref()
        .and_then(|semantics| semantics.target.as_deref())
        .or(body_target)
        .unwrap_or("");
    let verb = match row_kind(item) {
        RowKind::Read => "reading",
        RowKind::Search | RowKind::WebSearch => "searching",
        RowKind::List => "listing",
        RowKind::WebFetch => "fetching",
        RowKind::Reasoning => "thinking",
        RowKind::Subagent => "agent",
        RowKind::Edit => "editing",
        RowKind::Shell => "running",
        RowKind::Plan => "planning",
        RowKind::Todo => "updating todo",
        _ => "using tool",
    };
    if target.is_empty() {
        verb.into()
    } else {
        format!("{verb} {target}")
    }
}

fn format_duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn request_row(record: &RequestRecord) -> Row {
    let prefix = match record.resolution.as_deref() {
        Some("accept" | "accept_for_session") => "Allowed",
        Some("decline") => "Declined",
        Some("cancel") => "Cancelled",
        Some("answered") => "Answered",
        Some(_) => "Resolved",
        None if record.resolved_seq.is_some() => "Resolved",
        None => {
            if record.kind == RequestKind::Question {
                "Question pending"
            } else {
                "Approval pending"
            }
        }
    };
    let mut title = format!("{prefix}: {}", record.title);
    if let Some(summary) = record
        .answer_summary
        .as_ref()
        .filter(|summary| !summary.is_empty())
    {
        title.push_str(&format!(" · {summary}"));
    }
    Row {
        id: format!("request:{}", record.request_id),
        kind: RowKind::Request,
        title,
        request_id: Some(record.request_id.clone()),
        ..Row::default()
    }
}

/// Map a persistent item anchor to the nearest visible row after a disclosure,
/// pagination, or streaming change. Also handles group IDs changing on backfill.
pub fn visible_anchor(rows: &[Row], item_id: &str) -> Option<String> {
    fn contains(row: &Row, id: &str) -> bool {
        row.id == id
            || row.children.iter().any(|row| contains(row, id))
            || row.comments.iter().any(|row| contains(row, id))
    }
    for row in rows {
        if row.id == item_id {
            return Some(row.id.clone());
        }
        // Commentary is adjacent to the header and visible with a closed group.
        if let Some(anchor) = visible_anchor(&row.comments, item_id) {
            return Some(anchor);
        }
        if contains(row, item_id) {
            if row.open {
                return visible_anchor(&row.children, item_id)
                    .or_else(|| visible_anchor(&row.comments, item_id));
            }
            return Some(row.id.clone());
        }
    }
    None
}

/// Hidden banners keep their place in their own turn when switching modes.
/// Prefer the preceding visible item (usually the answer), never the next turn.
pub fn transcript_anchor(transcript: &Transcript, rows: &[Row], item_id: &str) -> Option<String> {
    if let Some(anchor) = visible_anchor(rows, item_id) {
        return Some(anchor);
    }
    let at = transcript
        .items
        .iter()
        .position(|item| item.id == item_id)?;
    let item = &transcript.items[at];
    transcript.items[..at]
        .iter()
        .rev()
        .take_while(|other| same_turn(item, other))
        .chain(
            transcript.items[at + 1..]
                .iter()
                .take_while(|other| same_turn(item, other)),
        )
        .find_map(|other| visible_anchor(rows, &other.id))
}

/// Remap manual overrides after streaming regroup or oversized-page backfill.
/// A group's first known child is a stable item anchor even when its first member
/// changes. An opened standalone call keeps its output visible in a new group.
/// Existing IDs win; mode/bulk changes should use the explicit reset transitions.
pub fn remap_disclosures(
    state: &DisclosureState,
    before: &[Row],
    after: &[Row],
) -> DisclosureState {
    struct Index<'a> {
        rows: HashMap<&'a str, &'a Row>,
        groups: HashMap<&'a str, &'a str>,
    }
    fn index(rows: &[Row]) -> Index<'_> {
        let mut result = Index {
            rows: HashMap::new(),
            groups: HashMap::new(),
        };
        let mut pending: Vec<_> = rows.iter().rev().map(|row| (row, None)).collect();
        while let Some((row, outer_group)) = pending.pop() {
            result.rows.entry(row.id.as_str()).or_insert(row);
            if let Some(group) = outer_group {
                result.groups.entry(row.id.as_str()).or_insert(group);
            }
            // Preserve group_for's outermost containing group and its original
            // child-only traversal. Adjacent commentary is not a group member.
            let group =
                outer_group.or_else(|| (row.kind == RowKind::Group).then_some(row.id.as_str()));
            pending.extend(row.children.iter().rev().map(|child| (child, group)));
        }
        result
    }
    fn first_leaf(row: &Row) -> &str {
        row.children.first().map_or(row.id.as_str(), first_leaf)
    }
    if state.overrides.is_empty() {
        return state.clone();
    }
    let before = index(before);
    let after = index(after);
    let mut result = state.clone();
    for (id, open) in &state.overrides {
        if let Some(old) = before.rows.get(id.as_str()) {
            let moved_group = old.kind == RowKind::Group && !after.rows.contains_key(id.as_str());
            let newly_grouped_leaf = *open
                && old.open
                && old.children.is_empty()
                && !before.groups.contains_key(id.as_str());
            if !moved_group && !newly_grouped_leaf {
                continue;
            }
            let anchor = first_leaf(old);
            if let Some(&new_id) = after.groups.get(anchor) {
                if moved_group {
                    result.overrides.remove(id);
                }
                if !state.overrides.contains_key(new_id) {
                    result
                        .overrides
                        .entry(new_id.into())
                        .and_modify(|value| *value |= *open)
                        .or_insert(*open);
                }
            }
        }
    }
    result
}

/// Text selection's visible leaves; collapsed containers export only headers.
pub fn selection_leaves(rows: &[Row]) -> Vec<(String, String)> {
    let mut leaves = Vec::new();
    for row in rows {
        let text = match &row.target {
            Some(target) => format!("{} {target}", row.title),
            None => row.title.clone(),
        };
        leaves.push((row.id.clone(), text));
        if row.open {
            leaves.extend(selection_leaves(&row.children));
        }
        leaves.extend(selection_leaves(&row.comments));
    }
    leaves
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FixtureFile {
        spec_version: u8,
        #[serde(default)]
        cases: Vec<Fixture>,
        #[serde(default)]
        shell_cases: Vec<ShellCase>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ShellCase {
        command: String,
        allowed: bool,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fixture {
        name: String,
        #[serde(deserialize_with = "strict_items")]
        items: Vec<Item>,
        #[serde(default)]
        item_sequences: HashMap<String, u64>,
        #[serde(default, deserialize_with = "strict_requests")]
        requests: Vec<RequestRecord>,
        #[serde(default, deserialize_with = "strict_options")]
        options: ProjectionOptions,
        expected_rows: Value,
        #[serde(default)]
        anchors: HashMap<String, String>,
        expected_selection: Option<Vec<String>>,
        page: Option<PageFixture>,
        #[serde(default)]
        actions: Vec<Action>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PageFixture {
        start: usize,
        end: usize,
        max_items: Option<usize>,
        expected: ExpectedWindow,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ExpectedWindow {
        start: usize,
        end: usize,
        provisional: bool,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Action {
        action: String,
        id: Option<String>,
        open: Option<bool>,
        mode: Option<DisplayMode>,
        #[serde(default, deserialize_with = "strict_optional_items")]
        items: Option<Vec<Item>>,
        expected_rows: Option<Value>,
        expected_open_ids: Vec<String>,
        #[serde(default)]
        anchors: HashMap<String, String>,
    }

    // Runtime wire models intentionally accept future fields. Fixture inputs do
    // not: a typo must fail before it can silently alter the contract example.
    fn keys(value: &Value, allowed: &[&str]) -> Result<(), String> {
        if let Some(fields) = value.as_object() {
            for key in fields.keys() {
                if !allowed.contains(&key.as_str()) {
                    return Err(format!("unknown fixture field `{key}`"));
                }
            }
        }
        Ok(())
    }

    fn check_item(value: &Value) -> Result<(), String> {
        keys(
            value,
            &[
                "id",
                "turn_id",
                "status",
                "body",
                "presentation",
                "started_at",
                "ended_at",
                "elided",
            ],
        )?;
        keys(
            &value["elided"],
            &[
                "event",
                "of",
                "kind",
                "tool_kind",
                "item_id",
                "turn_id",
                "parent_id",
                "status",
                "started_at",
                "ended_at",
                "bytes",
                "reason",
                "elided",
            ],
        )?;
        let presentation = &value["presentation"];
        keys(
            presentation,
            &[
                "phase",
                "images",
                "attachments",
                "semantics",
                "parent_id",
                "subagent",
                "worker",
                "elided",
            ],
        )?;
        keys(
            &presentation["worker"],
            &[
                "session_id",
                "kind",
                "title",
                "harness",
                "model",
                "worktree_id",
                "assignment",
                "noted_at",
                "after",
                "late",
                "follow_up",
                "final",
            ],
        )?;
        keys(
            &presentation["worker"]["final"],
            &[
                "status",
                "ended_at",
                "tail",
                "turn",
                "turn_end",
                "done_since",
            ],
        )?;
        keys(
            &presentation["subagent"],
            &[
                "name", "purpose", "status", "summary", "children", "omitted",
            ],
        )?;
        if let Some(children) = presentation["subagent"]["children"].as_array() {
            for child in children {
                keys(child, &["id", "status", "summary"])?;
            }
        }
        let semantics = &presentation["semantics"];
        keys(
            semantics,
            &[
                "kind",
                "target",
                "range",
                "match_count",
                "excerpt",
                "produced",
                "worker",
                "elided",
            ],
        )?;
        keys(&semantics["range"], &["start", "end", "elided"])?;
        keys(
            &semantics["worker"],
            &["role", "objective", "child_count", "elided"],
        )?;
        if let Some(paths) = semantics["produced"].as_array() {
            for path in paths {
                keys(path, &["path", "line", "elided"])?;
            }
        }
        if let Some(images) = presentation["images"].as_array() {
            for image in images {
                keys(image, &["label", "source"])?;
                keys(
                    &image["source"],
                    &["kind", "path", "mime", "base64", "url", "reason", "elided"],
                )?;
            }
        }
        if let Some(attachments) = presentation["attachments"].as_array() {
            for attachment in attachments {
                keys(
                    attachment,
                    &[
                        "id",
                        "name",
                        "path",
                        "kind",
                        "bytes",
                        "fingerprint",
                        "preview",
                    ],
                )?;
                keys(&attachment["kind"], &["kind", "mime"])?;
                keys(&attachment["preview"], &["kind", "excerpt", "path"])?;
            }
        }
        let body = &value["body"];
        let allowed: &[&str] = match body["type"].as_str() {
            Some("user_message" | "agent_message" | "reasoning") => &["type", "text"],
            Some("plan") => &["type", "explanation", "steps"],
            Some("command") => &["type", "command", "cwd", "output", "exit_code"],
            Some("file_change") => &["type", "changes"],
            Some("tool_call") => &["type", "server", "tool", "input", "output"],
            Some("web_search") => &["type", "query"],
            Some("todo") => &["type", "items"],
            Some("compaction") => &["type"],
            Some("notice") => &[
                "type",
                "level",
                "text",
                "kind",
                "resolved",
                "dismissed",
                "resets_at",
            ],
            _ => &["type"],
        };
        keys(body, allowed)?;
        for field in ["steps", "items"] {
            if let Some(steps) = body[field].as_array() {
                for step in steps {
                    keys(step, &["text", "status"])?;
                }
            }
        }
        if let Some(changes) = body["changes"].as_array() {
            for change in changes {
                keys(change, &["path", "kind", "diff"])?;
            }
        }
        Ok(())
    }

    fn strict_items<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Vec<Item>, D::Error> {
        let values = Vec::<Value>::deserialize(decoder)?;
        values
            .into_iter()
            .map(|value| {
                check_item(&value).map_err(serde::de::Error::custom)?;
                serde_json::from_value(value).map_err(serde::de::Error::custom)
            })
            .collect()
    }

    fn strict_optional_items<'de, D: serde::Deserializer<'de>>(
        decoder: D,
    ) -> Result<Option<Vec<Item>>, D::Error> {
        let values = Option::<Value>::deserialize(decoder)?;
        values
            .map(|values| strict_items(values).map_err(serde::de::Error::custom))
            .transpose()
    }

    fn strict_requests<'de, D: serde::Deserializer<'de>>(
        decoder: D,
    ) -> Result<Vec<RequestRecord>, D::Error> {
        let values = Vec::<Value>::deserialize(decoder)?;
        values
            .into_iter()
            .map(|value| {
                keys(
                    &value,
                    &[
                        "request_id",
                        "kind",
                        "title",
                        "turn_id",
                        "item_id",
                        "requested_seq",
                        "resolved_seq",
                        "resolution",
                        "answer_summary",
                    ],
                )
                .map_err(serde::de::Error::custom)?;
                serde_json::from_value(value).map_err(serde::de::Error::custom)
            })
            .collect()
    }

    fn strict_options<'de, D: serde::Deserializer<'de>>(
        decoder: D,
    ) -> Result<ProjectionOptions, D::Error> {
        let value = Value::deserialize(decoder)?;
        keys(
            &value,
            &[
                "mode",
                "provider",
                "now_ms",
                "running_turn_id",
                "turn_timing",
                "disclosure",
                "provisional",
                "elided_items",
            ],
        )
        .map_err(serde::de::Error::custom)?;
        keys(&value["disclosure"], &["overrides", "bulk"]).map_err(serde::de::Error::custom)?;
        if let Some(timings) = value["turn_timing"].as_object() {
            for timing in timings.values() {
                keys(timing, &["started_at", "ended_at"]).map_err(serde::de::Error::custom)?;
            }
        }
        serde_json::from_value(value).map_err(serde::de::Error::custom)
    }

    fn find<'a>(rows: &'a [Row], id: &str) -> Option<&'a Row> {
        for row in rows {
            if row.id == id {
                return Some(row);
            }
            if let Some(row) = find(&row.children, id).or_else(|| find(&row.comments, id)) {
                return Some(row);
            }
        }
        None
    }

    fn open_ids(rows: &[Row]) -> Vec<String> {
        let mut ids = Vec::new();
        for row in rows {
            if row.open {
                ids.push(row.id.clone());
            }
            ids.extend(open_ids(&row.children));
        }
        ids.sort();
        ids
    }

    #[test]
    fn shared_v2_fixtures() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../spec/transcript-grouping/v2");
        let mut paths: Vec<_> = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();
        assert_eq!(paths.len(), 8, "every shared fixture file must be checked");
        let mut case_names = HashSet::new();
        let mut shell_commands = HashSet::new();
        let mut case_count = 0;
        let mut shell_count = 0;
        for path in paths {
            let file: FixtureFile = serde_json::from_slice(&std::fs::read(&path).unwrap())
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert_eq!(file.spec_version, SPEC_VERSION);
            assert!(!file.cases.is_empty() || !file.shell_cases.is_empty());
            case_count += file.cases.len();
            shell_count += file.shell_cases.len();
            for shell in file.shell_cases {
                assert!(
                    shell_commands.insert(shell.command.clone()),
                    "duplicate shared shell command: {}",
                    shell.command
                );
                assert_eq!(
                    is_read_only_shell(&shell.command),
                    shell.allowed,
                    "{}: {}",
                    path.display(),
                    shell.command
                );
            }
            for mut fixture in file.cases {
                assert!(
                    case_names.insert(fixture.name.clone()),
                    "duplicate shared fixture name: {}",
                    fixture.name
                );
                if let Some(page) = fixture.page.take() {
                    let window =
                        turn_aligned_page(&fixture.items, page.start, page.end, page.max_items);
                    assert_eq!(
                        (window.start, window.end, window.provisional),
                        (
                            page.expected.start,
                            page.expected.end,
                            page.expected.provisional
                        ),
                        "{} page",
                        fixture.name
                    );
                    fixture.items = fixture.items[window.start..window.end].to_vec();
                    fixture.options.provisional |= window.provisional;
                }
                let mut rows = project(
                    &fixture.items,
                    &fixture.item_sequences,
                    &fixture.requests,
                    &fixture.options,
                );
                assert_eq!(
                    serde_json::to_value(&rows).unwrap(),
                    fixture.expected_rows,
                    "{}: {}",
                    path.display(),
                    fixture.name
                );
                for (item, row) in &fixture.anchors {
                    assert_eq!(
                        visible_anchor(&rows, item).as_ref(),
                        Some(row),
                        "{} anchor {item}",
                        fixture.name
                    );
                }
                if let Some(selection) = fixture.expected_selection {
                    assert_eq!(
                        selection_leaves(&rows)
                            .into_iter()
                            .map(|(_, text)| text)
                            .collect::<Vec<_>>(),
                        selection,
                        "{} selection",
                        fixture.name
                    );
                }
                for action in fixture.actions {
                    match action.action.as_str() {
                        "toggle" => {
                            let id = action.id.unwrap();
                            let open = find(&rows, &id).unwrap().open;
                            fixture.options.disclosure.toggle(&id, open);
                        }
                        "bulk" => fixture.options.disclosure.reset_bulk_for_rows(
                            action.open.unwrap(),
                            fixture.options.mode,
                            &rows,
                        ),
                        "mode" => {
                            fixture.options.mode = action.mode.unwrap();
                            fixture.options.disclosure.reset_mode();
                        }
                        "backfill" => {
                            fixture.items = action.items.unwrap();
                            fixture.options.provisional = false;
                            let after = project(
                                &fixture.items,
                                &fixture.item_sequences,
                                &fixture.requests,
                                &fixture.options,
                            );
                            fixture.options.disclosure =
                                remap_disclosures(&fixture.options.disclosure, &rows, &after);
                        }
                        word => panic!("unknown fixture action {word}"),
                    }
                    rows = project(
                        &fixture.items,
                        &fixture.item_sequences,
                        &fixture.requests,
                        &fixture.options,
                    );
                    let mut expected = action.expected_open_ids;
                    expected.sort();
                    assert_eq!(
                        open_ids(&rows),
                        expected,
                        "{} {}",
                        fixture.name,
                        action.action
                    );
                    if let Some(expected_rows) = action.expected_rows {
                        assert_eq!(
                            serde_json::to_value(&rows).unwrap(),
                            expected_rows,
                            "{} {} rows",
                            fixture.name,
                            action.action
                        );
                    }
                    for (item, row) in action.anchors {
                        assert_eq!(
                            visible_anchor(&rows, &item),
                            Some(row),
                            "{} anchor {item}",
                            fixture.name
                        );
                    }
                }
            }
        }
        assert_eq!(
            case_count, 66,
            "every shared projection case must be checked"
        );
        assert_eq!(shell_count, 44, "every shared shell case must be checked");
    }

    #[test]
    fn transcript_adapter_uses_captured_turn_wall_time_instead_of_parallel_call_sum() {
        use serde_json::json;
        let mut transcript = Transcript::default();
        let events = [
            json!({"event":"turn_started","turn_id":"t","started_at":1000}),
            json!({"event":"item_completed","item":{"id":"a","turn_id":"t","status":"completed","started_at":2000,"ended_at":10000,"body":{"type":"command","command":"cargo test"}}}),
            json!({"event":"item_completed","item":{"id":"b","turn_id":"t","status":"completed","started_at":2000,"ended_at":10000,"body":{"type":"command","command":"cargo fmt"}}}),
            json!({"event":"turn_completed","turn_id":"t","ended_at":13000,"outcome":{"outcome":"completed"}}),
        ];
        for (at, event) in events.into_iter().enumerate() {
            transcript.apply_sequenced(at as u64 + 1, &serde_json::from_value(event).unwrap());
        }
        let rows = project_transcript(&transcript, &ProjectionOptions::default());
        assert_eq!(rows[0].title, "Worked 12s · 2 commands");
        assert_eq!(rows[0].duration_ms, Some(12000));
        assert_eq!(
            rows[0]
                .children
                .iter()
                .filter_map(|row| row.duration_ms)
                .sum::<u64>(),
            16000
        );
    }

    #[test]
    fn bulk_disclosure_only_changes_the_rows_present_when_invoked() {
        use serde_json::json;
        let original: Vec<Item> = serde_json::from_value(json!([
            {"id":"a","turn_id":"t","status":"completed","body":{"type":"command","command":"cat a"}},
            {"id":"b","turn_id":"t","status":"completed","body":{"type":"command","command":"cat b"}}
        ])).unwrap();
        for mode in [DisplayMode::Normal, DisplayMode::Verbose] {
            let mut options = ProjectionOptions {
                mode,
                ..ProjectionOptions::default()
            };
            let before = project(&original, &HashMap::new(), &[], &options);
            options.disclosure.reset_bulk_for_rows(true, mode, &before);
            assert_eq!(options.disclosure.bulk, None);
            let mut expanded = original.clone();
            expanded.push(
                serde_json::from_value(json!({
                    "id":"later", "turn_id":if mode == DisplayMode::Normal { "next" } else { "t" },
                    "body":{"type":"command","command":"cargo test"}
                }))
                .unwrap(),
            );
            let rows = project(&expanded, &HashMap::new(), &[], &options);
            if mode == DisplayMode::Normal {
                assert!(find(&rows, "activity:t").unwrap().open);
                assert!(!find(&rows, "activity:next").unwrap().open);
                assert!(!find(&rows, "g:t:a").unwrap().open);
                assert!(!find(&rows, "a").unwrap().open);
            } else {
                assert!(find(&rows, "g:t:a").unwrap().open);
                assert!(find(&rows, "a").unwrap().open);
                assert!(find(&rows, "b").unwrap().open);
            }
            assert!(!find(&rows, "later").unwrap().open);
            options.disclosure.reset_bulk_for_rows(false, mode, &rows);
            assert!(open_ids(&project(&expanded, &HashMap::new(), &[], &options)).is_empty());
        }
    }

    #[test]
    fn indexed_request_assignment_matches_scan_for_partial_and_legacy_turns() {
        let items: Vec<Item> = serde_json::from_value(serde_json::json!([
            {"id":"a", "turn_id":"t", "body":{"type":"agent_message", "text":"a"}},
            {"id":"b", "turn_id":"t", "body":{"type":"agent_message", "text":"b"}},
            {"id":"c", "body":{"type":"agent_message", "text":"c"}},
            {"id":"d", "turn_id":"u", "body":{"type":"agent_message", "text":"d"}},
            {"id":"e", "turn_id":"t", "body":{"type":"agent_message", "text":"e"}}
        ]))
        .unwrap();
        let items: Vec<_> = items.iter().map(Cow::Borrowed).collect();
        let mut requests = Vec::new();
        for turn in [
            None,
            Some("t"),
            Some("u"),
            Some("missing"),
            Some("legacy:c"),
        ] {
            for item in [
                None,
                Some("a"),
                Some("b"),
                Some("c"),
                Some("e"),
                Some("missing"),
            ] {
                // Deliberately unsorted and repeated sequences: ownership, not
                // input ordering, determines which turn can claim a request.
                for seq in [8, 0, u64::MAX, 4, 4, 1] {
                    requests.push(RequestRecord {
                        request_id: format!("request-{}", requests.len()),
                        turn_id: turn.map(str::to_owned),
                        item_id: item.map(str::to_owned),
                        requested_seq: seq,
                        ..RequestRecord::default()
                    });
                }
            }
        }
        let index = RequestIndex::new(&items, &requests);
        let mut unplaced: Vec<_> = requests
            .iter()
            .filter(|record| {
                !items.iter().any(|item| {
                    record
                        .turn_id
                        .as_ref()
                        .is_some_and(|turn| item.turn_id.as_ref() == Some(turn))
                        || record.item_id.as_ref() == Some(&item.id)
                })
            })
            .collect();
        unplaced.sort_by_key(|record| record.requested_seq);
        assert_eq!(index.unplaced, unplaced);
        let seen: HashSet<_> = requests
            .iter()
            .step_by(4)
            .map(|record| record.request_id.clone())
            .collect();
        for (turn, block) in [
            ("t", &items[..2]),
            ("legacy:c", &items[2..3]),
            ("u", &items[3..4]),
            ("t", &items[4..]),
        ] {
            for (first_seq, next_seq) in
                [(0, 5), (5, 8), (8, 4), (8, u64::MAX), (u64::MAX, u64::MAX)]
            {
                let expected: Vec<_> = requests
                    .iter()
                    .filter(|record| {
                        record.turn_id.as_deref() == Some(turn)
                            || (record.turn_id.is_none()
                                && record
                                    .item_id
                                    .as_ref()
                                    .is_some_and(|id| block.iter().any(|item| &item.id == id)))
                            || (record.turn_id.is_none()
                                && record.item_id.is_none()
                                && record.requested_seq >= first_seq
                                && record.requested_seq < next_seq
                                && !seen.contains(&record.request_id))
                    })
                    .collect();
                assert_eq!(
                    index.for_turn(turn, block, first_seq, next_seq, &seen),
                    expected
                );
            }
        }
    }

    fn nesting_item(index: usize, parent: Option<String>, subagent: bool) -> Item {
        serde_json::from_value(serde_json::json!({
            "id": format!("item-{index}"), "turn_id":"turn", "status":"completed",
            "body":{"type":"command", "command":format!("cat file-{index}")},
            "presentation":{"parent_id":parent, "subagent":subagent.then(|| serde_json::json!({
                "name":format!("Agent {index}"), "status":"done", "purpose":null, "summary":null
            }))}
        }))
        .unwrap()
    }

    fn nesting_requests(count: usize, seed: usize) -> Vec<RequestRecord> {
        (0..count + 3)
            .map(|index| RequestRecord {
                request_id: format!("request-{index}"),
                kind: if index % 3 == 0 {
                    RequestKind::Question
                } else {
                    RequestKind::Approval
                },
                item_id: (index != count)
                    .then(|| format!("item-{}", (index * 17 + seed) % (count + 3))),
                requested_seq: ((index * 13 + seed) % (count + 5)) as u64,
                ..RequestRecord::default()
            })
            .collect()
    }

    #[test]
    fn indexed_nesting_and_controls_match_ancestor_and_sibling_scans() {
        fn scan_parent<'a>(item: &'a Item, by_id: &HashMap<&str, &'a Item>) -> Option<&'a str> {
            let parent = item.presentation.parent_id.as_deref()?;
            if parent == item.id || row_kind(by_id.get(parent)?) != RowKind::Subagent {
                return None;
            }
            let mut visited = HashSet::from([item.id.as_str()]);
            let mut next = Some(parent);
            while let Some(id) = next {
                if !visited.insert(id) {
                    return None;
                }
                next = by_id
                    .get(id)
                    .and_then(|item| item.presentation.parent_id.as_deref());
            }
            Some(parent)
        }
        let mut random = 0x176a9b3_u64;
        for seed in 0..128 {
            let items: Vec<_> = (0..80)
                .map(|index| {
                    random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let parent = if random % 7 == 0 {
                        None
                    } else {
                        Some(format!("item-{}", (random >> 32) % 84))
                    };
                    Cow::Owned(nesting_item(index, parent, random % 4 != 0))
                })
                .collect();
            let records = nesting_requests(items.len(), seed);
            let requests: Vec<_> = records.iter().collect();
            let nesting = ItemNesting::new(&items);
            let by_id: HashMap<_, _> = items
                .iter()
                .map(|item| (item.id.as_str(), item.as_ref()))
                .collect();
            let mut roots = Vec::new();
            let mut nested: HashMap<&str, Vec<&Item>> = HashMap::new();
            for item in items.iter().map(Cow::as_ref) {
                if let Some(parent) = scan_parent(item, &by_id) {
                    nested.entry(parent).or_default().push(item);
                    assert_eq!(nesting.parents.get(item.id.as_str()), Some(&parent));
                } else {
                    roots.push(item);
                    assert!(!nesting.parents.contains_key(item.id.as_str()));
                }
            }
            assert_eq!(nesting.roots, roots);
            assert_eq!(nesting.children, nested);
            let indexed = TurnRequestIndex::new(&requests, &nesting);
            for item in &items {
                assert_eq!(
                    indexed.approvals.contains(item.id.as_str()),
                    requests.iter().any(|request| {
                        request.kind == RequestKind::Approval
                            && request.item_id.as_ref() == Some(&item.id)
                    })
                );
            }
            for (level, siblings) in std::iter::once((0, roots.as_slice()))
                .chain(nested.values().map(|items| (1, items.as_slice())))
            {
                let expected: Vec<_> = requests
                    .iter()
                    .copied()
                    .filter(|record| {
                        let belongs = record.item_id.as_ref().and_then(|id| {
                            nested
                                .iter()
                                .find(|(_, children)| children.iter().any(|item| &item.id == id))
                                .map(|(parent, _)| *parent)
                        });
                        let parent = siblings
                            .first()
                            .and_then(|item| item.presentation.parent_id.as_deref());
                        belongs == parent || (belongs.is_none() && level == 0)
                    })
                    .collect();
                assert_eq!(indexed.for_items(siblings, level), expected);
            }
        }
    }

    #[test]
    fn deep_nesting_keeps_the_existing_bounded_projection_order() {
        let mut items: Vec<Cow<'_, Item>> = (0..180usize)
            .map(|index| {
                Cow::Owned(nesting_item(
                    index,
                    index.checked_sub(1).map(|parent| format!("item-{parent}")),
                    true,
                ))
            })
            .collect();
        // Two siblings start the depth-limited fallback, which keeps the
        // historical reverse sibling order and does not insert controls.
        items[66].to_mut().presentation.parent_id = Some("item-64".into());
        let records = nesting_requests(items.len(), 3);
        let requests: Vec<_> = records.iter().collect();
        let options = ProjectionOptions {
            mode: DisplayMode::Verbose,
            ..ProjectionOptions::default()
        };
        let rows = verbose_rows(&items, &HashMap::new(), &requests, "turn", &options);
        let fallback = &find(&rows, "item-64").unwrap().children;
        assert_eq!(fallback.first().unwrap().id, "item-66");
        assert_eq!(fallback.last().unwrap().id, "item-65");
        assert!(
            fallback
                .iter()
                .all(|row| row.kind == RowKind::Subagent && row.children.is_empty())
        );
        assert_eq!(fallback.len(), 115);
    }

    #[test]
    fn activity_counts_keep_raw_parent_and_mixed_tool_semantics() {
        use super::super::model::{ChangeKind, FileChange, Semantics};
        let mut items = vec![
            nesting_item(0, None, true),
            nesting_item(1, Some("item-0".into()), true),
            nesting_item(2, Some("item-2".into()), true),
            nesting_item(3, None, false),
        ];
        for (index, paths) in [(4, vec!["a", "b"]), (5, vec!["a"])] {
            let mut item = nesting_item(index, None, false);
            item.body = ItemBody::FileChange {
                changes: paths
                    .into_iter()
                    .map(|path| FileChange {
                        path: path.into(),
                        kind: ChangeKind::Modify,
                        diff: None,
                    })
                    .collect(),
            };
            items.push(item);
        }
        for (index, kind) in [
            ToolKind::Read,
            ToolKind::Search,
            ToolKind::List,
            ToolKind::WebSearch,
            ToolKind::WebFetch,
            ToolKind::Mcp,
            ToolKind::Mcp,
            ToolKind::Other,
        ]
        .into_iter()
        .enumerate()
        {
            let mut item = nesting_item(10 + index, None, false);
            item.body = ItemBody::ToolCall {
                server: None,
                tool: "tool".into(),
                input: Value::Null,
                output: None,
            };
            item.presentation.semantics = Some(Semantics {
                kind,
                ..Semantics::default()
            });
            items.push(item);
        }
        let work: Vec<_> = items.iter().collect();
        assert_eq!(
            activity_title(&work, false, Some(65000), None),
            "Worked 1m 5s · 2 files edited · 1 command · 5 reads · 4 tool calls"
        );
        assert_eq!(
            activity_title(&work, true, Some(65000), Some("reading")),
            "Working 1m 5s · reading"
        );
        let mcp: Vec<_> = items
            .iter()
            .filter(|item| row_kind(item) == RowKind::Mcp)
            .collect();
        assert_eq!(
            activity_title(&mcp, false, None, None),
            "Worked · 2 MCP calls"
        );
        assert_eq!(
            activity_title(&mcp[..1], false, None, None),
            "Worked · 1 MCP call"
        );
    }

    #[test]
    #[ignore = "slow: wall-clock activity title benchmark"]
    fn nested_activity_title_performance() {
        use std::time::Instant;
        for count in [12usize, 64, 128, 2000] {
            let items: Vec<_> = (0..count)
                .map(|index| {
                    nesting_item(
                        index,
                        index.checked_sub(1).map(|parent| format!("item-{parent}")),
                        true,
                    )
                })
                .collect();
            let work: Vec<_> = items.iter().collect();
            let mut samples = Vec::new();
            for _ in 0..7 {
                let start = Instant::now();
                std::hint::black_box(activity_title(&work, false, None, None));
                samples.push(start.elapsed());
            }
            assert_eq!(
                activity_title(&work, false, None, None),
                "Worked · 1 tool call"
            );
            samples.sort();
            eprintln!("activity title ({count} nested items): {:?}", samples[3]);
        }
    }

    #[test]
    #[ignore = "slow: wall-clock dense-turn projection benchmark"]
    fn dense_turn_nesting_and_request_performance() {
        use std::time::Instant;
        for (name, count, shape) in [
            ("typical", 12usize, 0),
            ("dense approvals", 2000, 0),
            ("deep chain", 1500, 1),
            ("wide nesting", 400, 2),
        ] {
            let items: Vec<_> = (0..count)
                .map(|index| {
                    let (parent, subagent) = match shape {
                        1 => (
                            index.checked_sub(1).map(|parent| format!("item-{parent}")),
                            true,
                        ),
                        2 => (
                            (index % 2 == 1).then(|| format!("item-{}", index - 1)),
                            index % 2 == 0,
                        ),
                        _ => (None, false),
                    };
                    Cow::Owned(nesting_item(index, parent, subagent))
                })
                .collect();
            let records = nesting_requests(count, 0);
            let requests: Vec<_> = records.iter().collect();
            let options = ProjectionOptions {
                mode: DisplayMode::Verbose,
                ..ProjectionOptions::default()
            };
            let orders = HashMap::new();
            let mut samples = Vec::new();
            for _ in 0..7 {
                let start = Instant::now();
                std::hint::black_box(verbose_rows(&items, &orders, &requests, "turn", &options));
                samples.push(start.elapsed());
            }
            samples.sort();
            eprintln!("{name} ({count} items): {:?}", samples[3]);
        }
    }

    #[test]
    #[ignore = "slow: wall-clock projection benchmark"]
    fn approval_heavy_history_projection_performance() {
        use super::super::model::ChatEvent;
        use std::time::Instant;

        let mut transcript = Transcript::default();
        for turn in 0..1500 {
            for call in 0..4 {
                let item = serde_json::from_value(serde_json::json!({
                    "id": format!("call-{turn}-{call}"),
                    "turn_id": format!("turn-{turn}"),
                    "status": "completed",
                    "body": {"type": "command", "command": "cargo test"}
                }))
                .unwrap();
                transcript.apply(&ChatEvent::ItemCompleted { item });
            }
            transcript.requests.push(RequestRecord {
                request_id: format!("request-{turn}"),
                turn_id: Some(format!("turn-{turn}")),
                item_id: Some(format!("call-{turn}-0")),
                requested_seq: turn * 4 + 2,
                ..RequestRecord::default()
            });
        }
        let mut cache = ProjectionCache::default();
        let options = ProjectionOptions::default();
        let clean = HashSet::new();
        cache.project(&transcript, &options, &clean);
        let started = Instant::now();
        for _ in 0..5 {
            let rows = cache.project(&transcript, &options, &clean);
            let requests: Vec<_> = rows
                .iter()
                .filter_map(|row| row.request_id.as_deref())
                .collect();
            assert_eq!(requests.len(), 1500);
            for (turn, request) in requests.iter().enumerate() {
                assert_eq!(*request, format!("request-{turn}"));
            }
            std::hint::black_box(rows);
        }
        let per_projection_ms = started.elapsed().as_secs_f64() * 1000.0 / 5.0;
        eprintln!(
            "approval-heavy projection: 6000 items, 1500 requests, {per_projection_ms:.3}ms/cached projection"
        );
        assert_eq!(cache.projected_turns, 1500);
    }

    /// Exercise the same cache entry point used by desktop on a transcript whose
    /// outputs and inline image payloads are much larger than its row headers.
    /// Keep this enabled in CI: the limits allow slow debug runners, while the
    /// structural checks below catch accidentally projecting history per token.
    #[test]
    fn large_transcript_streaming_and_disclosure_performance() {
        use super::super::model::{ChatEvent, ChatImage, Delta, ImageSource, Presentation};
        use std::hint::black_box;
        use std::time::Instant;

        let mut transcript = Transcript::default();
        let output = "src/model.rs:12: matching output line\n".repeat(1400);
        for turn in 0..96 {
            for call in 0..24 {
                let mut presentation = Presentation::default();
                if call == 0 {
                    presentation.images.push(ChatImage {
                        label: "screenshot.png".into(),
                        source: ImageSource::Data {
                            mime: "image/png".into(),
                            base64: "A".repeat(128 * 1024),
                        },
                    });
                }
                transcript.apply(&ChatEvent::ItemCompleted {
                    item: Item {
                        id: format!("call-{turn}-{call}"),
                        turn_id: Some(format!("turn-{turn}")),
                        status: ItemStatus::Completed,
                        started_at: Some(1000),
                        ended_at: Some(2000),
                        presentation,
                        body: ItemBody::Command {
                            command: format!("cat src/file-{call}.rs"),
                            cwd: None,
                            output: output.clone(),
                            exit_code: Some(0),
                        },
                    },
                });
            }
        }
        transcript.apply(&ChatEvent::TurnStarted {
            turn_id: "live".into(),
            started_at: Some(3000),
        });
        transcript.apply(&ChatEvent::ItemStarted {
            item: Item {
                id: "stream".into(),
                turn_id: Some("live".into()),
                status: ItemStatus::InProgress,
                started_at: Some(3000),
                ended_at: None,
                presentation: Presentation::default(),
                body: ItemBody::Command {
                    command: "cargo test".into(),
                    cwd: None,
                    output: output.clone(),
                    exit_code: None,
                },
            },
        });
        let mut cache = ProjectionCache::default();
        let mut options = ProjectionOptions {
            now_ms: Some(4000),
            ..ProjectionOptions::default()
        };
        let clean = HashSet::new();
        assert!(
            prepare_items(transcript.items.iter(), &options)
                .iter()
                .all(|item| matches!(item, Cow::Borrowed(_))),
            "folded items must project without copying output or image payloads"
        );
        let started = Instant::now();
        let mut rows = cache.project(&transcript, &options, &clean);
        let cold_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(cache.projected_turns, 97);

        let started = Instant::now();
        for _ in 0..120 {
            transcript.apply(&ChatEvent::ItemDelta {
                item_id: "stream".into(),
                delta: Delta::Output("token ".into()),
            });
            rows = cache.project(&transcript, &options, &clean);
            black_box(&rows);
        }
        let event_ms = started.elapsed().as_secs_f64() * 1000.0 / 120.0;
        assert_eq!(cache.projected_turns, 97, "tokens never regroup history");

        let started = Instant::now();
        for index in 0..40 {
            options.disclosure.toggle("activity:turn-0", index % 2 != 0);
            rows = cache.project(&transcript, &options, &clean);
            black_box(&rows);
        }
        let disclosure_ms = started.elapsed().as_secs_f64() * 1000.0 / 40.0;
        assert_eq!(cache.projected_turns, 97, "disclosures reuse headers");

        let started = Instant::now();
        for index in 0..24 {
            options.mode = if index % 2 == 0 {
                DisplayMode::Verbose
            } else {
                DisplayMode::Normal
            };
            options.disclosure.reset_mode();
            rows = cache.project(&transcript, &options, &clean);
            black_box(&rows);
        }
        let mode_ms = started.elapsed().as_secs_f64() * 1000.0 / 24.0;
        assert_eq!(
            cache.projected_turns, 194,
            "each mode projects once; repeated switches reuse both variants"
        );

        let started = Instant::now();
        for index in 0..20 {
            options
                .disclosure
                .reset_bulk_for_rows(index % 2 == 0, options.mode, &rows);
            rows = cache.project(&transcript, &options, &clean);
            black_box(&rows);
        }
        let bulk_ms = started.elapsed().as_secs_f64() * 1000.0 / 20.0;
        assert_eq!(cache.projected_turns, 194, "bulk disclosure reuses headers");
        eprintln!(
            "tool-ui perf: {} items, cold={cold_ms:.3}ms event={event_ms:.3}ms disclosure={disclosure_ms:.3}ms mode={mode_ms:.3}ms expand_all={bulk_ms:.3}ms",
            transcript.items.len()
        );
        assert!(cold_ms < 1000.0, "cold projection took {cold_ms:.3}ms");
        for (scenario, per_event_ms) in [
            ("streaming", event_ms),
            ("disclosure", disclosure_ms),
            ("mode switch", mode_ms),
            ("expand all", bulk_ms),
        ] {
            assert!(
                per_event_ms < 60.0,
                "{scenario} regressed to {per_event_ms:.3}ms per event on 2305 items"
            );
        }
    }
}
