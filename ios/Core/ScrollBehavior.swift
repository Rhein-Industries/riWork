import Foundation

/// What the terminal's scroll view reports, in points (SwiftUI's `ScrollGeometry`).
///
/// `offset` is `contentOffset.y`, which is negative by the top inset at rest. `viewportHeight` is the height of the view inside its
/// insets (`containerSize`): what is visible runs from `offset + topInset` for that height.
public struct ScrollMetrics: Sendable, Equatable {
    public var offset: Double
    public var contentHeight: Double
    public var viewportHeight: Double
    public var topInset: Double
    public var bottomInset: Double
    public init(offset: Double, contentHeight: Double, viewportHeight: Double, topInset: Double = 0, bottomInset: Double = 0) {
        self.offset = offset; self.contentHeight = contentHeight; self.viewportHeight = viewportHeight
        self.topInset = topInset; self.bottomInset = bottomInset
    }
    /// How far the last line's bottom edge is below the bottom of the visible part; zero at the bottom, negative while overscrolled.
    public var distanceFromBottom: Double { contentHeight - (offset + topInset + viewportHeight) }
    /// How far the first line's top edge is above the top of the visible part; zero at the top.
    public var distanceFromTop: Double { offset + topInset }
    /// The contents and the view changed size, not just the position in them.
    public func resized(since old: ScrollMetrics) -> Bool {
        old.contentHeight != contentHeight || old.viewportHeight != viewportHeight || old.topInset != topInset || old.bottomInset != bottomInset
    }
}

/// Whether the terminal follows new output, and what it says about output it is not following.
///
/// - The view follows while the user is at the bottom, or within one line of it.
/// - A scroll away from the bottom (a finger, or the momentum after it) stops following. Output then leaves what is being read where
///   it is, and is counted: `newLines` lines arrived since the user left the bottom.
/// - Reaching the bottom again, tapping the pill, typing or sending keys resumes following.
public struct StickyBottom: Sendable, Equatable {
    /// The bottom is this many lines wide: within it counts as being at the bottom.
    public static let slackLines = 1.0

    public private(set) var following = true
    /// Lines that arrived while not following.
    public private(set) var newLines = 0
    /// Scrolled further from the bottom than one screen.
    public private(set) var far = false
    private var lastEnd: Int?
    private var lastEpoch: Int?

    public init() {}

    public enum Response: Sendable, Equatable { case none, scrollToBottom }

    /// The pill that takes the user back to the latest output, or nil while following (or when there is nothing to come back to).
    public struct Pill: Sendable, Equatable {
        public let newLines: Int
        public var label: String { newLines > 0 ? "↓ Live · \(newLines) new" : "↓ Live" }
        public var accessibilityLabel: String {
            newLines > 0 ? "Jump to latest output, \(newLines) new \(newLines == 1 ? "line" : "lines")" : "Jump to latest output"
        }
    }
    public var pill: Pill? { !following && (newLines > 0 || far) ? Pill(newLines: newLines) : nil }

    /// The scroll view reported new numbers. `userDriven` is true while a finger or its momentum moves the view.
    ///
    /// Being at the bottom is following, however it came about. Away from it, following ends only when the view itself moved
    /// (a user scroll, or a position change with the contents unchanged): when the contents or the view merely grew, the bottom
    /// moved away from a view that was following, and the answer is `scrollToBottom`.
    public mutating func metricsChanged(from old: ScrollMetrics?, to new: ScrollMetrics, lineHeight: Double, userDriven: Bool) -> Response {
        let slack = max(1, lineHeight) * Self.slackLines
        let resized = old.map { new.resized(since: $0) } ?? true
        // Less than a point is layout rounding (a bar settling under the view), not a scroll.
        let moved = old.map { abs($0.offset - new.offset) >= 1 } ?? false
        far = new.distanceFromBottom > new.viewportHeight
        if new.distanceFromBottom <= slack {
            following = true; newLines = 0; far = false
            return .none
        }
        // The view itself moved (a finger, its momentum, or a scroll that changed nothing else): the reader left the bottom. A finger
        // that merely rests on the view while output arrives has not.
        if moved, userDriven || !resized {
            following = false
            return .none
        }
        return following && resized ? .scrollToBottom : .none
    }

    /// The buffer ends at absolute index `end` (one past its last line) in numbering `epoch`.
    public mutating func contentChanged(end: Int, epoch: Int) {
        defer { lastEnd = end; lastEpoch = epoch }
        guard let last = lastEnd, lastEpoch == epoch else { return }
        if !following, end > last { newLines += end - last }
    }

