//! Links in terminal tabs: the URL or file under a ⌘-click, found in the text tmux shows.
//!
//! A tab's Ghostty surface is a tmux client, so the text on screen is read from the tmux
//! the shell lives in (`SessionManager::capture_link_view`) and never from Ghostty, which
//! has no API for it here. Engine owns capture parsing and path policy. This module
//! maps mouse gestures and Ghostty surface geometry onto those shared APIs.

use crate::theme::{GhosttyPadding, PaddingBalance};

mod overlay;
mod ui;
pub use overlay::Overlay;
pub use ui::LinkState;

#[cfg(test)]
mod tests;

pub use riwork_engine::terminal_links::*;

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
