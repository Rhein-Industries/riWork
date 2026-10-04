//! Links in terminal tabs: the URL or file under a ⌘-click, found in the text tmux shows.
//!
//! A tab's Ghostty surface is a tmux client, so the text on screen is read from the tmux
//! the shell lives in (`SessionManager::capture_link_view`) and never from Ghostty, which
//! has no API for it here. This module is the part that needs neither GPUI nor a window:
//! the pointer-to-cell arithmetic, the parsing of that capture, the search for a token under
//! a cell, and the check that a path is real. `ui` wires them to the mouse and opens the
//! result.

use std::{
    fs,
    ops::Range,
    path::{Component, Path, PathBuf},
};

use crate::theme::{GhosttyPadding, PaddingBalance};

mod overlay;
mod ui;
pub use overlay::Overlay;
pub use ui::LinkState;

#[cfg(test)]
mod tests;

/// Rows of context captured above and below the visible screen, so a link wrapped across the
/// screen's edge can still be joined.
pub const CONTEXT_ROWS: i64 = 12;

/// Longest path or URL taken from the screen. PATH_MAX is 1024 on macOS; this leaves room for
/// a long URL while keeping a pathological line from reaching the file system.
const MAX_TOKEN: usize = 4096;

// ---------------------------------------------------------------------------------------
// What a click means
// ---------------------------------------------------------------------------------------

/// What the pointer is over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// An `http`, `https` or `mailto` URL, to open in the default handler.
    Url(String),
    /// A file or folder that exists.
    Path(ResolvedPath),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPath {
    /// Absolute, symbolic links resolved.
    pub path: PathBuf,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub is_dir: bool,
}

/// How the modifier keys of a click ask for the link to be opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// ⌘: in the browser, in Preview, or in the default app.
    Open,
    /// ⌘⇧: a file in the Vim editor tab.
    Edit,
}

/// Which modifiers make a mouse event a link gesture: ⌘, with Shift for editing, and nothing
/// else, so ⌘⌃ and ⌘⌥ chords stay with the terminal.
pub fn open_mode(modifiers: gpui::Modifiers) -> Option<OpenMode> {
    (modifiers.platform && !modifiers.control && !modifiers.alt && !modifiers.function).then_some(
        if modifiers.shift {
            OpenMode::Edit
        } else {
            OpenMode::Open
        },
    )
}

// ---------------------------------------------------------------------------------------
// Pointer to cell
// ---------------------------------------------------------------------------------------

/// Where Ghostty puts its cell grid inside the surface, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub cell_width: f64,
    pub cell_height: f64,
    pub origin_x: f64,
    pub origin_y: f64,
    pub cols: u32,
    pub rows: u32,
}

/// The padding Ghostty reserves at each edge: its `window-padding-*` settings are in points and
/// it floors them to pixels (`Surface.zig`, `DerivedConfig.scaledPadding`).
pub fn explicit_padding(config: &GhosttyPadding, scale: f64) -> Padding {
    let pixels = |points: u32| (f64::from(points) * scale).floor() as u32;
    Padding {
        left: pixels(config.x.0),
        right: pixels(config.x.1),
        top: pixels(config.y.0),
        bottom: pixels(config.y.1),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Padding {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

/// The whole-pixel sizes one cell can have along an axis.
///
/// Ghostty's cells are whole pixels and the grid is however many fit:
/// `cells == floor(available / size)`. Only some sizes satisfy that for a known count.
/// Dividing the space by the count instead would be wrong by up to the pixels left over at the
/// far edge, which is up to a whole cell at the last column or row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeRange {
    smallest: u32,
    largest: u32,
}

impl SizeRange {
    /// The sizes at which `cells` fit in `available` pixels and one more does not. None when
    /// there are none: the count and the space are not those of one grid.
    fn of(available: u32, cells: u32) -> Option<Self> {
        let largest = available / cells;
        // One more cell must not fit.
        let smallest = available / (cells + 1) + 1;
        (smallest <= largest).then_some(Self { smallest, largest })
    }

    fn intersect(self, other: Self) -> Option<Self> {
        let (smallest, largest) = (
            self.smallest.max(other.smallest),
            self.largest.min(other.largest),
        );
        (smallest <= largest).then_some(Self { smallest, largest })
    }

    /// One size to use. A count above the cell size in pixels leaves a single size; a shorter
    /// axis can admit two, and the middle keeps the far end of the axis within half a cell.
    fn size(self) -> f64 {
        f64::from(self.smallest + self.largest) / 2.0
    }
}

/// What the terminals of a window have shown about their cell size.
///
/// A window's terminals share a font and a display scale, so their cells are the same size. A
/// terminal too short to settle the size on its own (a split pane of a dozen rows can fit two
/// sizes) is helped by the taller one beside it. A terminal that contradicts all the earlier
/// ones has a font of its own (it was zoomed) and starts the record again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellHint {
    /// The display scale in thousandths: sizes in pixels mean nothing on another display.
    scale: u32,
    width: Option<SizeRange>,
    height: Option<SizeRange>,
}

impl CellHint {
    fn for_scale(&mut self, scale: f64) {
        let scale = (scale * 1000.0).round() as u32;
        if self.scale != scale {
            *self = Self {
                scale,
                ..Self::default()
            };
        }
    }

    fn narrow(known: &mut Option<SizeRange>, seen: Option<SizeRange>) -> Option<SizeRange> {
        let seen = seen?;
        let narrowed = known
            .and_then(|known| known.intersect(seen))
            .unwrap_or(seen);
        *known = Some(narrowed);
        Some(narrowed)
    }
}

fn cell_size(range: Option<SizeRange>, available: u32, cells: u32) -> f64 {
    // Without a range, the count and the space disagree, so this is not the grid they describe.
    range.map_or(f64::from(available) / f64::from(cells), SizeRange::size)
}

/// The grid Ghostty lays out in a surface of `screen` pixels that holds `cols` by `rows`
/// cells, as `renderer/size.zig` does it.
pub fn grid_geometry(
    screen: (u32, u32),
    cols: u32,
    rows: u32,
    explicit: Padding,
    balance: PaddingBalance,
    hint: &mut CellHint,
) -> Option<Grid> {
    if cols == 0 || rows == 0 {
        return None;
    }
    let available_width = screen.0.checked_sub(explicit.left + explicit.right)?;
    let available_height = screen.1.checked_sub(explicit.top + explicit.bottom)?;
    if available_width < cols || available_height < rows {
        return None;
    }
    let width = CellHint::narrow(&mut hint.width, SizeRange::of(available_width, cols));
    let height = CellHint::narrow(&mut hint.height, SizeRange::of(available_height, rows));
    let cell_width = cell_size(width, available_width, cols);
    let cell_height = cell_size(height, available_height, rows);
    let (mut origin_x, mut origin_y) = (f64::from(explicit.left), f64::from(explicit.top));
    if balance != PaddingBalance::False {
        // The leftover space is split evenly after the explicit padding is counted.
        origin_x = ((f64::from(screen.0) - f64::from(cols) * cell_width) / 2.0).floor();
        origin_y = ((f64::from(screen.1) - f64::from(rows) * cell_height) / 2.0).floor();
        if balance == PaddingBalance::True {
            // The top padding is capped at the horizontal padding plus half a cell, and the
            // rest goes to the bottom.
            let most = (f64::from(explicit.left + explicit.right) + cell_width) / 2.0;
            origin_y = origin_y.min(most.floor());
        }
    }
    Some(Grid {
        cell_width,
        cell_height,
        origin_x,
        origin_y,
        cols,
        rows,
    })
}

/// The cell under a point in surface pixels, or none over the padding or the unused strip past
/// the last cell.
pub fn cell_at(grid: &Grid, x: f64, y: f64) -> Option<(u32, u32)> {
    let (x, y) = (x - grid.origin_x, y - grid.origin_y);
    if x < 0.0 || y < 0.0 {
        return None;
    }
    let (col, row) = ((x / grid.cell_width) as u32, (y / grid.cell_height) as u32);
    (col < grid.cols && row < grid.rows).then_some((col, row))
}

/// The cell under a pointer. `size` is the terminal's size and `offset` the pointer's place in it,
/// both in points; `cells` is the columns and rows of the grid; the surface is sized in whole
/// pixels, as `gpui-libghostty` does it.
pub fn cell_under_pointer(
    size: (f64, f64),
    offset: (f64, f64),
    scale: f64,
    cells: (u32, u32),
    padding: &GhosttyPadding,
    hint: &mut CellHint,
) -> Option<(u32, u32)> {
    let grid = surface_grid(size, scale, cells, padding, hint)?;
    cell_at(&grid, offset.0 * scale, offset.1 * scale)
}

/// The grid of a terminal `size` points large, in pixels.
fn surface_grid(
    size: (f64, f64),
    scale: f64,
    cells: (u32, u32),
    padding: &GhosttyPadding,
    hint: &mut CellHint,
) -> Option<Grid> {
    let screen = ((size.0 * scale) as u32, (size.1 * scale) as u32);
    hint.for_scale(scale);
    grid_geometry(
        screen,
        cells.0,
        cells.1,
        explicit_padding(padding, scale),
        padding.balance,
        hint,
    )
}

/// The cells of one row of the visible screen that a link covers: columns `cols.start` up to
/// `cols.end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRow {
    pub row: u32,
    pub cols: Range<u32>,
}

