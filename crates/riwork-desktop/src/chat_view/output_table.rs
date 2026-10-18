//! Table sizing and portable copies use rendered cell values, never UI controls.
use super::markdown::{Align, Span, plain_text};

pub(super) fn widths(header: &[Vec<Span>], rows: &[Vec<Vec<Span>>], wrapped: bool) -> Vec<f32> {
    (0..header.len())
        .map(|column| {
            let longest = std::iter::once(header)
                .chain(rows.iter().map(Vec::as_slice))
                .filter_map(|row| row.get(column))
                .map(|cell| {
                    plain_text(cell)
                        .lines()
                        .map(|line| line.chars().count())
                        .max()
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0);
            // Keep short status/number columns narrow, but bound a prose column so it can wrap.
            longest.clamp(6, if wrapped { 52 } else { 4096 }) as f32 * 7.0 + 18.0
        })
        .collect()
}

pub(super) fn csv(header: &[Vec<Span>], rows: &[Vec<Vec<Span>>]) -> String {
    std::iter::once(header)
        .chain(rows.iter().map(Vec::as_slice))
        .map(|row| {
            row.iter()
                .map(|cell| {
                    let text = plain_text(cell);
                    if text.contains([',', '"', '\n', '\r']) {
                        format!("\"{}\"", text.replace('"', "\"\""))
                    } else {
                        text
                    }
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

pub(super) fn markdown(align: &[Align], header: &[Vec<Span>], rows: &[Vec<Vec<Span>>]) -> String {
    let row = |cells: &[Vec<Span>]| {
        format!(
            "| {} |",
            cells
                .iter()
                .map(|cell| {
                    plain_text(cell)
                        .replace('\\', "\\\\")
                        .replace('|', "\\|")
                        .replace('\r', "")
                        .replace('\n', "<br>")
                })
                .collect::<Vec<_>>()
                .join(" | ")
        )
    };
    let divider = format!(
        "| {} |",
        (0..header.len())
            .map(|column| match align.get(column) {
                Some(Align::Center) => ":---:",
                Some(Align::Right) => "---:",
                _ => "---",
            })
            .collect::<Vec<_>>()
            .join(" | ")
    );
    std::iter::once(row(header))
        .chain(std::iter::once(divider))
        .chain(rows.iter().map(|cells| row(cells)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_view::markdown::{Block, parse};

    #[test]
    fn csv_preserves_quotes_commas_and_multiline_values() {
        let header = vec![
            super::super::markdown::inline("Name"),
            super::super::markdown::inline("Value"),
        ];
        let rows = vec![vec![
            super::super::markdown::inline("a, b"),
            super::super::markdown::inline("\"quoted\"\nnext"),
        ]];
        assert_eq!(
            csv(&header, &rows),
            "Name,Value\r\n\"a, b\",\"\"\"quoted\"\"\nnext\""
        );
    }

    #[test]
    fn markdown_copy_keeps_columns_pipes_and_alignment() {
        let Block::Table {
            align,
            header,
            rows,
        } = parse("| Name | Count |\n| :--- | ---: |\n| a\\|b | 2 |").remove(0)
        else {
            panic!()
        };
        let copied = markdown(&align, &header, &rows);
        let Block::Table {
            header: next_header,
            rows: next_rows,
            ..
        } = parse(&copied).remove(0)
        else {
            panic!()
        };
        assert_eq!(header, next_header);
        assert_eq!(rows, next_rows);
        assert!(copied.contains("---:"));
    }

    #[test]
    fn descriptive_columns_get_space_without_unbounded_width() {
        let header = vec![
            super::super::markdown::inline("OK"),
            super::super::markdown::inline("Description"),
        ];
        let rows = vec![vec![
            super::super::markdown::inline("yes"),
            super::super::markdown::inline(&"long ".repeat(200)),
        ]];
        let sizes = widths(&header, &rows, true);
        assert!(sizes[0] < sizes[1]);
        assert!(sizes[1] <= 382.0);
        assert!(widths(&header, &rows, false)[1] > sizes[1]);
    }
}
