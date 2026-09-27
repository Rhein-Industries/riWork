import Foundation

/// Character-cell viewport of the single visible terminal, excluding native chrome and keyboard.
public struct TerminalViewport: Codable, Sendable, Equatable, Hashable {
    public let columns: Int
    public let rows: Int
    public init(columns: Int, rows: Int) { self.columns = columns; self.rows = rows }
    public static func fit(width: Double, height: Double, cellWidth: Double, lineHeight: Double) -> TerminalViewport? {
        guard width.isFinite, height.isFinite, cellWidth.isFinite, lineHeight.isFinite,
              width > 0, height > 0, cellWidth > 0, lineHeight > 0 else { return nil }
        return TerminalViewport(columns: Int(min(300, max(20, width / cellWidth))), rows: Int(min(160, max(8, height / lineHeight))))
    }
}
