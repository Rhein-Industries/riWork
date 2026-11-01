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
use std::{collections::BTreeMap, collections::VecDeque, sync::Mutex};

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
    by_device: Mutex<BTreeMap<String, VecDeque<Screen>>>,
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
        output: &str,
    ) {
        let mut all = self.by_device.lock().expect("screens");
        let screens = all.entry(asked.device.to_owned()).or_default();
        screens.retain(|screen| screen.hash != hash || !same_question(screen, asked));
        if screens.len() >= KEEP {
            screens.pop_front();
        }
        screens.push_back(Screen {
            shell: asked.shell.to_owned(),
            lines: asked.lines,
            styled: asked.styled,
            hash: hash.to_owned(),
            history_size,
            output: output.to_owned(),
        });
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
        let all = self.by_device.lock().expect("screens");
        let held = all
            .get(asked.device)?
            .iter()
            .rev()
            .find(|screen| screen.hash == base && same_question(screen, asked))?;
        let (shift, line_count, changed) = changes(
            &held.output,
            held.history_size,
            output,
            history_size,
            asked.lines,
        )?;
        Some(json!({"base": base, "shift": shift, "line_count": line_count, "changed": changed}))
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
    let mut shifts = vec![0];
    if let Some(shift) = history_shift(old_history, new_history, lines)
        && shift != 0
    {
        shifts.push(shift);
    }
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
    let shift = shifts.into_iter().min_by_key(|&shift| cost(shift))?;
    if cost(shift).saturating_mul(2) >= new.len().max(1) {
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