/// A rectangle in points, from the terminal's top left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strip {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Where the line under a link goes in a terminal laid out as `cell_under_pointer` sees it: one
/// strip per row, along the bottom of the link's cells, a device pixel per point of scale thick.
pub fn underline_strips(
    size: (f64, f64),
    scale: f64,
    cells: (u32, u32),
    padding: &GhosttyPadding,
    hint: &mut CellHint,
    rows: &[LinkRow],
) -> Vec<Strip> {
    let Some(grid) = surface_grid(size, scale, cells, padding, hint) else {
        return Vec::new();
    };
    let thickness = scale.round().max(1.0);
    rows.iter()
        .filter(|run| run.row < grid.rows && run.cols.start < run.cols.end)
        .map(|run| {
            let end = run.cols.end.min(grid.cols);
            let left = grid.origin_x + f64::from(run.cols.start) * grid.cell_width;
            let right = grid.origin_x + f64::from(end) * grid.cell_width;
            let bottom = (grid.origin_y + f64::from(run.row + 1) * grid.cell_height).floor();
            Strip {
                x: left / scale,
                y: (bottom - thickness) / scale,
                width: (right - left) / scale,
                height: thickness / scale,
            }
        })
        .filter(|strip| strip.width > 0.0)
        .collect()
}

// ---------------------------------------------------------------------------------------
// What tmux shows
// ---------------------------------------------------------------------------------------

/// The three answers of the one tmux call that reads a shell's screen, split but not parsed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawCapture {
    /// Pane facts: see `PaneView::parse`.
    pub header: String,
    /// One line per client attached to the session.
    pub clients: String,
    /// The captured rows, each padded to the full width (`capture-pane -N`).
    pub rows: String,
    /// The same rows with wrapped ones joined (`capture-pane -J`).
    pub joined: String,
    /// The rows again with their escape sequences (`capture-pane -e -N`), which is where tmux
    /// keeps the target of an OSC 8 hyperlink.
    pub escaped: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneMode {
    /// The live screen, or the alternate screen of a full-screen program.
    Live,
    /// copy mode (the scrollback view the wheel enters) or view mode.
    Scrolled,
    /// Anything else (a chooser, say): what tmux shows is not the pane's text.
    Other,
}

