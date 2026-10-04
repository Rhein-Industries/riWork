import SwiftUI
import UIKit
import RiWorkCore

/// What the surface reads its lines from: the model's `TerminalBuffer`. A copy of the buffer is a handful of words (its lines are one
/// array), but it must not be kept: a second reference to that array would make the next live answer copy all 50,000 lines.
@MainActor protocol TerminalLineSource: AnyObject {
    var terminalBuffer: TerminalBuffer { get }
    /// The desktop pane's width in cells, when the desktop reports it: a row that wide is taken to wrap onto the next (links).
    var terminalColumns: Int? { get }
}
extension TerminalLineSource {
    var terminalColumns: Int? { nil }
}

/// The row at the top of the loaded lines: "Loading…", "Beginning of history", or "Couldn't load, tap to retry".
struct HistoryHeader: Equatable {
    var text: String
    var failed: Bool
}

/// The scrollback of one shell on the iPhone: a scroll surface that draws only the rows in view.
///
/// **Why UIKit.** The old surface was a SwiftUI `ScrollView` with a `LazyVStack` and `scrollPosition(id:)`. With lines added above the
/// view (a page of history) SwiftUI has to find the anchored row again and move the offset; it did that after the layout, never in
/// the frame the page arrived, and not at all under a finger or in the momentum after it (the test of this on the iOS 26 simulator
/// fails to keep the reader in place). With tens of thousands of lines every body evaluation also diffs every identity. Here nothing
/// has to be found or corrected, because nothing moves:
///
/// - Every line has a fixed place, `index * lineHeight` from the top of the content (`TerminalScrollGeometry`). Lines added above
///   the view, or taken off the top, change no line's place. A page of history is therefore put in the moment it arrives, under a
///   moving finger or a fling, and the reader never notices. What changes is how far up the reader may scroll: the content inset.
/// - The scroll view is only the physics and the gestures. The rows are drawn into a plain view next to it, relative to the view (not
///   to a content 1.4 million points tall, where a single-precision coordinate would lose a fraction of a point): one view per row in
///   view, recycled, painted once, and moved by the compositor.
/// - A live answer reconfigures the visible rows and repaints the ones whose line differs, usually the last one or two.
///
/// The model owns the buffer and tells the surface when it changed (`refresh`); the surface tells the model where the reader is
/// (`onMetrics` for the follow logic, `onReader` for the prefetch).
@MainActor final class TerminalSurfaceView: UIView, UIScrollViewDelegate, @preconcurrency UIEditMenuInteractionDelegate, UIGestureRecognizerDelegate {
    struct Look: Equatable {
        var settings: TerminalRenderer.Settings
        /// The size being drawn (a pinch changes it before it is committed).
        var fontSize: Double
        var padding: Double
        /// Room kept under the last line for the pending-input chip and notices that float over the pane.
        var floatingInset: Double
        /// The size of the header row's text (Menlo), already scaled with the interface. Its color is the terminal's text at 55 %.
        var headerSize: Double
    }
    private var headerFont: UIFont { UIFont(name: "Menlo", size: look.headerSize) ?? .monospacedSystemFont(ofSize: look.headerSize, weight: .regular) }
    private var headerColor: UIColor {
        let rgb = look.settings.colors.foreground
        return UIColor(red: CGFloat(rgb.red) / 255, green: CGFloat(rgb.green) / 255, blue: CGFloat(rgb.blue) / 255, alpha: 0.55)
    }

    // MARK: Wiring
    weak var source: (any TerminalLineSource)? { didSet { if source !== oldValue { needsRefresh = true; setNeedsLayout() } } }
    /// Changes made from SwiftUI's update pass (`header`, `configure`, `jumpToken`) are applied in the next layout pass: the follow logic
    /// writes model state, which must not happen while SwiftUI is still evaluating views.
    var header: HistoryHeader? { didSet { if header != oldValue { needsRefresh = true; setNeedsLayout() } } }
    /// The follow logic: `(old, new, line height, userDriven)` → what to do.
    var onMetrics: ((ScrollMetrics?, ScrollMetrics, Double, Bool) -> StickyBottom.Response)?
    /// The reader moved: the index of the first line in view, and how many rows the view shows.
    var onReader: ((Int, Int) -> Void)?
    var isFollowing: () -> Bool = { true }
    var onRetry: (() -> Void)?
    /// A link was tapped: open it (Safari, Mail). Tests put a recorder here.
    var onOpenLink: ((URL) -> Void)?
    /// A touch came down, on a link or not. The SwiftUI tap that focuses the keyboard asks the model, so a tap on a link only opens it.
    var onLinkTouch: ((Bool) -> Void)?
    /// Counts requests to go to the bottom (typing, sending, the menu).
    var jumpToken = 0 { didSet { if jumpToken != oldValue { pendingJump = true; setNeedsLayout() } } }
    private var pendingJump = false
    private var pendingForce = false

