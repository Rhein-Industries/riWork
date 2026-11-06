//! Changed rows only: `shell.output` with `"delta": true` (`ready.features.output_delta`).
//!
//! A phone that follows a terminal asks again the moment the screen changes, and an
//! echo or a line of agent output changes a row or two of the 200 to 540 it holds.
//! A request with `delta` asks the connector to answer with only the rows that differ
//! from the screen the phone already has, the one its `if_changed` hash names. The
//! connector remembers the last few full screens it sent to each device that asked
//! (`Screens`, content-addressed by that hash, so a reconnect does not lose them), and
//! answers in full whenever it does not hold the one named, or the change is not
//! smaller than the screen. The CLI is not involved: it captures the whole screen
//! either way, and only what crosses the link gets smaller.
//!
//! The changes, `delta` in the result in place of `output`:
//!
//! ```json
//! {"base":"HASH","shift":3,"line_count":524,"changed":[[521,["$ ls","a  b",""]]]}
//! ```
//!
//! Split the base `output` at every `\n` (an output that ends in `\n` gives a last,
//! empty piece). The new output has `line_count` pieces: piece `i` is the text a
//! `changed` run gives it (`[start, [pieces...]]`, runs in order and apart), and
//! every other piece is base piece `i + shift`. Joined with `\n` they are exactly the
//! `output` a full answer would have carried, so its `hash` names it.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// Full screens remembered per device.
const KEEP: usize = 4;

/// A full screen as it was sent.
struct Screen {
    shell: String,
    lines: u32,
    styled: bool,
    hash: String,
    history_size: Option<u32>,
    output: String,
}

/// The last full screens sent to each device that asked for changed rows, newest last.
#[derive(Default)]
pub(super) struct Screens {
    by_device: Mutex<BTreeMap<String, VecDeque<Arc<Screen>>>>,
}

/// What the screen a device holds is, by the request that asked for it.
pub(super) struct Asked<'a> {
    pub device: &'a str,
    pub shell: &'a str,
    pub lines: u32,
    pub styled: bool,
}

impl Screens {
    pub(super) fn forget(&self, device: &str) {
        self.by_device.lock().expect("screens").remove(device);
    }

    /// Remembers the full screen `output` (with its `hash`) as now held by the device.
    pub(super) fn remember(
        &self,
        asked: &Asked<'_>,
        hash: &str,
        history_size: Option<u32>,
        output: String,
    ) {
        // Own the immutable screen before locking the shared device table.
        let screen = Arc::new(Screen {
            shell: asked.shell.to_owned(),
            lines: asked.lines,
            styled: asked.styled,
            hash: hash.to_owned(),
            history_size,
            output,
        });
        let mut all = self.by_device.lock().expect("screens");
        let screens = all.entry(asked.device.to_owned()).or_default();
        screens.retain(|screen| screen.hash != hash || !same_question(screen, asked));
        if screens.len() >= KEEP {
            screens.pop_front();
        }
        screens.push_back(screen);
    }

    /// The changes from the screen `base` names to `output`, if this device was
    /// sent that screen for the same question and the changes are worth sending.
    pub(super) fn delta(
        &self,
        asked: &Asked<'_>,
        base: &str,
        history_size: Option<u32>,
        output: &str,
    ) -> Option<Value> {
        let held = self.held(asked, base)?;
        let (shift, line_count, changed) = changes(
            &held.output,
            held.history_size,
            output,
            history_size,
            asked.lines,
        )?;
        Some(json!({"base": base, "shift": shift, "line_count": line_count, "changed": changed}))
    }

    // Retain the immutable screen, not the table lock, during comparison and
    // JSON construction. Other devices can read or replace their screens meanwhile.
    fn held(&self, asked: &Asked<'_>, base: &str) -> Option<Arc<Screen>> {
        self.by_device
            .lock()
            .expect("screens")
            .get(asked.device)?
            .iter()
            .rev()
            .find(|screen| screen.hash == base && same_question(screen, asked))
            .cloned()
    }
}

fn same_question(screen: &Screen, asked: &Asked<'_>) -> bool {
    screen.shell == asked.shell && screen.lines == asked.lines && screen.styled == asked.styled
}

/// How far the lines moved: base piece `i + shift` is new piece `i` for every line
/// that did not change. The output starts `min(lines, history_size)` lines above the
/// screen, so scrolling moves it by how much further down the history it starts.
fn history_shift(old: Option<u32>, new: Option<u32>, lines: u32) -> Option<i64> {
    let start = |size: u32| i64::from(size) - i64::from(size.min(lines));
    Some(start(new?) - start(old?))
}