/// A shell's screen as tmux reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneView {
    /// The size of the Ghostty grid: the attached client's, or the pane's when no client says.
    pub cols: u32,
    pub rows: u32,
    /// Lines the view is scrolled back in copy mode; 0 on the live screen.
    pub scroll: i64,
    pub mode: PaneMode,
    pub alternate: bool,
    /// The working directory of the pane's foreground process.
    pub cwd: PathBuf,
    /// The line number of `lines[0]`, counted from the top of the live screen, so history is
    /// negative.
    first_line: i64,
    /// Whether `first_line` is the oldest line the pane has.
    at_history_top: bool,
    /// The captured rows, `pane width` cells each.
    lines: Vec<String>,
    /// `wraps[i]`: row `i` continues on row `i + 1`.
    wraps: Vec<bool>,
    /// `padded[i]`: row `i` ends in a cell that is not text. A double-width character that did
    /// not fit in the last column went to the next row and left that cell empty.
    padded: Vec<bool>,
    /// The capture ends in a row that may continue past it.
    end_cut: bool,
    /// The OSC 8 hyperlink on each character of each row, as an index into `targets`.
    hyperlinks: Vec<Vec<Option<usize>>>,
    targets: Vec<String>,
}

/// The text of one logical line (rows joined at the places the terminal wrapped them) and the
/// character under a cell.
struct Located {
    chars: Vec<char>,
    index: usize,
    /// The target of the OSC 8 hyperlink on that character.
    hyperlink: Option<String>,
    /// Where each character is: its captured row, first cell and width in cells.
    places: Vec<(usize, u32, u32)>,
    /// The OSC 8 hyperlink on each character, as an index into the targets.
    links: Vec<Option<usize>>,
    /// The line may begin or end beyond what was captured.
    start_cut: bool,
    end_cut: bool,
}

impl Located {
    /// The characters around the one under the cell that carry the same OSC 8 hyperlink.
    fn hyperlink_span(&self) -> Range<usize> {
        let Some(link) = self.links.get(self.index).copied().flatten() else {
            return self.index..self.index;
        };
        let same = |at: &usize| self.links.get(*at).copied().flatten() == Some(link);
        let start = (0..self.index)
            .rev()
            .take_while(same)
            .last()
            .unwrap_or(self.index);
        let end = (self.index..self.links.len()).take_while(same).count() + self.index;
        start..end
    }
}

impl PaneView {
    /// Parse the answers of `SessionManager::capture_link_view`.
    ///
    /// The header is `pane width, pane height, history size, scroll position, pane mode,
    /// alternate screen flag, working directory`, tab separated. A client line is
    /// `control mode, read only, width, height, activity`.
    pub fn parse(raw: &RawCapture) -> Result<Self, String> {
        let fields: Vec<&str> = raw.header.trim_end_matches('\n').splitn(7, '\t').collect();
        let [pane_cols, pane_rows, history, scroll, mode, alternate, cwd] = fields[..] else {
            return Err("tmux did not describe the pane".into());
        };
        let number = |text: &str| -> Result<i64, String> {
            text.trim()
                .parse()
                .map_err(|_| format!("tmux sent {text:?} where a number belongs"))
        };
        let (pane_cols, pane_rows) = (number(pane_cols)?, number(pane_rows)?);
        let history = number(history)?.max(0);
        let scroll = number(scroll)?.max(0);
        if pane_cols < 1 || pane_rows < 1 {
            return Err("the pane has no size".into());
        }
        let mode = match mode {
            "" => PaneMode::Live,
            "copy-mode" | "view-mode" => PaneMode::Scrolled,
            _ => PaneMode::Other,
        };
        // The desktop's own client is the one that was used last; control-mode clients (the
        // phone's, a watcher) never size a window. Its size is the Ghostty grid, which a phone
        // that resized the window leaves larger than the pane.
        let mut grid = (pane_cols, pane_rows);
        let mut latest = i64::MIN;
        for line in raw.clients.lines() {
            let mut fields = line.split('\t');
            let (Some("0"), Some(_), Some(width), Some(height), Some(activity)) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                continue;
            };
            if let (Ok(width), Ok(height), Ok(activity)) = (
                width.trim().parse::<i64>(),
                height.trim().parse::<i64>(),
                activity.trim().parse::<i64>(),
            ) && activity >= latest
            {
                latest = activity;
                grid = (width, height);
            }
        }
        let (cols, rows) = grid;
        let first_line = (-scroll - CONTEXT_ROWS).max(-history);
        let lines: Vec<String> = raw.rows.lines().map(str::to_owned).collect();
        let last_line = (pane_rows - 1 - scroll + CONTEXT_ROWS).min(pane_rows - 1);
        if lines.len() as i64 != last_line - first_line + 1 {
            return Err(format!(
                "tmux sent {} rows where {} were asked for",
                lines.len(),
                last_line - first_line + 1
            ));
        }
        let (wraps, padded) = derive_wraps(&lines, &raw.joined)
            .unwrap_or_else(|| (vec![false; lines.len()], vec![false; lines.len()]));
        // tmux ends the joined capture with a newline whether or not its last row goes on, so a
        // row that fills the line and has rows after it that were not captured may be cut.
        let more_below = last_line < pane_rows - 1;
        let end_cut = more_below
            && lines
                .last()
                .is_some_and(|row| !row.is_empty() && !row.ends_with(' '));
        let (hyperlinks, targets) = parse_hyperlinks(&raw.escaped, lines.len());
        Ok(Self {
            cols: cols.clamp(1, 1000) as u32,
            rows: rows.clamp(1, 1000) as u32,
            scroll,
            mode,
            alternate: alternate.trim() == "1",
            cwd: PathBuf::from(cwd.trim_end_matches('\n')),
            first_line,
            at_history_top: first_line == -history,
            lines,
            wraps,
            padded,
            end_cut,
            hyperlinks,
            targets,
        })
    }

    /// The logical line under a cell of the visible screen.
    fn locate(&self, row: u32, col: u32) -> Option<Located> {
        let index = (i64::from(row) - self.scroll - self.first_line) as usize;
        let last = self.lines.len().checked_sub(1)?;
        if index > last {
            return None;
        }
        let first = (0..index)
            .rev()
            .take_while(|row| self.wraps[*row])
            .last()
            .unwrap_or(index);
        let end = (index..last).take_while(|row| self.wraps[*row]).count() + index;
        let mut chars = Vec::new();
        let mut places = Vec::new();
        let mut links = Vec::new();
        let mut found = None;
        let mut hyperlink = None;
        for row in first..=end {
            let text = &self.lines[row];
            if row == index {
                let offset = char_at_cell(text, col)?;
                found = Some(chars.len() + offset);
                hyperlink = self
                    .hyperlinks
                    .get(row)
                    .and_then(|row| row.get(offset).copied().flatten())
                    .and_then(|target| self.targets.get(target).cloned());
            }
            // A row that continues is full; the last one is trimmed of its blank tail.
            let kept = if row == end {
                text.trim_end_matches(' ').chars().count()
            } else if self.padded[row] {
                text.chars().count().saturating_sub(1)
            } else {
                text.chars().count()
            };
            let mut cell = 0;
            for (offset, ch) in text.chars().take(kept).enumerate() {
                // A combining mark covers no cell of its own.
                let width = char_width(ch) as u32;
                chars.push(ch);
                places.push((row, cell, width));
                links.push(
                    self.hyperlinks
                        .get(row)
                        .and_then(|row| row.get(offset).copied().flatten()),
                );
                cell += width;
            }
        }
        Some(Located {
            chars,
            index: found?,
            hyperlink,
            places,
            links,
            start_cut: first == 0 && !self.at_history_top,
            end_cut: end == last && self.end_cut,
        })
    }

    /// The text of the logical line under a cell, for tests of how rows are joined.
    #[cfg(test)]
    pub fn logical_text(&self, row: u32, col: u32) -> Option<String> {
        self.locate(row, col)
            .map(|located| located.chars.iter().collect())
    }

    /// What a cell of the visible screen links to.
    #[cfg(test)]
    pub fn link_at(&self, row: u32, col: u32, bases: &Bases) -> Option<Link> {
        self.link_span_at(row, col, bases).map(|(link, _)| link)
    }

    /// What a cell of the visible screen links to, and the cells of the screen the link covers,
    /// one run per row: the words of a hyperlink, or the text that was read as the link, with
    /// its position (`:120`) and on every row it was wrapped over.
    pub fn link_span_at(&self, row: u32, col: u32, bases: &Bases) -> Option<(Link, Vec<LinkRow>)> {
        if self.mode == PaneMode::Other {
            return None;
        }
        let located = self.locate(row, col)?;
        // The target of a hyperlink is what its author meant; the words on it may be anything.
        let target = located
            .hyperlink
            .as_deref()
            .and_then(|target| hyperlink_candidate(target, local_hostname().as_deref()));
        let text = candidates_at(&located.chars, located.index)
            .into_iter()
            .filter(|candidate| {
                // A token that touches the edge of what was captured may be only part of one.
                !(located.start_cut && candidate.span().start == 0
                    || located.end_cut && candidate.span().end == located.chars.len())
            });
        let text: Vec<Candidate> = text.collect();
        // Words that are themselves an address are not allowed to point somewhere else: a link
        // that says `https://github.com/x` and goes to another site is a trick, and the words are
        // what the person saw.
        let target = target.filter(|target| match (target, text.first()) {
            (Candidate::Url { url: to, .. }, Some(Candidate::Url { url: shown, .. })) => {
                url_host(to) == url_host(shown)
            }
            _ => true,
        });
        let from_target = target.is_some();
        let candidates: Vec<Candidate> = target.into_iter().chain(text).collect();
        let (link, taken) = resolve_which(&candidates, bases)?;
        let span = if from_target && taken == 0 {
            located.hyperlink_span()
        } else {
            candidates[taken].span().clone()
        };
        Some((link, self.screen_runs(&located, span)))
    }

    /// The cells of the visible screen that characters of a located line cover, one run per row.
    /// Rows scrolled out of view have none.
    fn screen_runs(&self, located: &Located, span: Range<usize>) -> Vec<LinkRow> {
        let mut runs: Vec<LinkRow> = Vec::new();
        for &(line, cell, width) in located.places.get(span).unwrap_or_default() {
            let row = line as i64 + self.first_line + self.scroll;
            if width == 0 || row < 0 || row >= i64::from(self.rows) {
                continue;
            }
            let row = row as u32;
            match runs.last_mut() {
                Some(run) if run.row == row => run.cols.end = run.cols.end.max(cell + width),
                _ => runs.push(LinkRow {
                    row,
                    cols: cell..cell + width,
                }),
            }
        }
        runs
    }
}