    private(set) var look: Look
    private let scroll = UIScrollView()
    private let canvas = UIView()
    private let headerView = HeaderRowView()
    private var rows: [Int: TerminalRowView] = [:]
    private var spare: [TerminalRowView] = []
    private let highlight = UIView()
    /// Bumped when anything about how a row is painted changes, so that every row is painted again.
    private var lookID = 0
    private(set) var geometry: TerminalScrollGeometry
    private var anchorRow = 0
    private var lastEpoch: Int?
    private var lastMetrics: ScrollMetrics?
    private var pressedRow: Int?
    /// The links of the rows worked out since the buffer last changed, by line index. Rows are added as they come into view.
    private var linkRows: [Int: [TerminalLinkSpan]] = [:]
    /// The touch that is under way came down on a link of a still view (a tap that stops a fling opens nothing).
    private var touchOnLink = false
    /// The index of the first line held, and of the row that shows the header, when there is one.
    private var firstLine = 0
    private var headerRow: Int?

    init(look: Look) {
        self.look = look
        geometry = TerminalScrollGeometry(lineHeight: TerminalFont.cell(size: look.fontSize).height, firstRow: 0, endRow: 0, viewportHeight: 0)
        super.init(frame: .zero)
        clipsToBounds = true
        backgroundColor = .clear
        canvas.isUserInteractionEnabled = false
        canvas.clipsToBounds = false
        addSubview(canvas)
        headerView.isHidden = true
        canvas.addSubview(headerView)
        highlight.isHidden = true
        highlight.isUserInteractionEnabled = false
        canvas.addSubview(highlight)

        scroll.delegate = self
        scroll.backgroundColor = .clear
        scroll.contentInsetAdjustmentBehavior = .never
        scroll.showsHorizontalScrollIndicator = false
        scroll.alwaysBounceHorizontal = false
        scroll.isDirectionalLockEnabled = true
        addSubview(scroll)

        let tap = UITapGestureRecognizer(target: self, action: #selector(tapped(_:)))
        tap.cancelsTouchesInView = false
        tap.delegate = self
        scroll.addGestureRecognizer(tap)
        let press = UILongPressGestureRecognizer(target: self, action: #selector(pressed(_:)))
        press.minimumPressDuration = 0.45
        scroll.addGestureRecognizer(press)
        scroll.addInteraction(UIEditMenuInteraction(delegate: self))

        isAccessibilityElement = true
        accessibilityLabel = "Terminal output"
        accessibilityTraits = [.staticText, .updatesFrequently]
    }
    required init?(coder: NSCoder) { fatalError("not used") }

    var scrollView: UIScrollView { scroll }
    private var lineHeight: Double { geometry.lineHeight }

    // MARK: Configuration

    /// A new look (colors, size, padding, floating inset). Repaints what shows.
    func configure(_ next: Look) {
        guard next != look else { return }
        let sizeChanged = next.fontSize != look.fontSize
        let paints = next.settings != look.settings || sizeChanged || next.headerSize != look.headerSize
        look = next
        if paints { lookID += 1 }
        pendingForce = pendingForce || sizeChanged
        needsRefresh = true
        setNeedsLayout()
    }

    // MARK: Layout

    override func layoutSubviews() {
        super.layoutSubviews()
        // Only when different: UIKit settles a scroll view's offset into its limits whenever its frame is set, which would cut a bounce short.
        if scroll.frame != bounds { scroll.frame = bounds }
        if canvas.frame != bounds { canvas.frame = bounds }
        let force = pendingForce
        pendingForce = false
        // The model refreshes the surface itself whenever the buffer changes, so a layout pass has something to do only when the view or
        // what it was told changed. Doing it anyway reads the model's observable state in here, and UIKit (iOS 26) then lays the view out
        // again after every live answer, to do all of it a second time.
        if needsRefresh || force || bounds != laidOutBounds {
            laidOutBounds = bounds
            refresh(force: force)
        }
        if pendingJump { pendingJump = false; jumpToBottom() }
    }
    /// What a layout pass refreshes for: something the surface was told changed (its source, its header, its look), or its bounds did.
    private var needsRefresh = true
    private var laidOutBounds = CGRect.zero

    // MARK: Following the buffer

    /// The buffer changed, or the view did: works out where every line is, keeps the reader where they are (or at the bottom, when
    /// they were following it), and brings the rows that show up to date.
    func refresh(force: Bool = false) {
        guard let source, bounds.height > 0 else { return }
        Perf.count("surface.refresh")
        let signpost = Perf.signposter.beginInterval("SurfaceRefresh")
        defer { Perf.signposter.endInterval("SurfaceRefresh", signpost) }
        needsRefresh = false
        let buffer = source.terminalBuffer
        let hasHeader = header != nil && !buffer.isEmpty
        let height = TerminalFont.cell(size: look.fontSize).height
        let next = TerminalScrollGeometry(lineHeight: height, firstRow: hasHeader ? buffer.start - 1 : buffer.start, endRow: buffer.endIndex,
                                          topPadding: look.padding, bottomPadding: look.padding + look.floatingInset, viewportHeight: bounds.height)
        let old = geometry
        let firstTime = lastEpoch == nil
        let renumbered = lastEpoch != nil && lastEpoch != buffer.epoch
        let following = firstTime || isFollowing()
        let offset = scroll.contentOffset.y
        var target = next.offset(after: old, offset: offset, following: following, renumbered: renumbered)
        // The offset is the reader's own and is left alone, including past an end while it bounces back. Only a wall that moved past
        // the reader pushes them: lines gone from the top (the cap), or a view that grew taller than what is left under the reader.
        if !following, !scroll.isTracking {
            if next.minOffset > old.minOffset, target < next.minOffset { target = next.minOffset }
            if next.maxOffset < old.maxOffset, target > next.maxOffset { target = next.maxOffset }
        }
        geometry = next
        firstLine = buffer.start
        headerRow = hasHeader ? buffer.start - 1 : nil
        lastEpoch = buffer.epoch
        linkRows.removeAll(keepingCapacity: true)
        // The scroll view tells its delegate about the size and offset changes made here, which would lay the rows out and report the
        // metrics before this method does both, once, below.
        refreshing = true
        apply(next)
        if abs(target - offset) > 0.001 { scroll.setContentOffset(CGPoint(x: 0, y: target), animated: false) }
        refreshing = false
        if force || old.lineHeight != next.lineHeight { anchorRow = Int.min }
        layoutRows(buffer: buffer, repaint: true)
        emitMetrics()
    }
    /// `refresh` is moving the scroll view itself; it lays the rows out and reports the metrics when it is done.
    private var refreshing = false

    /// Hands the scroll view its content size and insets. Nothing else: the reader's offset is kept as it was when UIKit moves it
    /// along with an inset.
    private func apply(_ g: TerminalScrollGeometry) {
        let size = CGSize(width: bounds.width, height: g.contentHeight)
        if scroll.contentSize != size { scroll.contentSize = size }
        let inset = UIEdgeInsets(top: g.topInset, left: 0, bottom: g.bottomInset, right: 0)
        if scroll.contentInset != inset {
            let keep = scroll.contentOffset
            scroll.contentInset = inset
            if scroll.contentOffset != keep { scroll.contentOffset = keep }
        }
    }

    private func emitMetrics() {
        let metrics = geometry.metrics(offset: scroll.contentOffset.y)
        let moving = scroll.isTracking || scroll.isDragging || scroll.isDecelerating
        let response = onMetrics?(lastMetrics, metrics, geometry.lineHeight, moving)
        lastMetrics = metrics
        if response == .scrollToBottom { jumpToBottom() }
    }

    /// Goes to the newest line, at once.
    func jumpToBottom() {
        guard bounds.height > 0 else { return }
        scroll.setContentOffset(CGPoint(x: 0, y: geometry.maxOffset), animated: false)
    }

    // MARK: Rows

    /// Puts a view on every row in view, recycles those that left, and (with `repaint`) brings every row up to date with its line.
    private func layoutRows(buffer: TerminalBuffer?, repaint: Bool) {
        let offset = scroll.contentOffset.y
        let g = geometry
        let visible = g.visibleRows(offset: offset)
        var reposition = false
        if anchorRow == Int.min || abs(offset - Double(anchorRow) * g.lineHeight) > 1500 {
            anchorRow = g.topRow(offset: offset).row
            reposition = true
        }
        // The canvas shows the rows relative to the view; its origin is the only thing that moves while scrolling.
        let origin = CGPoint(x: 0, y: offset - Double(anchorRow) * g.lineHeight)
        if canvas.bounds.origin != origin { canvas.bounds.origin = origin }

        // Hiding and showing go to the layer: UIView's own `isHidden` also tells the focus system, which costs more than the rest of
        // putting a row in or out of use.
        for (index, view) in rows where !visible.contains(index) || index == headerRow {
            view.clear(); view.layer.isHidden = true
            spare.append(view); rows[index] = nil
        }
        if spare.count > 24 { for view in spare.suffix(spare.count - 24) { view.removeFromSuperview() }; spare.removeLast(spare.count - 24) }

        let width = max(0, bounds.width - 2 * look.padding)
        let needsLines = repaint || visible.contains { $0 != headerRow && rows[$0] == nil }
        let held = needsLines ? (buffer ?? source?.terminalBuffer) : nil
        for index in visible where index != headerRow {
            let view: TerminalRowView
            if let known = rows[index] {
                view = known
                if !reposition && !repaint { continue }
            } else {
                view = spare.popLast() ?? { let made = TerminalRowView(frame: .zero); canvas.insertSubview(made, belowSubview: highlight); return made }()
                view.layer.isHidden = false
                rows[index] = view
            }
            let frame = CGRect(x: look.padding, y: Double(index - anchorRow) * g.lineHeight, width: width, height: g.lineHeight)
            if view.frame != frame { view.frame = frame }
            guard let held else { continue }
            let line = held[index] ?? .missing
            let cursor = look.settings.showCursor && held.cursorIndex == index ? held.cursorColumn : nil
            view.configure(line: line, cursorColumn: cursor, links: links(row: index, in: held).map(\.range), look: lookID, settings: look.settings, fontSize: look.fontSize)
        }

        if let headerRow, visible.contains(headerRow) {
            if headerView.layer.isHidden { headerView.layer.isHidden = false }
            let frame = CGRect(x: look.padding, y: Double(headerRow - anchorRow) * g.lineHeight, width: width, height: g.lineHeight)
            if headerView.frame != frame { headerView.frame = frame }
            headerView.configure(text: header?.text ?? "", font: headerFont, color: headerColor)
        } else if !headerView.layer.isHidden {
            headerView.layer.isHidden = true
        }
        if let pressed = pressedRow {
            highlight.frame = CGRect(x: 0, y: Double(pressed - anchorRow) * g.lineHeight, width: bounds.width, height: g.lineHeight)
        }
        // Where the reader is, for the prefetch: the first line in view, not the header's slot.
        if !visible.isEmpty { onReader?(max(firstLine, visible.lowerBound), max(1, Int(bounds.height / g.lineHeight))) }
    }

    // MARK: UIScrollViewDelegate

    func scrollViewDidScroll(_ scrollView: UIScrollView) {
        guard !refreshing else { return }
        layoutRows(buffer: nil, repaint: false)
        emitMetrics()
    }

    // MARK: Tap, press and copy

    @objc private func tapped(_ recognizer: UITapGestureRecognizer) {
        tap(at: recognizer.location(in: scroll))
    }
    /// A tap at a point of the scroll view's own space: retries a failed header, or opens the link under it.
    func tap(at point: CGPoint) {
        defer { touchOnLink = false }
        if header?.failed == true, let headerRow, row(at: point) == headerRow { onRetry?(); return }
        guard touchOnLink, pressedRow == nil, let link = link(at: point) else { return }
        onOpenLink?(link)
    }
    /// A touch came down at a point of the scroll view's own space. Called before any gesture ends, so the keyboard tap can tell.
    func touchBegan(at point: CGPoint) {
        touchOnLink = !scroll.isDecelerating && link(at: point) != nil
        onLinkTouch?(touchOnLink)
    }
    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer, shouldReceive touch: UITouch) -> Bool {
        touchBegan(at: touch.location(in: scroll))
        return true
    }

    // MARK: Links

    /// The links on a row, worked out once per state of the buffer. A row that is part of a wrapped line is read with the rest of it,
    /// and every row of that line is kept.
    private func links(row index: Int, in buffer: TerminalBuffer) -> [TerminalLinkSpan] {
        if let known = linkRows[index] { return known }
        guard let line = buffer[index], !line.isMissing else { return [] }
        let columns = source?.terminalColumns
        let rows = TerminalLinks.wrappedRows(around: index, wrapColumns: columns) { buffer[$0] }
        if rows.count == 1 {
            let found = TerminalLinks.spans(in: line.text)
            linkRows[index] = found
            return found
        }
        let found = TerminalLinks.spans(inWrapped: rows.map { buffer[$0] ?? .missing })
        for (offset, row) in rows.enumerated() { linkRows[row] = found[offset] }
        return linkRows[index] ?? []
    }

    /// The link under a point of the scroll view's own space. A finger is wider than a cell: a column either side still counts.
    func link(at point: CGPoint) -> URL? {
        guard let index = row(at: point), index >= firstLine, let buffer = source?.terminalBuffer, let line = buffer[index] else { return nil }
        let spans = links(row: index, in: buffer)
        guard !spans.isEmpty else { return nil }
        let cellWidth = TerminalFont.cell(size: look.fontSize).width
        let x = Double(point.x) - look.padding
        guard x >= -cellWidth else { return nil }
        // The character under each cell: a wide character covers two.
        let tapped = Int((x / cellWidth).rounded(.down))
        var column = 0, character = 0
        var hit: Int?
        for glyph in line.text {
            let width = TerminalText.cellWidth(glyph)
            if tapped < column + width { hit = character; break }
            column += width; character += 1
        }
        let at = hit ?? character
        for candidate in [at, at - 1, at + 1] {
            if let span = spans.first(where: { $0.range.contains(candidate) }) { return span.url }
        }
        return nil
    }

    /// The row under a point of the scroll view's own space (offset included).
    private func row(at point: CGPoint) -> Int? {
        let index = Int(((Double(point.y)) / geometry.lineHeight).rounded(.down))
        return index >= geometry.firstRow && index < geometry.endRow ? index : nil
    }

    @objc private func pressed(_ recognizer: UILongPressGestureRecognizer) {
        guard recognizer.state == .began, let index = row(at: recognizer.location(in: scroll)), index >= firstLine,
              let source, let line = source.terminalBuffer[index], !line.isMissing else { return }
        pressedRow = index
        highlight.backgroundColor = headerColor.withAlphaComponent(0.18)
        highlight.isHidden = false
        layoutRows(buffer: nil, repaint: false)
        // The menu belongs to the scroll view, so the point is in its space (offset included).
        let interaction = scroll.interactions.compactMap { $0 as? UIEditMenuInteraction }.first
        interaction?.presentEditMenu(with: UIEditMenuConfiguration(identifier: nil, sourcePoint: recognizer.location(in: scroll)))
    }
    func editMenuInteraction(_ interaction: UIEditMenuInteraction, menuFor configuration: UIEditMenuConfiguration, suggestedActions: [UIMenuElement]) -> UIMenu? {
        guard let index = pressedRow, let buffer = source?.terminalBuffer else { return nil }
        let line = Self.copyable(buffer[index]?.text ?? "")
        let visible = geometry.visibleRows(offset: scroll.contentOffset.y)
        let screen = visible.compactMap { buffer[$0] }.filter { !$0.isMissing }.map { Self.copyable($0.text) }.joined(separator: "\n")
        return UIMenu(children: [
            UIAction(title: "Copy line", image: UIImage(systemName: "doc.on.doc")) { _ in UIPasteboard.general.string = line },
            UIAction(title: "Copy lines in view", image: UIImage(systemName: "text.alignleft")) { _ in UIPasteboard.general.string = screen }
        ])
    }
    func editMenuInteraction(_ interaction: UIEditMenuInteraction, willDismissMenuFor configuration: UIEditMenuConfiguration, animator: any UIEditMenuInteractionAnimating) {
        animator.addCompletion { [weak self] in
            self?.pressedRow = nil
            self?.highlight.isHidden = true
        }
    }
    /// The text the way a person typed it: without the U+FE0E that makes symbols draw as text.
    static func copyable(_ text: String) -> String { text.replacingOccurrences(of: "\u{FE0E}", with: "") }

    // MARK: Accessibility

    /// VoiceOver's three-finger scroll: a page toward newer lines (up) or older ones (down).
    override func accessibilityScroll(_ direction: UIAccessibilityScrollDirection) -> Bool {
        let page = max(geometry.lineHeight, bounds.height - 2 * geometry.lineHeight)
        let current = scroll.contentOffset.y
        switch direction {
        case .up: scroll.setContentOffset(CGPoint(x: 0, y: min(geometry.maxOffset, current + page)), animated: false)
        case .down: scroll.setContentOffset(CGPoint(x: 0, y: max(geometry.minOffset, current - page)), animated: false)
        default: return false
        }
        UIAccessibility.post(notification: .pageScrolled, argument: nil)
        return true
    }
    override var accessibilityCustomActions: [UIAccessibilityCustomAction]? {
        get {
            var actions: [UIAccessibilityCustomAction] = []
            if header?.failed == true {
                actions.append(UIAccessibilityCustomAction(name: "Retry loading older lines") { [weak self] _ in self?.onRetry?(); return true })
            }
            // The links in view, each once, top to bottom.
            if let buffer = source?.terminalBuffer {
                var seen = Set<URL>()
                for index in geometry.visibleRows(offset: scroll.contentOffset.y) where index >= firstLine {
                    for span in links(row: index, in: buffer) where seen.insert(span.url).inserted {
                        let url = span.url
                        actions.append(UIAccessibilityCustomAction(name: "Open \(url.absoluteString)") { [weak self] _ in self?.onOpenLink?(url); return true })
                    }
                }
            }
            return actions.isEmpty ? nil : actions
        }
        set {}
    }

    override var accessibilityValue: String? {
        get {
            guard let buffer = source?.terminalBuffer else { return nil }
            let visible = geometry.visibleRows(offset: scroll.contentOffset.y)
            return visible.compactMap { buffer[$0] }.filter { !$0.isMissing }.map { Self.copyable($0.text) }.joined(separator: "\n")
        }
        set {}
    }

    // MARK: For tests

    /// The rows being shown, by line index.
    var shownRows: [Int: TerminalRowView] { rows }
    var headerShown: Bool { !headerView.isHidden }
    var headerRowText: String { headerView.text }
    /// The links of a row as the surface found them.
    func linkSpans(row index: Int) -> [TerminalLinkSpan] { source.map { links(row: index, in: $0.terminalBuffer) } ?? [] }
}

/// "Loading…" and its kin: one grid row of small, quiet text.
@MainActor final class HeaderRowView: UIView {
    private(set) var text = ""
    private var font = UIFont.monospacedSystemFont(ofSize: 10, weight: .regular)
    private var color = UIColor.secondaryLabel

