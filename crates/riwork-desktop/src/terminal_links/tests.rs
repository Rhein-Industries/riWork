use super::*;
use crate::theme::{GhosttyPadding, PaddingBalance};

#[test]
fn only_command_and_command_shift_make_a_link_gesture() {
    let command = gpui::Modifiers::command();
    assert_eq!(open_mode(command), Some(OpenMode::Open));
    assert_eq!(
        open_mode(gpui::Modifiers {
            shift: true,
            ..command
        }),
        Some(OpenMode::Edit)
    );
    assert_eq!(open_mode(gpui::Modifiers::default()), None);
    assert_eq!(open_mode(gpui::Modifiers::shift()), None);
    // Chords with Control or Option belong to the terminal.
    for chord in [
        gpui::Modifiers {
            control: true,
            ..command
        },
        gpui::Modifiers {
            alt: true,
            ..command
        },
        gpui::Modifiers {
            function: true,
            ..command
        },
    ] {
        assert_eq!(open_mode(chord), None);
    }
}

// ---- pointer to cell --------------------------------------------------------------------

const DEFAULT_PADDING: GhosttyPadding = GhosttyPadding::DEFAULT;

fn size(available: u32, cells: u32) -> f64 {
    cell_size(SizeRange::of(available, cells), available, cells)
}

#[test]
fn a_cell_is_the_whole_pixel_size_that_fits_the_grid() {
    // 1000 pt wide at 2x with 2 pt of padding each side leaves 1992 px, in which 124 cells of 16
    // px fit (and the 8 px left over are not a cell). Dividing by the count would say 16.06 and
    // put the last column 7 px off.
    assert_eq!(size(1992, 124), 16.0);
    // 1192 px of rows of 34: 35 fit.
    assert_eq!(size(1192, 35), 34.0);
    // A short axis allows two sizes (34 and 35 both give 17 rows in 600 px), so the middle is
    // taken to keep the far end within half a pixel per cell.
    assert_eq!(size(600, 17), 34.5);
    // A count that no size gives falls back to division.
    assert_eq!(size(10, 20), 0.5);
}

#[test]
fn a_taller_terminal_settles_the_cell_size_of_a_short_one() {
    let explicit = explicit_padding(&DEFAULT_PADDING, 2.0);
    let rows = |hint: &mut CellHint, height: u32, rows: u32| {
        grid_geometry(
            (2000, height),
            124,
            rows,
            explicit,
            PaddingBalance::False,
            hint,
        )
        .unwrap()
        .cell_height
    };
    // On its own, 21 rows in 747 px of space could be rows of 34 or of 35.
    let mut alone = CellHint::default();
    assert_eq!(rows(&mut alone, 755, 21), 34.5);
    // After a terminal with room for 35 rows of 34 px, it is known.
    let mut hint = CellHint::default();
    hint.for_scale(2.0);
    assert_eq!(rows(&mut hint, 1200, 35), 34.0);
    assert_eq!(rows(&mut hint, 755, 21), 34.0);
    // A terminal that contradicts them (a zoomed font) starts the record again, and the next
    // one is compared with that.
    assert_eq!(rows(&mut hint, 1000, 20), 48.5);
    assert_eq!(rows(&mut hint, 755, 15), 48.5);
    // Another display scale knows nothing of these sizes.
    hint.for_scale(1.0);
    assert_eq!(
        hint,
        CellHint {
            scale: 1000,
            ..CellHint::default()
        }
    );
}