/// The character of a full-width row that covers `cell`, as an index into its characters.
fn char_at_cell(row: &str, cell: u32) -> Option<usize> {
    let mut start = 0usize;
    let mut base = None;
    for (index, ch) in row.chars().enumerate() {
        let width = char_width(ch);
        if width == 0 {
            // Combining marks share the cell of the character before them.
            continue;
        }
        if (cell as usize) < start + width {
            base = Some(index);
            break;
        }
        start += width;
    }
    base
}

/// Which rows of a capture continue on the next one, found by comparing the capture of the rows
/// (every row padded to the full width) with the capture that joins wrapped rows; and which of
/// those end in a cell the joined capture leaves out. `None` when the two disagree, which is
/// better handled by not joining anything than by guessing.
fn derive_wraps(rows: &[String], joined: &str) -> Option<(Vec<bool>, Vec<bool>)> {
    let mut lines: Vec<Vec<char>> = joined
        .split('\n')
        .map(|line| line.chars().collect())
        .collect();
    if joined.ends_with('\n') || joined.is_empty() {
        lines.pop();
    }
    let mut lines = lines.into_iter();
    let mut wraps = vec![false; rows.len()];
    let mut padded = vec![false; rows.len()];
    let mut row = 0;
    while row < rows.len() {
        let line = lines.next()?;
        let mut offset = 0;
        loop {
            let text: Vec<char> = rows.get(row)?.chars().collect();
            let rest = &line[offset..];
            if rest.len() >= text.len() && rest[..text.len()] == text[..] {
                // The whole row is in this line; the line goes on if there is more of it.
                offset += text.len();
                if offset < line.len() {
                    wraps[row] = true;
                    row += 1;
                    continue;
                }
            } else if text.last() == Some(&' ')
                && rest.len() >= text.len()
                && rest[..text.len() - 1] == text[..text.len() - 1]
            {
                // A double-width character that did not fit in the last column went on the next
                // row and left that cell empty, which the joined capture does not print.
                wraps[row] = true;
                padded[row] = true;
                offset += text.len() - 1;
                row += 1;
                continue;
            } else if !(rest.len() < text.len()
                && rest == &text[..rest.len()]
                && text[rest.len()..].iter().all(|ch| *ch == ' '))
            {
                return None;
            }
            break;
        }
        row += 1;
    }
    (lines.next().is_none()).then_some((wraps, padded))
}