    override init(frame: CGRect) {
        super.init(frame: frame)
        isOpaque = false; backgroundColor = .clear; isUserInteractionEnabled = false; clipsToBounds = true
    }
    required init?(coder: NSCoder) { fatalError("not used") }

    func configure(text: String, font: UIFont, color: UIColor) {
        guard text != self.text || font != self.font || color != self.color else { return }
        self.text = text; self.font = font; self.color = color
        setNeedsDisplay()
    }
    override func draw(_ rect: CGRect) {
        let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: color]
        let size = (text as NSString).size(withAttributes: attributes)
        (text as NSString).draw(at: CGPoint(x: 0, y: max(0, (bounds.height - size.height) / 2)), withAttributes: attributes)
    }
}

/// The terminal surface for SwiftUI.
struct TerminalSurface: UIViewRepresentable {
    let model: RemoteModel
    let look: TerminalSurfaceView.Look
    let header: HistoryHeader?
    let jumpToken: Int

    func makeUIView(context: Context) -> TerminalSurfaceView {
        let view = TerminalSurfaceView(look: look)
        view.source = model
        view.header = header
        view.isFollowing = { [weak model] in model?.scrollFollow.following ?? true }
        view.onMetrics = { [weak model] old, new, lineHeight, moving in
            model?.scrollMetricsChanged(from: old, to: new, lineHeight: lineHeight, userDriven: moving) ?? .none
        }
        view.onReader = { [weak model] top, rows in model?.noteReader(top: top, rows: rows) }
        view.onRetry = { [weak model] in model?.retryHistory() }
        view.onOpenLink = { url in UIApplication.shared.open(url) }
        view.onLinkTouch = { [weak model] onLink in model?.linkTouchedAt = onLink ? ProcessInfo.processInfo.systemUptime : nil }
        view.jumpToken = jumpToken
        model.attach(surface: view)
        return view
    }
    func updateUIView(_ view: TerminalSurfaceView, context: Context) {
        Perf.count("update.TerminalSurface")
        view.source = model
        view.header = header
        view.configure(look)
        view.jumpToken = jumpToken
    }
    static func dismantleUIView(_ view: TerminalSurfaceView, coordinator: ()) {
        view.onMetrics = nil; view.onReader = nil; view.onRetry = nil; view.onOpenLink = nil; view.onLinkTouch = nil
    }
}
