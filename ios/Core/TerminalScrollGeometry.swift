import Foundation

/// Where every line of the terminal sits in the scroll view, as plain arithmetic.
///
/// Lines are all one height (a monospace grid row, rounded to a whole pixel), so a line's place follows from its index alone:
/// **line `i` has its top edge at `i * lineHeight`** in the scroll view's content, counting from the first line the desktop ever had.
/// That is `TerminalBuffer`'s absolute index. Nothing about a line's place depends on how many lines are loaded above it, so a page
/// of older lines that arrives (or output that pushes the oldest lines out) changes no line's place at all and the view needs no
/// correction: only how far up the reader may scroll changes, and that is the content inset.
///
/// - `contentHeight` is the bottom edge of the last line: `endRow * lineHeight`.
/// - The top of the scrollable range is the first row (the header row when there is one) with the top padding above it:
///   `minOffset = firstRow * lineHeight - topPadding`. It is expressed as a negative top inset, `topInset = -minOffset`.
/// - The bottom is the last line with the bottom padding (and the room kept for floating status) under it.
/// - With less than a screenful the lines sit at the top, like a real terminal's.
///
/// Rows are drawn from the offset directly, relative to the view, so the large numbers (a 100,000-line history is 1.4 million points
/// down) never reach the renderer, which would lose a fraction of a point in single precision.
public struct TerminalScrollGeometry: Sendable, Equatable {
    public var lineHeight: Double
    /// Index of the first row that can be scrolled to: the first line held, or the header row above it.
    public var firstRow: Int
    /// One past the index of the last line.
    public var endRow: Int
    public var topPadding: Double
    public var bottomPadding: Double
    public var viewportHeight: Double

    public init(lineHeight: Double, firstRow: Int, endRow: Int, topPadding: Double = 0, bottomPadding: Double = 0, viewportHeight: Double) {
        self.lineHeight = max(1, lineHeight)
        self.firstRow = firstRow
        self.endRow = max(firstRow, endRow)
        self.topPadding = topPadding; self.bottomPadding = bottomPadding
        self.viewportHeight = max(0, viewportHeight)
    }

    /// The scroll view's `contentSize.height`.
    public var contentHeight: Double { Double(endRow) * lineHeight }
    /// The scroll view's `contentInset.top` (negative: the lines above the first row do not exist).
    public var topInset: Double { topPadding - Double(firstRow) * lineHeight }
    /// The scroll view's `contentInset.bottom`.
    public var bottomInset: Double { bottomPadding }
    /// The smallest content offset: the first row with its padding above.
    public var minOffset: Double { -topInset }
    /// The largest content offset: the last line with its padding under it at the bottom of the view. Never below `minOffset`.
    public var maxOffset: Double { max(minOffset, contentHeight + bottomInset - viewportHeight) }

    public func top(ofRow index: Int) -> Double { Double(index) * lineHeight }
    /// Where a row's top edge is in the view, `offset` being the scroll view's content offset.
    public func y(ofRow index: Int, offset: Double) -> Double { top(ofRow: index) - offset }

    /// The rows that are at least partly in view, none outside `firstRow..<endRow`. Empty when nothing is.
    public func visibleRows(offset: Double) -> Range<Int> {
        guard endRow > firstRow, viewportHeight > 0 else { return firstRow..<firstRow }
        let first = Int((offset / lineHeight).rounded(.down))
        let last = Int(((offset + viewportHeight) / lineHeight).rounded(.up))
        let low = min(endRow, max(firstRow, first)), high = min(endRow, max(low, last))
        return low..<high
    }

    /// The row at the top edge of the view, and how far (0 up to one line) it is scrolled out of sight.
    public func topRow(offset: Double) -> (row: Int, hidden: Double) {
        let row = Int((offset / lineHeight).rounded(.down))
        return (row, offset - Double(row) * lineHeight)
    }

    public func clamped(offset: Double) -> Double { min(maxOffset, max(minOffset, offset)) }

    /// How much of the way from the bottom the view is, in points: zero at the bottom.
    public func distanceFromBottom(offset: Double) -> Double { maxOffset - offset }

    /// The numbers `StickyBottom` follows. The offset is the scroll view's own, so lines added above change the content and the top
    /// inset but not the offset: the reader has not moved.
    public func metrics(offset: Double) -> ScrollMetrics {
        ScrollMetrics(offset: offset, contentHeight: contentHeight + bottomInset + topInset, viewportHeight: viewportHeight, topInset: topInset, bottomInset: 0)
    }

    /// The offset that keeps the same place in the text after the line height, the viewport or the rows changed from `old`.
    ///
    /// - Following the bottom: the new bottom.
    /// - Otherwise the line (and the fraction of it) at the top of the view stays there when only the line height changed; when the
    ///   lines were renumbered (`renumbered`), the distance from the bottom stays, since the bottom is the one place the old and the
    ///   new numbering share.
    public func offset(after old: TerminalScrollGeometry, offset: Double, following: Bool, renumbered: Bool = false) -> Double {
        if following { return maxOffset }
        if renumbered { return clamped(offset: maxOffset - old.distanceFromBottom(offset: offset)) }
        if old.lineHeight != lineHeight { return clamped(offset: offset / old.lineHeight * lineHeight) }
        return offset
    }
}
