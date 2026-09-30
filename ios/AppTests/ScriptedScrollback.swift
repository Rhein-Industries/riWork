import Foundation
import RiWorkCore

/// A desktop's scrollback, as tmux keeps it: lines numbered in the order they were written (`L<serial>`), the last `rows` of them on
/// the screen and the rest in history, which drops its oldest lines once it holds `cap`. Answers like `shell.output` and
/// `shell.history` do, so a test can check the phone against what the desktop really has.
struct ScriptedScrollback: Sendable {
    var serials: [Int]
    let rows: Int
    var cap: Int?
    /// One line (by serial) that is far wider than any screen, for checking that long lines are clipped rather than scrolled.
    var longLine: Int?
    private var next: Int
    init(history: Int, rows: Int = 12, cap: Int? = nil) {
        self.rows = rows; self.cap = cap
        serials = Array(0..<(history + rows))
        next = history + rows
    }
    var historySize: Int { max(0, serials.count - rows) }
    static func text(_ serial: Int) -> String { "L\(serial)" }
    private func text(of serial: Int) -> String { serial == longLine ? "L\(serial) " + String(repeating: "x", count: 400) : Self.text(serial) }

    mutating func write(_ count: Int) {
        for _ in 0..<count { serials.append(next); next += 1 }
        if let cap, historySize > cap { serials.removeFirst(historySize - cap) }
    }
    private func join(_ slice: ArraySlice<Int>) -> String { slice.map(text(of:)).joined(separator: "\n") + "\n" }

    /// The fields `shell.output` adds to its result beyond `output` (which it replaces).
    func outputFields(lines: Int, reportsHistorySize: Bool, alternate: Bool?) -> [String: JSONValue] {
        var fields: [String: JSONValue] = ["rows": .number(Double(rows)), "cols": .number(80), "in_mode": .bool(false), "cursor": .object(["x": .number(0), "y": .number(Double(rows - 1))])]
        if alternate == true {
            // A full-screen program: no scrollback, just its own screen.
            fields["output"] = .string((0..<rows).map { "ALT \($0)" }.joined(separator: "\n") + "\n")
            fields["alternate"] = .bool(true)
            if reportsHistorySize { fields["history_size"] = .number(0) }
            return fields
        }
        let history = min(max(0, lines), historySize)
        fields["output"] = .string(join(serials.suffix(history + rows)))
        if reportsHistorySize { fields["history_size"] = .number(Double(historySize)) }
        if let alternate { fields["alternate"] = .bool(alternate) }
        return fields
    }
    /// `shell.history`: lines -(end+lines) … -(end+1) above the screen, clamped at the top of history.
    func historyFields(shellID: JSONValue, end: Int, lines: Int) -> [String: JSONValue] {
        let hist = historySize
        let top = min(hist, end + lines), bottom = min(hist, end)
        let first = serials.count - rows - hist
        let slice = serials[(first + hist - top)..<(first + hist - bottom)]
        return ["shell_id": shellID, "output": .string(slice.isEmpty ? "" : join(slice)), "line_count": .number(Double(slice.count)),
                "history_size": .number(Double(hist)), "complete": .bool(end + lines >= hist)]
    }
}