/// `(shift, line_count, changed runs)` turning `old` into `new`, or `None` when
/// the changes are not smaller than half of `new` (a full answer is as good).
fn changes(
    old: &str,
    old_history: Option<u32>,
    new: &str,
    new_history: Option<u32>,
    lines: u32,
) -> Option<(i64, usize, Vec<Value>)> {
    let old: Vec<&str> = old.split('\n').collect();
    let new_pieces: Vec<&str> = new.split('\n').collect();
    let unchanged = |shift: i64, index: usize| {
        let at = index as i64 + shift;
        at >= 0 && (at as usize) < old.len() && old[at as usize] == new_pieces[index]
    };
    // Bytes the changed pieces cost; the cheapest shift wins.
    let cost = |shift: i64| -> usize {
        (0..new_pieces.len())
            .filter(|&index| !unchanged(shift, index))
            .map(|index| new_pieces[index].len() + 3)
            .sum()
    };
    let (mut shift, mut best_cost) = (0, cost(0));
    // Reuse the winning cost instead of scanning all lines again. A zero-cost
    // unshifted screen already wins, including ties, so it needs no other scan.
    if best_cost != 0
        && let Some(candidate) = history_shift(old_history, new_history, lines)
        && candidate != 0
    {
        let candidate_cost = cost(candidate);
        if candidate_cost < best_cost {
            shift = candidate;
            best_cost = candidate_cost;
        }
    }
    if best_cost.saturating_mul(2) >= new.len().max(1) {
        return None;
    }
    let mut changed: Vec<Value> = Vec::new();
    let mut run: Option<(usize, Vec<&str>)> = None;
    for (index, piece) in new_pieces.iter().enumerate() {
        if unchanged(shift, index) {
            if let Some((start, pieces)) = run.take() {
                changed.push(json!([start, pieces]));
            }
        } else {
            run.get_or_insert_with(|| (index, Vec::new())).1.push(piece);
        }
    }
    if let Some((start, pieces)) = run {
        changed.push(json!([start, pieces]));
    }
    Some((shift, new_pieces.len(), changed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &str, delta: &Value) -> String {
        let old: Vec<_> = old.split('\n').collect();
        let shift = delta["shift"].as_i64().unwrap();
        let mut pieces: Vec<_> = (0..delta["line_count"].as_u64().unwrap())
            .map(|index| {
                let at = index as i64 + shift;
                usize::try_from(at)
                    .ok()
                    .and_then(|at| old.get(at))
                    .copied()
                    .unwrap_or("")
            })
            .collect();
        for run in delta["changed"].as_array().unwrap() {
            let start = run[0].as_u64().unwrap() as usize;
            for (offset, piece) in run[1].as_array().unwrap().iter().enumerate() {
                pieces[start + offset] = piece.as_str().unwrap();
            }
        }
        pieces.join("\n")
    }

    #[test]
    fn screen_deltas_preserve_text_and_question_boundaries() {
        let screens = Screens::default();
        let asked = Asked {
            device: "phone",
            shell: "shell",
            lines: 100,
            styled: true,
        };
        let rows: Vec<_> = (0..120)
            .map(|i| format!("\u{1b}[32mrow {i}: héllo 🦀{}", " ".repeat(80)))
            .collect();
        let old = rows.join("\n");
        screens.remember(&asked, "base", Some(500), old.clone());
        for (history, expected_shift, new) in [
            (Some(500), 0, old.replacen("héllo", "world", 1)),
            (
                Some(503),
                3,
                format!("{}\nnew\nrows\n", rows[3..].join("\n")),
            ),
            (
                Some(497),
                -3,
                format!("new\nrows\n\n{}", rows[..117].join("\n")),
            ),
            (None, 0, old.clone()),
        ] {
            let delta = screens.delta(&asked, "base", history, &new).unwrap();
            assert_eq!(delta["shift"], expected_shift);
            assert_eq!(apply(&old, &delta), new);
        }
        assert!(screens.delta(&asked, "missing", Some(500), &old).is_none());
        assert!(
            screens
                .delta(&asked, "base", Some(500), "entirely different")
                .is_none()
        );
        for other in [
            Asked {
                device: "other",
                ..asked
            },
            Asked {
                shell: "other",
                ..asked
            },
            Asked {
                lines: 101,
                ..asked
            },
            Asked {
                styled: false,
                ..asked
            },
        ] {
            assert!(screens.delta(&other, "base", Some(500), &old).is_none());
        }
        // Refreshing an existing hash does not consume another history slot.
        screens.remember(&asked, "base", Some(500), old.clone());
        for i in 0..KEEP - 1 {
            screens.remember(&asked, &i.to_string(), Some(500), old.clone());
        }
        assert!(screens.delta(&asked, "base", Some(500), &old).is_some());
        screens.remember(&asked, "newest", Some(500), old.clone());
        assert!(screens.delta(&asked, "base", Some(500), &old).is_none());
        screens.forget(asked.device);
        assert!(screens.delta(&asked, "newest", Some(500), &old).is_none());
    }

    #[test]
    fn retained_screen_does_not_hold_the_device_table_lock() {
        let screens = Screens::default();
        let asked = Asked {
            device: "phone",
            shell: "shell",
            lines: 500,
            styled: true,
        };
        screens.remember(&asked, "base", Some(1000), "original screen".into());
        let held = screens.held(&asked, "base").unwrap();
        assert!(screens.by_device.try_lock().is_ok());
        screens.forget(asked.device);
        screens.remember(&asked, "base", Some(1001), "replacement screen".into());
        assert_eq!(held.output, "original screen");
        assert_eq!(held.history_size, Some(1000));
        assert_eq!(
            screens.held(&asked, "base").unwrap().output,
            "replacement screen"
        );
    }

    #[test]
    fn parsed_output_allocation_moves_to_reply_or_delta_cache() {
        use super::super::{Output, output_reply};
        let screens = Screens::default();
        let asked = Asked {
            device: "phone",
            shell: "shell",
            lines: 200,
            styled: false,
        };
        let text = "terminal output with enough identical text for a delta\n".repeat(200);
        let request = |delta, base: Option<&str>| Output {
            shell_id: "shell".into(),
            lines: Some(200),
            styled: Some(false),
            if_changed: base.map(str::to_owned),
            wait_ms: None,
            delta,
        };

        let owned = text.clone();
        let pointer = owned.as_ptr();
        screens.remember(&asked, "direct", Some(400), owned);
        assert_eq!(
            screens.held(&asked, "direct").unwrap().output.as_ptr(),
            pointer
        );

        let cli = json!({"output": text, "hash": "plain"});
        let pointer = cli["output"].as_str().unwrap().as_ptr();
        let reply =
            output_reply::build(&screens, "phone", &request(None, None), 200, false, cli).unwrap();
        assert_eq!(reply["output"].as_str().unwrap().as_ptr(), pointer);
        assert!(screens.held(&asked, "plain").is_none());

        let cli = json!({"output": text, "hash": "base"});
        let pointer = cli["output"].as_str().unwrap().as_ptr();
        let reply = output_reply::build(
            &screens,
            "phone",
            &request(Some(true), None),
            200,
            false,
            cli,
        )
        .unwrap();
        assert_eq!(reply["output"].as_str().unwrap().as_ptr(), pointer);
        let cached = screens.held(&asked, "base").unwrap();
        assert_ne!(cached.output.as_ptr(), pointer);
        assert_eq!(cached.output, reply["output"].as_str().unwrap());

        let changed = format!("{text}new row");
        let cli = json!({"output": changed, "hash": "next"});
        let pointer = cli["output"].as_str().unwrap().as_ptr();
        let reply = output_reply::build(
            &screens,
            "phone",
            &request(Some(true), Some("base")),
            200,
            false,
            cli,
        )
        .unwrap();
        assert!(reply.get("output").is_none());
        assert!(reply.get("delta").is_some());
        let held = screens.held(&asked, "next").unwrap();
        assert_eq!(held.output.as_ptr(), pointer);
        assert_eq!(held.output, changed);
        screens.forget("phone");
        assert!(screens.held(&asked, "next").is_none());
        assert_eq!(held.output.as_ptr(), pointer);
        assert_eq!(held.output, changed);
    }

    #[test]
    #[ignore = "slow: manual screen-delta performance measurement"]
    fn screen_delta_cost() {
        let screens = Screens::default();
        let asked = Asked {
            device: "phone",
            shell: "shell",
            lines: 500,
            styled: true,
        };
        let old = (0..540)
            .map(|i| {
                format!(
                    "\u{1b}[32mrow {i}: {}",
                    "styled terminal output ".repeat(12)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let new = format!("{}\nnew output", old.split_once('\n').unwrap().1);
        screens.remember(&asked, "base", Some(1000), old.clone());
        let start = std::time::Instant::now();
        for _ in 0..2000 {
            std::hint::black_box(screens.delta(&asked, "base", Some(1001), &new).unwrap());
        }
        eprintln!(
            "screen_delta_cost: {:?}/delta ({} bytes)",
            start.elapsed() / 2000,
            old.len()
        );
    }
}
