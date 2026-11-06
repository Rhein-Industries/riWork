//! Lazy, bounded normalization for transcript search. The owning view invalidates
//! touched item positions after folding events, including replacements and deltas.
use std::collections::HashMap;

use riwork_engine::chat::model::Item;

use super::{
    dto::SearchHitDto,
    view::{item_label, item_source},
};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_ITEMS: usize = 4096;

#[derive(Default)]
pub(super) struct SearchIndex {
    lower: HashMap<usize, String>,
    bytes: usize,
    #[cfg(test)]
    builds: usize,
}

impl SearchIndex {
    pub(super) fn invalidate(&mut self, positions: impl Iterator<Item = usize>) {
        for position in positions {
            if let Some(text) = self.lower.remove(&position) {
                self.bytes -= text.capacity();
            }
        }
    }

    pub(super) fn search(&mut self, items: &[Item], query: &str) -> Vec<SearchHitDto> {
        self.search_while(items, query, || true).unwrap()
    }

    /// Partial normalization remains reusable, but superseded work returns no
    /// partial hit list. Cancellation is checked between transcript items.
    pub(super) fn search_while(
        &mut self,
        items: &[Item],
        query: &str,
        mut current: impl FnMut() -> bool,
    ) -> Option<Vec<SearchHitDto>> {
        if !current() {
            return None;
        }
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return current().then(Vec::new);
        }
        let mut hits = Vec::new();
        for (position, item) in items.iter().enumerate() {
            if !current() {
                return None;
            }
            let mut source = None;
            let offset = if let Some(lower) = self.lower.get(&position) {
                match_offset(lower, &query)
            } else {
                let text = item_source(item).prepared();
                let lower = text.lowercase();
                #[cfg(test)]
                {
                    self.builds += 1;
                }
                let offset = match_offset(&lower, &query);
                // Do not evict while traversing: a history larger than the budget
                // must not churn the whole cache on every subsequent search.
                if self.lower.len() < MAX_ITEMS && lower.capacity() <= MAX_BYTES - self.bytes {
                    self.bytes += lower.capacity();
                    self.lower.insert(position, lower);
                }
                if offset.is_some() {
                    source = Some(text);
                }
                offset
            };
            if !current() {
                return None;
            }
            let Some(offset) = offset else {
                continue;
            };
            let text = source.unwrap_or_else(|| item_source(item));
            let preview = text.excerpt(offset, 200);
            hits.push(SearchHitDto {
                id: item.id.clone(),
                label: item_label(item).into(),
                preview,
            });
            if hits.len() == 200 {
                break;
            }
        }
        current().then_some(hits)
    }
}