/// The OSC 8 hyperlinks in a capture made with `-e`: for each row, the link on each of its
/// characters (an index into the targets), and the targets. Rows that cannot be matched with
/// the captured rows give no links.
fn parse_hyperlinks(escaped: &str, rows: usize) -> (Vec<Vec<Option<usize>>>, Vec<String>) {
    let mut targets: Vec<String> = Vec::new();
    let mut links = Vec::with_capacity(rows);
    if !escaped.contains("\u{1b}]8;") {
        return (Vec::new(), targets);
    }
    for line in escaped.lines() {
        let mut row = Vec::new();
        let mut current = None;
        let mut chars = line.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '\u{1b}' {
                row.push(current);
                continue;
            }
            match chars.next() {
                // CSI: parameters, then a final byte.
                Some('[') => {
                    for ch in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&ch) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST.
                Some(']') => {
                    let mut body = String::new();
                    while let Some(ch) = chars.next() {
                        if ch == '\u{7}' {
                            break;
                        }
                        if ch == '\u{1b}' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                        body.push(ch);
                    }
                    if let Some(rest) = body.strip_prefix("8;") {
                        // `8;params;URI`
                        let target = rest.split_once(';').map_or("", |(_, target)| target);
                        current = (!target.is_empty()).then(|| {
                            targets
                                .iter()
                                .position(|known| known == target)
                                .unwrap_or_else(|| {
                                    targets.push(target.to_owned());
                                    targets.len() - 1
                                })
                        });
                    }
                }
                // A charset or other two-byte sequence: `ESC ( B`.
                Some('(' | ')' | '*' | '+' | '#') => {
                    chars.next();
                }
                _ => {}
            }
        }
        links.push(row);
    }
    if links.len() != rows {
        return (Vec::new(), Vec::new());
    }
    (links, targets)
}

/// What an OSC 8 hyperlink's target opens: a web or mail address, or a file of this machine.
/// The host a program puts in a `file:` target is the machine's own name.
fn hyperlink_candidate(target: &str, hostname: Option<&str>) -> Option<Candidate> {
    let lower = target.to_ascii_lowercase();
    let span = 0..0;
    if lower.starts_with("file://") {
        let rest = &target["file://".len()..];
        let host_end = rest.find('/')?;
        let host = &rest[..host_end];
        let local = host.is_empty()
            || host.eq_ignore_ascii_case("localhost")
            || hostname.is_some_and(|name| {
                let name = name.trim_end_matches(".local");
                host.eq_ignore_ascii_case(name)
                    || host.trim_end_matches(".local").eq_ignore_ascii_case(name)
            });
        return local
            .then(|| url_candidate(&format!("file://{}", &rest[host_end..]), span))
            .flatten();
    }
    (lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:"))
        .then(|| url_candidate(target, span))
        .flatten()
}

/// The host of a URL, in lower case: `mailto:` has none.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit('@').next().unwrap_or(authority);
    Some(host.to_ascii_lowercase())
}

/// This machine's name, as programs put it in `file:` hyperlinks.
fn local_hostname() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is writable for its whole length, and one byte is left for the NUL.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len() - 1) };
    if status != 0 {
        return None;
    }
    let length = buffer.iter().position(|byte| *byte == 0)?;
    String::from_utf8(buffer[..length].to_vec()).ok()
}

