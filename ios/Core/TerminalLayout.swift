import Foundation

/// How much of the screen the terminal text gets. Both layouts keep the padding small so the grid is as large as possible.
public struct TerminalLayout: Sendable, Equatable {
    /// Space between the pane edge and the text, per edge, in points.
    public let padding: Double
    public static let normal = TerminalLayout(padding: 4)
    /// Focus mode: the shell edge to edge.
    public static let focus = TerminalLayout(padding: 2)

    /// Menlo advances 0.602 em and its line box is about 1.17 em, used where no font is available (tests, Core).
    public static func approximateCell(fontSize: Double) -> (width: Double, height: Double) { (fontSize * 0.602, (fontSize * 1.17).rounded(.up)) }

    public func viewport(width: Double, height: Double, cellWidth: Double, lineHeight: Double) -> TerminalViewport? {
        TerminalViewport.fit(width: width - 2 * padding, height: height - 2 * padding, cellWidth: cellWidth, lineHeight: lineHeight)
    }
}

/// The terminal font size the user picks, in points. Stored in UserDefaults by the app.
public enum TerminalFontSize {
    public static let range: ClosedRange<Double> = 8...24
    public static let standard = 12.0
    public static let step = 1.0
    public static func clamped(_ value: Double) -> Double { value.isFinite ? min(range.upperBound, max(range.lowerBound, value.rounded())) : standard }
    public static func stepped(_ value: Double, by steps: Int) -> Double { clamped(value + Double(steps) * step) }
    /// Pinch scale relative to the size when the gesture began. 1.0 means unchanged.
    public static func pinched(from start: Double, scale: Double) -> Double { scale.isFinite && scale > 0 ? clamped(start * scale) : clamped(start) }
}