#[test]
fn grid_and_pointer_follow_ghosttys_padding() {
    // 1000 x 600 pt at 2x: 2000 x 1200 px, 4 px padding, cells of 16 x 34.
    let size = (1000.0, 600.0);
    let cells = (124, 35);
    let cell = |x_px: f64, y_px: f64| {
        cell_under_pointer(
            size,
            (x_px / 2.0, y_px / 2.0),
            2.0,
            cells,
            &DEFAULT_PADDING,
            &mut CellHint::default(),
        )
    };
    // The first cell begins after the padding, not at the edge.
    assert_eq!(cell(4.0, 4.0), Some((0, 0)));
    assert_eq!(cell(3.9, 10.0), None);
    assert_eq!(cell(10.0, 3.9), None);
    assert_eq!(cell(19.9, 37.9), Some((0, 0)));
    assert_eq!(cell(20.0, 38.0), Some((1, 1)));
    // Cell 10, row 5: x = 4 + 16 * 10 + 8, y = 4 + 34 * 5 + 17.
    assert_eq!(cell(172.0, 191.0), Some((10, 5)));
    // The last column, whose position division by the count would get wrong.
    assert_eq!(cell(4.0 + 16.0 * 123.0 + 15.0, 100.0), Some((123, 2)));
    // Past the last cell: the 8 px left over and the padding.
    assert_eq!(cell(4.0 + 16.0 * 124.0, 100.0), None);
    assert_eq!(cell(100.0, 4.0 + 34.0 * 35.0), None);
}

#[test]
fn balanced_padding_centres_the_grid() {
    let screen = (2000, 1200);
    let explicit = explicit_padding(&DEFAULT_PADDING, 2.0);
    let plain = grid_geometry(
        screen,
        124,
        35,
        explicit,
        PaddingBalance::False,
        &mut CellHint::default(),
    )
    .unwrap();
    assert_eq!((plain.origin_x, plain.origin_y), (4.0, 4.0));
    // 2000 - 124 * 16 = 16 left over: 8 each side. 1200 - 35 * 34 = 10: 5 each side.
    let equal = grid_geometry(
        screen,
        124,
        35,
        explicit,
        PaddingBalance::Equal,
        &mut CellHint::default(),
    )
    .unwrap();
    assert_eq!((equal.origin_x, equal.origin_y), (8.0, 5.0));
    // `true` caps the top: half a cell plus the horizontal padding is 12 px, so 5 stands.
    let capped = grid_geometry(
        screen,
        124,
        35,
        explicit,
        PaddingBalance::True,
        &mut CellHint::default(),
    )
    .unwrap();
    assert_eq!((capped.origin_x, capped.origin_y), (8.0, 5.0));
    // With nearly a whole row left over (1223 px of rows of 34 hold 35 and 33 more), `equal`
    // gives the top 20 px and `true` stops at half a cell plus the side padding, 12.
    let tall = grid_geometry(
        (2000, 1231),
        124,
        35,
        explicit,
        PaddingBalance::Equal,
        &mut CellHint::default(),
    )
    .unwrap();
    assert_eq!(tall.origin_y, 20.0);
    let tall = grid_geometry(
        (2000, 1231),
        124,
        35,
        explicit,
        PaddingBalance::True,
        &mut CellHint::default(),
    )
    .unwrap();
    assert_eq!(tall.origin_y, 12.0);
}

#[test]
fn a_surface_too_small_for_its_grid_has_no_cells() {
    let explicit = explicit_padding(&DEFAULT_PADDING, 2.0);
    assert!(
        grid_geometry(
            (10, 10),
            80,
            24,
            explicit,
            PaddingBalance::False,
            &mut CellHint::default()
        )
        .is_none()
    );
    assert!(
        grid_geometry(
            (2000, 1200),
            0,
            24,
            explicit,
            PaddingBalance::False,
            &mut CellHint::default()
        )
        .is_none()
    );
    assert!(
        grid_geometry(
            (4, 4),
            1,
            1,
            explicit,
            PaddingBalance::False,
            &mut CellHint::default()
        )
        .is_none()
    );
}

#[test]
fn oversized_configured_padding_never_wraps_into_a_clickable_grid() {
    for padding in [
        GhosttyPadding {
            x: (u32::MAX, 1),
            ..DEFAULT_PADDING
        },
        GhosttyPadding {
            y: (u32::MAX, 1),
            ..DEFAULT_PADDING
        },
    ] {
        for scale in [1.0, 2.0] {
            assert!(
                cell_under_pointer(
                    (1000.0, 600.0),
                    (10.0, 10.0),
                    scale,
                    (80, 24),
                    &padding,
                    &mut CellHint::default(),
                )
                .is_none()
            );
        }
    }
}

// ---- tokens -----------------------------------------------------------------------------