    /// The user asked for the latest output (the pill, typing, sending keys, a menu command).
    public mutating func stopFollowing() { following = false; far = true }

    public mutating func jumpToBottom() { following = true; newLines = 0; far = false }
    /// A shell was opened or switched to: at the bottom, nothing counted.
    public mutating func reset() { self = StickyBottom() }
}

/// Which rows of a full-screen program's screen the phone draws. The desktop's screen can have more rows than the pane has room for:
/// the keyboard came up and the desktop has not been resized yet, or the desktop keeps its own size. Then only the rows that fit are
/// drawn, as few as possible moved off the top so the cursor stays in view (the bottom ones, without a cursor). Drawing them all
/// would make the pane taller than the screen and push the whole screen up under the status bar and down under the keyboard.
public enum AlternateRows {
    /// Whole rows of `lineHeight` that fit `height` points, at least one.
    public static func fitting(height: Double, lineHeight: Double) -> Int {
        guard height.isFinite, lineHeight.isFinite, lineHeight > 0 else { return 1 }
        return max(1, Int((height / lineHeight + 0.001).rounded(.down)))
    }
    /// The rows, out of `count`, drawn in a pane of `fitting` rows with the cursor on row `cursor` (0 is the top), if there is one.
    public static func shown(count: Int, fitting: Int, cursor: Int?) -> Range<Int> {
        let count = max(0, count), fitting = max(1, fitting)
        guard count > fitting else { return 0..<count }
        let lowest = count - fitting
        let first = cursor.map { min(lowest, max(0, $0 - fitting + 1)) } ?? lowest
        return first..<(first + fitting)
    }
}

/// Turns vertical swipes into Page Up / Page Down, for a full-screen program on the alternate screen (vim, less, htop), which has
/// no scrollback to scroll.
///
/// Dragging the content down (positive translation) is Page Up; dragging it up is Page Down. One key per 80% of the view's height
/// dragged in the same direction, at most one per `minimumInterval`, and a fast swipe never queues more than two pages behind.
public struct SwipePager: Sendable, Equatable {
    public static let pageFraction = 0.8
    public static let defaultMinimumInterval = 0.12
    /// The most pages of unsent drag kept while the rate limit holds keys back.
    public static let backlogPages = 2.0

    public var minimumInterval: Double
    private var anchor = 0.0
    private var lastSent: Double?
    private var sentInDrag = false

    public init(minimumInterval: Double = SwipePager.defaultMinimumInterval) { self.minimumInterval = minimumInterval }

    /// A drag began.
    public mutating func begin() { anchor = 0; sentInDrag = false }

    /// The drag is now `translation` points from where it began (vertical; positive is downward). Returns the key to send, if one is due.
    public mutating func update(translation: Double, viewHeight: Double, now: Double) -> TerminalKey? {
        guard translation.isFinite, viewHeight.isFinite, viewHeight > 0 else { return nil }
        let page = viewHeight * Self.pageFraction
        let delta = translation - anchor
        guard abs(delta) >= page else { return nil }
        let direction = delta > 0 ? 1.0 : -1.0
        if let last = lastSent, now - last < minimumInterval {
            // Held back: drag beyond two pages of backlog is forgotten, so a long fast swipe does not keep paging afterwards.
            let backlog = Self.backlogPages * page
            if abs(delta) > backlog { anchor = translation - direction * backlog }
            return nil
        }
        anchor += direction * page
        // What is left over after this key may already be another page; keep at most `backlogPages` of it.
        let remaining = translation - anchor
        let backlog = Self.backlogPages * page
        if abs(remaining) > backlog { anchor = translation - (remaining > 0 ? 1.0 : -1.0) * backlog }
        lastSent = now
        sentInDrag = true
        return direction > 0 ? .pageUp : .pageDown
    }

    /// The drag ended. `predicted` is where its momentum would carry it: a flick that would go a page but did not get as far while the
    /// finger was down still pages once, so a quick swipe is not ignored.
    public mutating func end(predictedTranslation predicted: Double, viewHeight: Double, now: Double) -> TerminalKey? {
        defer { anchor = 0; sentInDrag = false }
        guard !sentInDrag, predicted.isFinite, viewHeight.isFinite, viewHeight > 0, abs(predicted) >= viewHeight * Self.pageFraction else { return nil }
        if let last = lastSent, now - last < minimumInterval { return nil }
        lastSent = now
        return predicted > 0 ? .pageUp : .pageDown
    }
}
