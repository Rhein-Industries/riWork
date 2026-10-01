import Foundation

/// The lines of shells that are not on screen, kept so that going back to one (another tab, another project, a reconnect) does not
/// fetch its scrollback again.
///
/// A buffer is only ever restored into an empty one: the next live answer is lined up with the lines it holds by `history_size` and
/// by comparing text (`TerminalBuffer.applyLive`), and renumbers everything if they do not fit, so a shell that was cleared, re-wrapped
/// or reused while it was away costs one refetch and never shows old lines under new ones. A resize that changes the columns is
/// caught before that: lines wrapped at another width are not kept.
///
/// Memory is bounded: a few shells and a total of lines, the least recently used going first.
public struct TerminalCache: Sendable {
    public static let maximumShells = 4
    public static let maximumLines = 120_000

    private struct Entry: Sendable {
        var buffer: TerminalBuffer
        var columns: Int?
        var used: Int
    }
    private var entries: [String: Entry] = [:]
    private var clock = 0
    public init() {}

    public var count: Int { entries.count }
    public var lineCount: Int { entries.values.reduce(0) { $0 + $1.buffer.lines.count } }
    public func contains(_ key: String) -> Bool { entries[key] != nil }

    /// Keeps `buffer` for `key`, the grid being `columns` wide. An empty buffer is not worth keeping (and replaces nothing).
    public mutating func store(_ buffer: TerminalBuffer, key: String, columns: Int?) {
        guard !buffer.isEmpty else { return }
        clock += 1
        entries[key] = Entry(buffer: buffer, columns: columns, used: clock)
        while entries.count > Self.maximumShells || lineCount > Self.maximumLines, let oldest = entries.min(by: { $0.value.used < $1.value.used })?.key {
            // The newest entry is never the one to go, however large: it is what the reader just left.
            if oldest == key { break }
            entries[oldest] = nil
        }
    }

    /// Hands the buffer for `key` back and forgets it. Nil when there is none, or its lines were wrapped at another width.
    public mutating func take(key: String, columns: Int?) -> TerminalBuffer? {
        guard let entry = entries.removeValue(forKey: key) else { return nil }
        if let kept = entry.columns, let now = columns, kept != now { return nil }
        return entry.buffer
    }

    public mutating func remove(_ key: String) { entries[key] = nil }
    public mutating func removeAll() { entries = [:] }
}