/// How many terminal cells a character covers. tmux decides what is on which cell, so this
/// only has to agree with its width tables for the characters terminal output actually holds.
pub fn char_width(ch: char) -> usize {
    let code = ch as u32;
    match code {
        0..=0x1F | 0x7F..=0x9F => 0,
        0x300..=0x36F
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0x200B..=0x200F
        | 0x2060..=0x2064
        | 0x20D0..=0x20FF
        | 0xFE00..=0xFE0F
        | 0xFE20..=0xFE2F
        | 0xFEFF
        | 0xE0100..=0xE01EF => 0,
        0x1100..=0x115F
        | 0x231A..=0x231B
        | 0x2329..=0x232A
        | 0x23E9..=0x23EC
        | 0x23F0
        | 0x23F3
        | 0x25FD..=0x25FE
        | 0x2614..=0x2615
        | 0x2648..=0x2653
        | 0x267F
        | 0x2693
        | 0x26A1
        | 0x26AA..=0x26AB
        | 0x26BD..=0x26BE
        | 0x26C4..=0x26C5
        | 0x26CE
        | 0x26D4
        | 0x26EA
        | 0x26F2..=0x26F3
        | 0x26F5
        | 0x26FA
        | 0x26FD
        | 0x2705
        | 0x270A..=0x270B
        | 0x2728
        | 0x274C
        | 0x274E
        | 0x2753..=0x2755
        | 0x2757
        | 0x2795..=0x2797
        | 0x27B0
        | 0x27BF
        | 0x2B1B..=0x2B1C
        | 0x2B50
        | 0x2B55
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F680..=0x1F6FF
        | 0x1F900..=0x1F9FF
        | 0x1FA70..=0x1FAFF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

// ---------------------------------------------------------------------------------------
// Finding the token under a cell
// ---------------------------------------------------------------------------------------

/// Something under the pointer that may be a link, before the file system has been asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Candidate {
    Url {
        url: String,
        span: Range<usize>,
    },
    Path {
        text: String,
        line: Option<u32>,
        col: Option<u32>,
        span: Range<usize>,
    },
}

impl Candidate {
    /// Where it sits in the line, in characters.
    pub fn span(&self) -> &Range<usize> {
        match self {
            Self::Url { span, .. } | Self::Path { span, .. } => span,
        }
    }
}

/// Every reading of the characters around `index` that could be a link, most specific first. A
/// URL under the pointer is the only answer; otherwise the paths are listed for `resolve` to try
/// against the file system in order.
pub fn candidates_at(chars: &[char], index: usize) -> Vec<Candidate> {
    if index >= chars.len() {
        return Vec::new();
    }
    if let Some(candidate) = url_at(chars, index) {
        return vec![candidate];
    }
    let mut found = Vec::new();
    // A quoted run can hold spaces, which a token never does.
    for span in quoted_spans(chars, index) {
        let text: String = chars[span.clone()].iter().collect();
        if !text.contains(' ') {
            continue;
        }
        for (text, line, col) in path_readings(&text) {
            let line = line.or_else(|| line_after_quote(chars, &span));
            found.push(Candidate::Path {
                text,
                line,
                col,
                span: span.clone(),
            });
        }
    }
    if is_token_char(chars[index], true) {
        // A bracketed token first: `app/(auth)/[id]/page.tsx` is a real path. Then the plain
        // one, for `[docs](docs/a.md)` and `(see docs/a.md)`.
        for brackets in [true, false] {
            let span = token_span(chars, index, brackets);
            let span = trim_token(chars, span);
            let text: String = chars[span.clone()].iter().collect();
            for (text, line, col) in path_readings(&text) {
                let line = line.or_else(|| line_after_quote(chars, &span));
                found.push(Candidate::Path {
                    text,
                    line,
                    col,
                    span: span.clone(),
                });
            }
        }
    }
    // The two readings are often the same.
    let mut unique: Vec<Candidate> = Vec::new();
    for candidate in found {
        let same = |other: &Candidate| match (other, &candidate) {
            (
                Candidate::Path {
                    text, line, col, ..
                },
                Candidate::Path {
                    text: new_text,
                    line: new_line,
                    col: new_col,
                    ..
                },
            ) => text == new_text && line == new_line && col == new_col,
            _ => false,
        };
        if !unique.iter().any(same) {
            unique.push(candidate);
        }
    }
    unique
}

const URL_SCHEMES: [&str; 4] = ["https://", "http://", "file://", "mailto:"];

/// The URL (or `file:` URL) the character at `index` is part of.
fn url_at(chars: &[char], index: usize) -> Option<Candidate> {
    // A URL has no gaps, so its start is within the unbroken run of URL characters that ends at
    // the pointer. The earliest start wins: `https://a.example/?next=https://b.example` is one
    // URL, whichever half the pointer is on.
    let mut found = None;
    let mut start = index;
    while is_url_char(chars[start]) {
        for scheme in URL_SCHEMES {
            let length = scheme.len();
            let fits = start + length <= chars.len()
                && chars[start..start + length]
                    .iter()
                    .zip(scheme.chars())
                    .all(|(ch, expected)| ch.to_ascii_lowercase() == expected);
            // `xhttp://` is not a scheme.
            let boundary = start == 0 || !chars[start - 1].is_alphanumeric();
            if !fits || !boundary {
                continue;
            }
            let mut end = start + length;
            while end < chars.len() && is_url_char(chars[end]) {
                end += 1;
            }
            // `https://host/a…` was cut short on screen; opening it would open another page.
            if chars.get(end) == Some(&'…') {
                continue;
            }
            let end = trim_url(chars, start + length, end);
            if end > start + length && index < end {
                let text: String = chars[start..end].iter().collect();
                found = url_candidate(&text, start..end).or(found);
            }
        }
        if start == 0 || index - start >= MAX_TOKEN {
            break;
        }
        start -= 1;
    }
    found
}

fn url_candidate(text: &str, span: Range<usize>) -> Option<Candidate> {
    let lower = text.to_ascii_lowercase();
    if let Some(rest) = lower
        .strip_prefix("file://")
        .map(|_| &text["file://".len()..])
    {
        // `file:///tmp/a`, or `file://localhost/tmp/a`; another host is not this machine.
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        if !rest.starts_with('/') {
            return None;
        }
        let rest = rest.split('?').next().unwrap_or(rest);
        let (path, fragment) = rest.split_once('#').unwrap_or((rest, ""));
        let path = percent_decode(path)?;
        let (line, col) = fragment_position(fragment);
        return Some(Candidate::Path {
            text: path,
            line,
            col,
            span,
        });
    }
    // A scheme alone, `https://`, is not an address.
    let after = &text[text.find(':')? + 1..];
    let has_content = after
        .trim_start_matches('/')
        .chars()
        .any(char::is_alphanumeric);
    has_content.then(|| Candidate::Url {
        url: text.to_owned(),
        span,
    })
}

/// Characters a URL can hold, taken liberally; the end is trimmed separately.
fn is_url_char(ch: char) -> bool {
    !ch.is_whitespace()
        && !ch.is_control()
        && !matches!(ch, '<' | '>' | '"' | '`' | '\\' | '^' | '{' | '}' | '|' | '…')
        // Box-drawing and block characters frame text in agent output.
        && !('\u{2500}'..='\u{259F}').contains(&ch)
}

/// Drop sentence punctuation and unbalanced closing brackets from the end of a URL, the way
/// Ghostty's own matcher does: `(https://example.com)` is the URL; the parentheses of
/// `https://en.wikipedia.org/wiki/Rust_(video_game)` are part of it.
fn trim_url(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start {
        let last = chars[end - 1];
        let count = |ch: char| chars[start..end].iter().filter(|c| **c == ch).count();
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' => true,
            ')' => count(')') > count('('),
            ']' => count(']') > count('['),
            _ => false,
        };
        if !drop {
            break;
        }
        end -= 1;
    }
    end
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The character can be part of a path token. `brackets` also admits `()[]`, which real paths
/// hold (`app/(auth)/[id].tsx`) and prose puts around them.
fn is_token_char(ch: char, brackets: bool) -> bool {
    ch.is_alphanumeric()
        || matches!(
            ch,
            '_' | '-' | '.' | '/' | '~' | '@' | '+' | '%' | '#' | ':'
        )
        || (brackets && matches!(ch, '(' | ')' | '[' | ']'))
}

fn token_span(chars: &[char], index: usize, brackets: bool) -> Range<usize> {
    let mut start = index;
    while start > 0 && is_token_char(chars[start - 1], brackets) && index - start < MAX_TOKEN {
        start -= 1;
    }
    let mut end = index + 1;
    while end < chars.len() && is_token_char(chars[end], brackets) && end - index < MAX_TOKEN {
        end += 1;
    }
    start..end
}

/// Cut a token back to the path it holds: sentence punctuation, then brackets that are not
/// closed within it (`(docs/a.md)` is `docs/a.md`; `app/(auth)/page.tsx` stays whole).
fn trim_token(chars: &[char], span: Range<usize>) -> Range<usize> {
    let (mut start, mut end) = (span.start, span.end);
    loop {
        let before = (start, end);
        // A run of dots alone is a folder (`..`), not punctuation.
        while end > start
            && matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?')
            && !chars[start..end].iter().all(|ch| *ch == '.')
        {
            end -= 1;
        }
        for (open, close) in [('(', ')'), ('[', ']')] {
            let (mut unopened, mut unclosed) = (0, 0);
            for ch in &chars[start..end] {
                if *ch == open {
                    unclosed += 1;
                } else if *ch == close {
                    if unclosed > 0 {
                        unclosed -= 1;
                    } else {
                        unopened += 1;
                    }
                }
            }
            if unopened > 0 {
                if end > start && chars[end - 1] == close {
                    end -= 1;
                } else if start < end && chars[start] == close {
                    start += 1;
                }
            }
            if unclosed > 0 {
                if start < end && chars[start] == open {
                    start += 1;
                } else if end > start && chars[end - 1] == open {
                    end -= 1;
                }
            }
        }
        if (start, end) == before {
            return start..end;
        }
    }
}

/// The runs between a pair of quotes that contain `index`, innermost first.
fn quoted_spans(chars: &[char], index: usize) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    for quote in ['"', '\'', '`'] {
        let mut open = None;
        for (position, ch) in chars.iter().enumerate() {
            if *ch != quote {
                continue;
            }
            match open.take() {
                None => open = Some(position),
                Some(start) => {
                    if start < index && index < position {
                        spans.push(start + 1..position);
                    }
                }
            }
        }
    }
    spans.sort_by_key(|span| span.len());
    spans
}