fn match_offset(lower: &str, query: &str) -> Option<usize> {
    // Preserve the existing excerpt offset, including expanding Unicode lowercase
    // mappings. The cache contains exactly str::to_lowercase's original result.
    lower
        .find(query)
        .map(|at| lower[..at].chars().count().saturating_sub(48))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{
        dto::wire_name,
        view::{ChatView, system_clock},
    };
    use riwork_engine::chat::lineage;
    use riwork_engine::chat::{
        model::{ChatEvent, Delta, ItemBody, ItemStatus},
        projection::DisplayMode,
        wire::Envelope,
    };

    fn item(id: usize, body: ItemBody) -> Item {
        Item {
            id: id.to_string(),
            turn_id: None,
            status: ItemStatus::Completed,
            presentation: Default::default(),
            started_at: None,
            ended_at: None,
            body,
        }
    }

    fn previous_item_text(item: &Item) -> String {
        if let Some(mark) = lineage::of(item) {
            return mark.label();
        }
        match &item.body {
            ItemBody::UserMessage { text } => previous_typed_text(text),
            ItemBody::AgentMessage { text }
            | ItemBody::Reasoning { text }
            | ItemBody::Notice { text, .. } => text.clone(),
            ItemBody::Command {
                command, output, ..
            } => format!("$ {command}\n\n{output}"),
            ItemBody::FileChange { changes } => changes
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
            ItemBody::Plan { explanation, steps } => format!(
                "{}\n{}",
                explanation.as_deref().unwrap_or_default(),
                steps
                    .iter()
                    .map(|step| format!("[{}] {}", wire_name(&step.status), step.text))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            ItemBody::Todo { items } => items
                .iter()
                .map(|step| format!("[{}] {}", wire_name(&step.status), step.text))
                .collect::<Vec<_>>()
                .join("\n"),
            ItemBody::ToolCall { input, output, .. } => format!(
                "{}\n\n{}",
                serde_json::to_string_pretty(input).unwrap_or_default(),
                output.as_deref().unwrap_or_default()
            ),
            ItemBody::WebSearch { query } => query.clone(),
            ItemBody::Compaction => String::new(),
        }
    }

    fn previous_typed_text(text: &str) -> String {
        text.lines()
            .filter(|line| {
                let line = line.trim();
                !(line.starts_with("[Attached: ") && line.ends_with(" bytes)]"))
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned()
    }

    fn previous_search(items: &[Item], query: &str) -> Vec<SearchHitDto> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        items
            .iter()
            .filter_map(|item| {
                let text = previous_item_text(item);
                let lower = text.to_lowercase();
                let at = lower.find(&query)?;
                let offset = lower[..at].chars().count().saturating_sub(48);
                let preview = text
                    .chars()
                    .skip(offset)
                    .take(200)
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(SearchHitDto {
                    id: item.id.clone(),
                    label: item_label(item).into(),
                    preview,
                })
            })
            .take(200)
            .collect()
    }

    #[test]
    fn cached_search_preserves_unicode_previews_order_and_limit() {
        let mut items = vec![
            item(
                0,
                ItemBody::AgentMessage {
                    text: format!(
                        "{} İSTANBUL ΟΣ ΣΟΣ café 🦀\nneedle\tend",
                        "prefix ".repeat(20)
                    ),
                },
            ),
            item(
                1,
                ItemBody::UserMessage {
                    text: "typed NEEDLE\n[Attached: screenshot.png (100 bytes)]".into(),
                },
            ),
            item(
                2,
                ItemBody::Command {
                    command: "echo Needle".into(),
                    cwd: None,
                    output: "result NEEDLE".into(),
                    exit_code: Some(0),
                },
            ),
            item(
                3,
                ItemBody::ToolCall {
                    server: None,
                    tool: "read".into(),
                    input: serde_json::json!({"path":"Needle.rs"}),
                    output: Some("tool output".into()),
                },
            ),
        ];
        items.extend((4..240).map(|i| {
            item(
                i,
                ItemBody::AgentMessage {
                    text: "NEEDLE".into(),
                },
            )
        }));
        let mut index = SearchIndex::default();
        for query in [
            "needle",
            "ΟΣ",
            "i\u{307}",
            "CAFÉ",
            "🦀",
            "missing",
            " screenshot.png ",
            " ",
        ] {
            assert_eq!(
                index.search(&items, query),
                previous_search(&items, query),
                "{query:?}"
            );
        }
        let built = index.builds;
        assert_eq!(index.search(&items, "needle").len(), 200);
        assert_eq!(
            index.builds, built,
            "unchanged history must not be normalized again"
        );
    }

    #[test]
    fn cancelled_search_stops_between_items_and_keeps_valid_normalization() {
        let items: Vec<_> = (0..4)
            .map(|i| {
                item(
                    i,
                    ItemBody::AgentMessage {
                        text: "needle".into(),
                    },
                )
            })
            .collect();
        let mut index = SearchIndex::default();
        assert_eq!(index.search_while(&items, "needle", || false), None);
        assert_eq!(index.builds, 0);
        let mut checks = 0;
        assert_eq!(
            index.search_while(&items, "needle", || {
                checks += 1;
                checks < 3
            }),
            None
        );
        assert_eq!(
            index.builds, 1,
            "cancellation must stop before the next item"
        );
        assert_eq!(
            index.search(&items, "needle"),
            previous_search(&items, "needle")
        );
        assert_eq!(
            index.builds,
            items.len(),
            "the valid prefix remains reusable"
        );
        let mut checks = 0;
        assert_eq!(
            index.search_while(&items, " ", || {
                checks += 1;
                checks < 2
            }),
            None
        );
    }

    #[test]
    fn normalization_cache_is_bounded_and_invalidations_release_its_budget() {
        let items: Vec<_> = (0..MAX_ITEMS + 1)
            .map(|i| {
                item(
                    i,
                    ItemBody::AgentMessage {
                        text: "tiny".into(),
                    },
                )
            })
            .collect();
        let mut index = SearchIndex::default();
        assert!(index.search(&items, "absent").is_empty());
        assert_eq!(index.lower.len(), MAX_ITEMS);
        index.invalidate(0..items.len());
        assert_eq!((index.bytes, index.lower.len()), (0, 0));
        let huge = vec![item(
            0,
            ItemBody::AgentMessage {
                text: "x".repeat(MAX_BYTES + 1),
            },
        )];
        assert!(index.search(&huge, "absent").is_empty());
        assert!(index.lower.is_empty());
        assert_eq!(index.bytes, 0);
    }

    #[test]
    fn folded_deltas_replacements_and_duplicate_events_keep_search_current() {
        let mut view = ChatView::new("search-test", DisplayMode::Normal, system_clock());
        let mut original = item(
            0,
            ItemBody::AgentMessage {
                text: "first".into(),
            },
        );
        original.status = ItemStatus::InProgress;
        let events = [
            ChatEvent::ItemStarted { item: original },
            ChatEvent::ItemDelta {
                item_id: "0".into(),
                delta: Delta::Text(" second İ".into()),
            },
            ChatEvent::ItemCompleted {
                item: item(
                    0,
                    ItemBody::AgentMessage {
                        text: "replacement ΟΣ".into(),
                    },
                ),
            },
            ChatEvent::ItemCompleted {
                item: item(
                    1,
                    ItemBody::AgentMessage {
                        text: "second appended".into(),
                    },
                ),
            },
        ];
        for (seq, event) in events.into_iter().enumerate() {
            let envelope = Envelope {
                chat_id: "search-test".into(),
                seq: seq as u64 + 1,
                event,
            };
            view.apply(std::slice::from_ref(&envelope));
            for query in ["first", "second", "i\u{307}", "ΟΣ", "replacement"] {
                let expected = previous_search(&view.fold.transcript.items, query);
                assert_eq!(view.search(query), expected);
            }
            view.apply(std::slice::from_ref(&envelope));
            assert_eq!(
                view.search("second"),
                previous_search(&view.fold.transcript.items, "second")
            );
        }
    }

    #[test]
    #[ignore = "manual paired transcript-search performance measurement"]
    fn repeated_search_benchmark() {
        let items: Vec<_> = (0..300)
            .map(|i| {
                item(
                    i,
                    ItemBody::ToolCall {
                        server: None,
                        tool: "read".into(),
                        input: serde_json::json!({"path":format!("file{i}.rs")}),
                        output: Some("Héllo content without the requested token.\n".repeat(400)),
                    },
                )
            })
            .collect();
        let query = "never-present";
        let start = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(previous_search(&items, query));
        }
        let previous = start.elapsed();
        let mut index = SearchIndex::default();
        let start = std::time::Instant::now();
        assert_eq!(index.search(&items, query), previous_search(&items, query));
        let cold_and_check = start.elapsed();
        let start = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(index.search(&items, query));
        }
        let cached = start.elapsed();
        assert_eq!(index.builds, items.len());
        eprintln!(
            "search: {} cached bytes; 20 queries previous {previous:?}, cached {cached:?}; cold plus parity {cold_and_check:?}",
            index.bytes
        );
    }
    #[test]
    fn borrowed_and_composite_sources_preserve_every_body_and_unicode_excerpt() {
        use riwork_engine::chat::model::{
            ChangeKind, FileChange, LineageContext, LineageKind, Step, StepStatus,
        };
        let text = " \tNEEDLE İ ΟΣ ΣΟΣ Σ'ΟΣ 🦀\u{2003}\nend ".repeat(9);
        let steps: Vec<_> = [
            StepStatus::Pending,
            StepStatus::InProgress,
            StepStatus::Completed,
        ]
        .into_iter()
        .map(|status| Step {
            status,
            text: text.clone(),
        })
        .collect();
        let bodies = vec![
            ItemBody::AgentMessage { text: text.clone() },
            ItemBody::Reasoning { text: text.clone() },
            ItemBody::UserMessage { text: text.clone() },
            ItemBody::Command {
                command: "echo ΟΣ".into(),
                output: text.clone(),
                cwd: None,
                exit_code: None,
            },
            ItemBody::Command {
                command: "".into(),
                output: "".into(),
                cwd: None,
                exit_code: Some(0),
            },
            ItemBody::ToolCall {
                server: None,
                tool: "read".into(),
                input: serde_json::json!({"text":text,"nested":[null,true,42,"\r\n\t"]}),
                output: Some(text.clone()),
            },
            ItemBody::ToolCall {
                server: Some("mcp".into()),
                tool: "empty".into(),
                input: serde_json::Value::Null,
                output: None,
            },
            ItemBody::FileChange {
                changes: [
                    ChangeKind::Add,
                    ChangeKind::Modify,
                    ChangeKind::Delete,
                    ChangeKind::Rename,
                ]
                .into_iter()
                .enumerate()
                .map(|(i, kind)| FileChange {
                    kind,
                    path: format!("{text}/{i}"),
                    diff: (i % 2 == 0).then(|| text.clone()),
                })
                .collect(),
            },
            ItemBody::FileChange { changes: vec![] },
            ItemBody::Plan {
                explanation: Some(text.clone()),
                steps: steps.clone(),
            },
            ItemBody::Plan {
                explanation: None,
                steps: vec![],
            },
            ItemBody::Todo { items: steps },
            ItemBody::Todo { items: vec![] },
            ItemBody::WebSearch {
                query: text.clone(),
            },
            serde_json::from_value(
                serde_json::json!({"type":"notice","level":"warning","text":text}),
            )
            .unwrap(),
            ItemBody::Compaction,
        ];
        let mut items: Vec<_> = bodies
            .into_iter()
            .enumerate()
            .map(|(id, body)| item(id, body))
            .collect();
        items.push(lineage::item(lineage::LineageMark {
            kind: LineageKind::Fork,
            target_id: "next-chat".into(),
            target_title: text.clone(),
            provider: Some("codex".into()),
            model: None,
            at: None,
            context: LineageContext::Native,
            noted_at: 0,
        }));
        let fragments = [
            "",
            "plain",
            " İΣ 🦀 ",
            "\r",
            "\n",
            "\r\n",
            "\u{2003}",
            " [Attached: file (10 bytes)] ",
            "[Attached: incomplete",
            "bad bytes)]",
        ];
        let mut seed = 19usize;
        for id in 0..256 {
            let mut text = String::new();
            for _ in 0..8 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                text.push_str(fragments[(seed >> 8) % fragments.len()]);
            }
            items.push(item(id + 100, ItemBody::UserMessage { text }));
        }
        for item in &items {
            let expected = previous_item_text(item);
            let source = item_source(item);
            assert_eq!(
                source.lowercase(),
                expected.to_lowercase(),
                "lowercase {}",
                item.id
            );
            assert_eq!(
                crate::chat::view::item_text(item),
                expected,
                "copy {}",
                item.id
            );
            for offset in 0..expected.chars().count() + 2 {
                for limit in [0, 1, 5, 140, 200] {
                    let preview = expected
                        .chars()
                        .skip(offset)
                        .take(limit)
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    assert_eq!(
                        source.excerpt(offset, limit),
                        preview,
                        "{} offset {offset} limit {limit}",
                        item.id
                    );
                }
            }
        }
        let mut index = SearchIndex::default();
        for query in [
            "needle", "i\u{307}", "ος", "οσ", "Σ'ΟΣ", "🦀", "pending", "null", "attached",
            "missing", " ",
        ] {
            let expected = previous_search(&items, query);
            assert_eq!(index.search(&items, query), expected, "cold {query}");
            assert_eq!(index.search(&items, query), expected, "warm {query}");
        }
    }

    /// The prior borrowed-source implementation, including its full warm JSON
    /// formatting and scalar character skips, with identical cache admission.
    fn cached_before(index: &mut SearchIndex, items: &[Item], query: &str) -> Vec<SearchHitDto> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return vec![];
        }
        let mut hits = Vec::new();
        for (position, item) in items.iter().enumerate() {
            let mut source = None;
            let offset = if let Some(lower) = index.lower.get(&position) {
                match_offset(lower, &query)
            } else {
                let text = item_source(item).prepared();
                let lower = text.lowercase();
                index.builds += 1;
                let offset = match_offset(&lower, &query);
                if index.lower.len() < MAX_ITEMS && lower.capacity() <= MAX_BYTES - index.bytes {
                    index.bytes += lower.capacity();
                    index.lower.insert(position, lower);
                }
                if offset.is_some() {
                    source = Some(text);
                }
                offset
            };
            if let Some(offset) = offset {
                let text = source.unwrap_or_else(|| item_source(item).prepared());
                let preview = text.excerpt_before(offset, 200);
                hits.push(SearchHitDto {
                    id: item.id.clone(),
                    label: item_label(item).into(),
                    preview,
                });
                if hits.len() == 200 {
                    break;
                }
            }
        }
        hits
    }

    #[test]
    #[ignore = "manual paired source/excerpt allocation performance measurement"]
    fn source_allocation_benchmark() {
        use std::{hint::black_box, time::Instant};
        fn history(kind: &str, count: usize, bytes: usize, late: bool) -> Vec<Item> {
            (0..count)
                .map(|id| {
                    let fill = if kind.starts_with("unicode_") {
                        "Ος İ content 🦀 "
                    } else {
                        "Ordinary ASCII payload. "
                    };
                    let padding = fill.repeat(bytes / fill.len());
                    let text = if late {
                        format!("{padding} needle")
                    } else {
                        format!("needle {padding}")
                    };
                    let kind = if kind == "mixed" {
                        ["agent", "user", "command", "tool"][id % 4]
                    } else {
                        kind
                    };
                    item(
                        id,
                        match kind {
                            "agent" | "unicode_agent" => ItemBody::AgentMessage { text },
                            "user" => ItemBody::UserMessage { text },
                            "command" => ItemBody::Command {
                                command: "echo result".into(),
                                cwd: None,
                                output: text,
                                exit_code: Some(0),
                            },
                            "input_tool" => ItemBody::ToolCall {
                                server: None,
                                tool: "read".into(),
                                input: serde_json::json!({"text":text}),
                                output: None,
                            },
                            _ => ItemBody::ToolCall {
                                server: None,
                                tool: "read".into(),
                                input: serde_json::json!({"path":"file.rs"}),
                                output: Some(text),
                            },
                        },
                    )
                })
                .collect()
        }
        for (name, kind, count, bytes, late, repetitions) in [
            ("typical", "mixed", 12, 256, false, 100),
            ("large_agent", "agent", 128, 48 * 1024, false, 10),
            ("large_user", "user", 128, 48 * 1024, false, 10),
            ("large_command", "command", 128, 48 * 1024, false, 10),
            ("large_tool", "tool", 128, 48 * 1024, false, 10),
            ("unicode_tool", "unicode_tool", 48, 32 * 1024, false, 3),
            ("large_input_tool", "input_tool", 32, 48 * 1024, false, 3),
            ("late_agent", "agent", 64, 48 * 1024, true, 5),
            (
                "late_unicode_agent",
                "unicode_agent",
                64,
                48 * 1024,
                true,
                5,
            ),
            ("late_input_tool", "input_tool", 32, 48 * 1024, true, 3),
            ("overflow_input", "input_tool", 240, 48 * 1024, false, 2),
            ("overflow", "mixed", 240, 48 * 1024, false, 5),
        ] {
            let items = history(kind, count, bytes, late);
            for (phase, query, warm) in [
                ("cold_hit", "needle", false),
                ("cold_miss", "absent-token", false),
                ("warm_hit", "needle", true),
                ("warm_miss", "absent-token", true),
            ] {
                let mut before_times = Vec::new();
                let mut after_times = Vec::new();
                for _ in 0..3 {
                    let mut before = SearchIndex::default();
                    let mut after = SearchIndex::default();
                    if warm {
                        assert_eq!(
                            cached_before(&mut before, &items, query),
                            after.search(&items, query)
                        );
                        assert_eq!(
                            (before.bytes, before.lower.len()),
                            (after.bytes, after.lower.len())
                        );
                    }
                    let start = Instant::now();
                    for _ in 0..repetitions {
                        if !warm {
                            before = SearchIndex::default();
                        }
                        black_box(cached_before(&mut before, &items, query));
                    }
                    before_times.push(start.elapsed().as_secs_f64() * 1e6 / repetitions as f64);
                    let start = Instant::now();
                    for _ in 0..repetitions {
                        if !warm {
                            after = SearchIndex::default();
                        }
                        black_box(after.search(&items, query));
                    }
                    after_times.push(start.elapsed().as_secs_f64() * 1e6 / repetitions as f64);
                    assert_eq!(
                        cached_before(&mut before, &items, query),
                        after.search(&items, query)
                    );
                    assert_eq!(
                        (before.bytes, before.lower.len(), before.builds),
                        (after.bytes, after.lower.len(), after.builds)
                    );
                }
                let mut sorted_before = before_times.clone();
                sorted_before.sort_by(f64::total_cmp);
                let mut sorted_after = after_times.clone();
                sorted_after.sort_by(f64::total_cmp);
                eprintln!(
                    "SOURCE_BENCH {}",
                    serde_json::json!({"fixture":name,"phase":phase,"items":count,"payload_bytes":bytes,"queries_per_sample":repetitions,"before_us":before_times,"after_us":after_times,"before_median_us":sorted_before[1],"after_median_us":sorted_after[1]})
                );
            }
        }
    }
    #[test]
    fn lazy_json_prefix_matches_canonical_pretty_formatting_and_utf8_windows() {
        use crate::chat::view::write_json_prefix;
        fn value(seed: &mut u64, depth: usize) -> serde_json::Value {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let choice = (*seed >> 32) as usize;
            match if depth == 0 { choice % 5 } else { choice % 7 } {
                0 => serde_json::Value::Null,
                1 => serde_json::json!(choice % 2 == 0),
                2 => serde_json::json!((choice as f64 - 2_000_000_000.0) / 7.0),
                3 => serde_json::json!(
                    (0u8..32).map(char::from).collect::<String>() + "\"\\/ ΟΣ İ 🦀\u{2028}\u{2029}"
                ),
                4 => serde_json::json!("prefix İ 🦀 ".repeat(choice % 160)),
                5 => serde_json::Value::Array(
                    (0..choice % 5).map(|_| value(seed, depth - 1)).collect(),
                ),
                _ => serde_json::Value::Object(
                    (0..choice % 5)
                        .map(|index| (format!("key {index} 🦀\n\""), value(seed, depth - 1)))
                        .collect(),
                ),
            }
        }
        let mut seed = 91;
        let mut values: Vec<_> = (0..128).map(|_| value(&mut seed, 4)).collect();
        values.extend([
            serde_json::json!(f64::MAX),
            serde_json::json!(f64::MIN_POSITIVE),
            serde_json::json!(-0.0),
        ]);
        for value in &values {
            let mut bytes = Vec::new();
            write_json_prefix(
                value,
                &mut serde_json::ser::PrettyFormatter::new(),
                &mut bytes,
            )
            .unwrap();
            assert_eq!(
                bytes,
                serde_json::to_string_pretty(value).unwrap().as_bytes()
            );
        }
        let text = "a\0\n\r\t\"\\ İ ΟΣ 🦀\u{2003}".repeat(1200);
        let item = item(
            0,
            ItemBody::ToolCall {
                server: None,
                tool: "read".into(),
                input: serde_json::json!({"nested":[values, text], "tail": "needle"}),
                output: Some("\n after JSON ΟΣ 🦀".repeat(500)),
            },
        );
        let expected = previous_item_text(&item);
        let source = item_source(&item);
        let len = expected.chars().count();
        for offset in [
            0,
            1,
            47,
            199,
            200,
            255,
            256,
            511,
            1023,
            1024,
            2047,
            4095,
            4096,
            8191,
            8192,
            8193,
            len - 100,
            len,
            len + 1,
        ] {
            for limit in [0, 1, 200, 801] {
                let preview = expected
                    .chars()
                    .skip(offset)
                    .take(limit)
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                assert_eq!(
                    source.excerpt(offset, limit),
                    preview,
                    "offset {offset} limit {limit}"
                );
            }
        }
    }

    #[test]
    fn json_prefix_stops_before_escaping_an_entire_large_string() {
        use crate::chat::view::write_json_prefix;
        struct Limited {
            remaining: usize,
            largest: usize,
            supplied: usize,
        }
        impl std::io::Write for Limited {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                std::str::from_utf8(bytes).expect("formatter writes whole UTF-8 chunks");
                self.largest = self.largest.max(bytes.len());
                self.supplied += bytes.len();
                if bytes.len() >= self.remaining {
                    return Err(std::io::Error::other("complete"));
                }
                self.remaining -= bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for text in [
            "x".repeat(1024 * 1024),
            "\0".repeat(1024 * 1024),
            "🦀".repeat(256 * 1024),
        ] {
            let mut writer = Limited {
                remaining: 200,
                largest: 0,
                supplied: 0,
            };
            assert!(
                write_json_prefix(
                    &serde_json::json!(text),
                    &mut serde_json::ser::PrettyFormatter::new(),
                    &mut writer
                )
                .is_err()
            );
            assert!(
                writer.largest <= 256 * 6,
                "only one bounded escaped string chunk is built"
            );
            assert!(writer.supplied <= 1 + 256 * 6);
        }
    }

    #[test]
    fn late_excerpts_preserve_ascii_unicode_and_segment_boundaries() {
        for text in [
            " A needle ".repeat(8000),
            " A🦀İΣ\u{2003}\nneedle ".repeat(5000),
            format!("{}{}", "A".repeat(512), "🦀 suffix ".repeat(12000)),
        ] {
            let item = item(
                0,
                ItemBody::Command {
                    command: text.clone(),
                    output: text,
                    cwd: None,
                    exit_code: None,
                },
            );
            let expected = previous_item_text(&item);
            let source = item_source(&item);
            let len = expected.chars().count();
            for offset in [
                0,
                255,
                256,
                1023,
                1024,
                4095,
                4096,
                8192,
                len / 2 - 1,
                len / 2,
                len / 2 + 1,
                len - 200,
                len - 1,
                len,
                len + 1,
            ] {
                assert_eq!(
                    source.excerpt(offset, 200),
                    expected
                        .chars()
                        .skip(offset)
                        .take(200)
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    "offset {offset}"
                );
            }
        }
    }

    #[test]
    fn large_lazy_tool_replacements_and_cancelled_searches_keep_cache_valid() {
        let mut view = ChatView::new("search-test", DisplayMode::Normal, system_clock());
        let original = item(
            0,
            ItemBody::ToolCall {
                server: None,
                tool: "read".into(),
                input: serde_json::json!({"text":"needle ΙΣ 🦀 ".repeat(5000)}),
                output: Some("old output".into()),
            },
        );
        view.apply(&[Envelope {
            chat_id: "search-test".into(),
            seq: 1,
            event: ChatEvent::ItemCompleted { item: original },
        }]);
        assert_eq!(
            view.search("needle"),
            previous_search(&view.fold.transcript.items, "needle")
        );
        let mut checks = 0;
        assert_eq!(
            view.search_while("needle", || {
                checks += 1;
                checks < 3
            }),
            None
        );
        assert_eq!(
            view.search("needle"),
            previous_search(&view.fold.transcript.items, "needle")
        );
        let replacement = item(
            0,
            ItemBody::ToolCall {
                server: None,
                tool: "read".into(),
                input: serde_json::json!({"text":"replacement ΣΟΣ ".repeat(2000)}),
                output: None,
            },
        );
        let event = Envelope {
            chat_id: "search-test".into(),
            seq: 2,
            event: ChatEvent::ItemCompleted { item: replacement },
        };
        view.apply(std::slice::from_ref(&event));
        assert!(view.search("needle").is_empty());
        assert_eq!(
            view.search("replacement"),
            previous_search(&view.fold.transcript.items, "replacement")
        );
        view.apply(&[event]);
        assert_eq!(
            view.search("ΣΟΣ"),
            previous_search(&view.fold.transcript.items, "ΣΟΣ")
        );
    }
}