/// Python's traceback names the line after the quoted file name: `File "a.py", line 12, in f`.
fn line_after_quote(chars: &[char], span: &Range<usize>) -> Option<u32> {
    let quote = *chars.get(span.end)?;
    if !matches!(quote, '"' | '\'') || span.start == 0 || chars[span.start - 1] != quote {
        return None;
    }
    let after: String = chars[span.end + 1..].iter().take(32).collect();
    let digits: String = after
        .strip_prefix(", line ")?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// A token as the paths it could be, with the position that follows it: `src/main.rs:120:5`,
/// `src/main.rs#L120`, `src/main.rs:120-135`. Nothing when it cannot be a path: no folder part
/// and no extension, a URL, or a number. The reading without a position comes first; the token
/// whole follows it when they differ, because a name can hold the characters that mark one
/// (`docs/C#Tools/readme.md`).
fn path_readings(token: &str) -> Vec<(String, Option<u32>, Option<u32>)> {
    let token = token.trim();
    if token.is_empty() || token.chars().count() > MAX_TOKEN || token.contains("://") {
        return Vec::new();
    }
    // A test id names a file and what to run in it: `tests/test_a.py::TestA::test_b`.
    let token = match token.split_once("::") {
        Some((file, _)) if !file.is_empty() => file,
        _ => token,
    };
    let (path, line, col) = split_position(token);
    let path = path.trim_end_matches(':');
    let whole = token.trim_end_matches(':');
    let mut readings: Vec<_> = path_shape(path, line, col).into_iter().collect();
    if whole != path {
        readings.extend(path_shape(whole, None, None));
    }
    readings
}

fn path_shape(
    path: &str,
    line: Option<u32>,
    col: Option<u32>,
) -> Option<(String, Option<u32>, Option<u32>)> {
    if path.chars().count() < 2 || !path.chars().any(char::is_alphanumeric) {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    // `.gitignore` has an empty stem and is a file all the same; `v1.2.3` and `3.14` are numbers.
    let looks_like_file = name.rsplit_once('.').is_some_and(|(_, extension)| {
        !extension.is_empty()
            && extension.len() <= 12
            && extension.chars().all(char::is_alphanumeric)
            && !extension.chars().all(|ch| ch.is_ascii_digit())
    });
    // `src/main.rs` and `docs/` are paths wherever they are; a bare `README.md` is one only
    // because it has an extension, so "and", "the" and "3.14" are not asked about.
    (path.contains('/') || looks_like_file).then(|| (path.to_owned(), line, col))
}

/// `path:12`, `path:12:5`, `path:12-30`, `path#L12`, `path#L12C5`, `path#L12-L30`: the path and
/// the first position.
fn split_position(token: &str) -> (&str, Option<u32>, Option<u32>) {
    // The last `#`: an anchor has no `/` in it, and a folder name can hold a `#`.
    if let Some((path, fragment)) = token.rsplit_once('#')
        && !fragment.contains('/')
    {
        let (line, col) = fragment_position(fragment);
        return (path, line, col);
    }
    let token = token.trim_end_matches(':');
    // From the right: optional `-end`, then `:col`, then `:line`.
    let mut path = token;
    let mut numbers = Vec::new();
    while numbers.len() < 2 {
        let Some((head, tail)) = path.rsplit_once(':') else {
            break;
        };
        let tail = tail.split('-').next().unwrap_or(tail);
        match tail.parse::<u32>() {
            Ok(number) if !tail.is_empty() && !head.is_empty() => {
                numbers.push(number);
                path = head;
            }
            _ => break,
        }
    }
    match numbers[..] {
        [line] => (path, Some(line), None),
        [col, line] => (path, Some(line), Some(col)),
        _ => (token, None, None),
    }
}

/// `L12`, `L12C5` and `L12-L30` after a `#`.
fn fragment_position(fragment: &str) -> (Option<u32>, Option<u32>) {
    let Some(rest) = fragment.strip_prefix('L') else {
        return (None, None);
    };
    let digits = |text: &str| -> (Option<u32>, usize) {
        let length = text.chars().take_while(char::is_ascii_digit).count();
        (text[..length].parse().ok(), length)
    };
    let (line, used) = digits(rest);
    let col = rest[used..]
        .strip_prefix('C')
        .and_then(|rest| digits(rest).0);
    (line, col)
}

// ---------------------------------------------------------------------------------------
// Asking the file system
// ---------------------------------------------------------------------------------------

/// Where a relative path can start from.
pub struct Bases<'a> {
    /// The directory the shell is in.
    pub cwd: &'a Path,
    /// The project or worktree root: tools print paths relative to the repository from inside a
    /// folder of it.
    pub root: Option<&'a Path>,
    pub home: Option<&'a Path>,
}

/// The first candidate that is a URL or names something that exists. A path that does not
/// exist is not a link.
#[cfg(test)]
pub fn resolve(candidates: &[Candidate], bases: &Bases) -> Option<Link> {
    resolve_which(candidates, bases).map(|(link, _)| link)
}

/// `resolve`, and which of the candidates it took.
fn resolve_which(candidates: &[Candidate], bases: &Bases) -> Option<(Link, usize)> {
    let link = |candidate: &Candidate| match candidate {
        Candidate::Url { url, .. } => Some(Link::Url(url.clone())),
        Candidate::Path {
            text, line, col, ..
        } => resolve_path(text, bases).map(|(path, is_dir)| {
            Link::Path(ResolvedPath {
                path,
                line: *line,
                col: *col,
                is_dir,
            })
        }),
    };
    candidates
        .iter()
        .enumerate()
        .find_map(|(taken, candidate)| link(candidate).map(|link| (link, taken)))
}

fn resolve_path(text: &str, bases: &Bases) -> Option<(PathBuf, bool)> {
    let attempts: Vec<PathBuf> = if let Some(rest) = text.strip_prefix("~/") {
        vec![bases.home?.join(rest)]
    } else if text.starts_with('/') {
        vec![PathBuf::from(text)]
    } else {
        // `git diff` names the old and new file `a/src/x.rs` and `b/src/x.rs`; the folder is
        // tried as written first.
        let stripped = text
            .strip_prefix("a/")
            .or_else(|| text.strip_prefix("b/"))
            .filter(|rest| !rest.is_empty());
        [Some(text), stripped]
            .into_iter()
            .flatten()
            .flat_map(|name| {
                [Some(bases.cwd), bases.root]
                    .into_iter()
                    .flatten()
                    .map(move |base| base.join(name))
            })
            .collect()
    };
    attempts.into_iter().find_map(|attempt| {
        // `metadata` follows links; `canonicalize` also settles `..` the way the system does.
        let metadata = fs::metadata(&attempt).ok()?;
        if !metadata.is_file() && !metadata.is_dir() {
            return None;
        }
        let path = fs::canonicalize(&attempt).ok()?;
        Some((path, metadata.is_dir()))
    })
}

/// Where `path` sits inside `root`, as a path that starts with `root` as the caller spells it.
/// Both are compared with their links resolved (`/var` is `/private/var`), so a path from the
/// shell matches a root registered by either name.
pub fn within(root: &Path, path: &Path) -> Option<PathBuf> {
    let canonical_root = fs::canonicalize(root).ok()?;
    let canonical_path = fs::canonicalize(path).ok()?;
    let relative = canonical_path.strip_prefix(&canonical_root).ok()?;
    Some(root.join(relative))
}

/// The kinds of file `open` shows in an app and never runs: documents, pictures, sound and
/// video, data, and source code. A script language whose files a launcher may run (Python's
/// does) is left out.
const OPENABLE_EXTENSIONS: &[&str] = &[
    // Documents
    "pdf", "txt", "md", "markdown", "rst", "rtf", "tex", "csv", "tsv", "log", "doc", "docx", "xls",
    "xlsx", "ppt", "pptx", "key", "pages", "numbers", "epub", "odt", "ods", "odp",
    // Data and markup
    "json", "jsonl", "yaml", "yml", "toml", "xml", "ini", "cfg", "conf", "plist", "html", "htm",
    "css", "svg", "sql", "graphql", "proto", // Pictures, sound and video
    "png", "jpg", "jpeg", "gif", "webp", "heic", "tif", "tiff", "bmp", "ico", "mp3", "m4a", "wav",
    "aac", "flac", "ogg", "mp4", "mov", "m4v", "webm", "avi", "mkv", // Source code
    "rs", "c", "h", "cc", "cpp", "hpp", "m", "mm", "swift", "go", "java", "kt", "kts", "scala",
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "zig", "dart", "cs", "hs", "ex", "exs", "erl", "clj",
    "ml", "vue", "svelte", "lua",
];

/// Whether the system `open` may be handed this path. It launches applications, mounts and
/// installs, and runs scripts and executables, and a terminal link is text that anyone's output
/// can put on screen. A file that is not plainly a document, a picture or source code is shown in
/// Finder instead.
pub fn open_refusal(path: &Path) -> Option<&'static str> {
    use std::os::unix::fs::MetadataExt;
    let Ok(metadata) = fs::metadata(path) else {
        return Some("It does not exist.");
    };
    let extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default();
    let is = |list: &[&str]| {
        list.iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    };
    if is(crate::file_explorer::LAUNCHER_EXTENSIONS) {
        Some("Applications and scripts are not opened from a terminal link.")
    } else if metadata.is_file() && metadata.mode() & 0o111 != 0 {
        Some("Executable files are not opened from a terminal link.")
    } else if metadata.is_file() && !is(OPENABLE_EXTENSIONS) {
        Some("Only documents, pictures and source files are opened from a terminal link.")
    } else {
        None
    }
}

/// `path` relative to nothing in particular, for a message: the last two components.
pub fn short_name(path: &Path) -> String {
    let parts: Vec<_> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let from = parts.len().saturating_sub(2);
    parts[from..].join("/")
}
