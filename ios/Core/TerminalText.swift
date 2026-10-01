import Foundation

public enum TerminalText {
    private static let stripPatterns: [NSRegularExpression] = [
        // OSC strings end at the first BEL or ESC \; a greedy body would swallow visible text up to a later terminator.
        "\\u001B\\](?:(?!\\u0007|\\u001B\\\\)[^\\n])*(?:\\u0007|\\u001B\\\\)", "\\u001B\\[[0-?]*[ -/]*[@-~]", "\\u001B[()][0-2A-Z]", "\\u001B[@-_]"
    ].compactMap { try? NSRegularExpression(pattern: $0) }

    /// Screen rows with escape sequences removed and CR overprints resolved. Row count is preserved (no trimming).
    /// With `keepCells`, glyphs the font cannot show (private use, e.g. prompt icons) become one space so columns still line up.
    static func rows(_ input: String, keepCells: Bool) -> [String] {
        var text = input
        for regex in stripPatterns { text = regex.stringByReplacingMatches(in: text, range: NSRange(text.startIndex..., in: text), withTemplate: "") }
        return text.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n").map { row in
            var out = ""
            for character in (row.components(separatedBy: "\r").last ?? "") {
                let scalars = character.unicodeScalars
                if scalars.allSatisfy({ $0.value >= 32 && $0.value != 127 && $0.properties.generalCategory != .privateUse }) { out.append(character) }
                else if keepCells, scalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) { out.append(" ") }
                else if character == "\t" { out.append(character) }
            }
            return out
        }
    }
    private static func trimmed(_ rows: [String]) -> [String] {
        var rows = rows
        // tmux snapshots include empty screen rows and font-specific prompt glyphs.
        while rows.last?.trimmingCharacters(in: .whitespaces).isEmpty == true { rows.removeLast() }
        return rows
    }

    /// Render terminal snapshots as readable text, removing ANSI control strings and CR overprints.
    public static func readable(_ input: String) -> String {
        trimmed(rows(input, keepCells: false)).joined(separator: "\n")
    }

    /// Terminal cells a character occupies: 2 for East Asian wide and emoji, 1 otherwise.
    public static func cellWidth(_ character: Character) -> Int {
        if character.isASCII { return 1 }
        guard let scalar = character.unicodeScalars.first else { return 1 }
        // Emoji presentation (✅ ✨ 🚀 …) is East Asian Wide, so tmux counts two cells.
        if scalar.properties.isEmojiPresentation { return 2 }
        switch scalar.value {
        case 0x1100...0x115F, 0x2E80...0xA4CF, 0xAC00...0xD7A3, 0xF900...0xFAFF, 0xFE30...0xFE6F, 0xFF00...0xFF60, 0xFFE0...0xFFE6,
             0x1F300...0x1F64F, 0x1F900...0x1F9FF, 0x20000...0x3FFFD: return 2
        default: return 1
        }
    }

    /// The readable text plus, when the desktop reported a cursor, where to draw it.
    ///
    /// `output` ends with the visible screen: its last `rows` lines. The cursor is `(x, y)` from that screen's first line.
    /// The cursor row keeps its blank neighbours (and gets padded) so the cursor always lands on a real character.
    public static func screen(_ input: String, cursor: (x: Int, y: Int)?, rows screenRows: Int?) -> TerminalScreen {
        guard let cursor, let screenRows, screenRows > 0, cursor.x >= 0, cursor.y >= 0, cursor.y < screenRows else {
            return TerminalScreen(text: readable(input), cursorOffset: nil)
        }
        var lines = rows(input, keepCells: true)
        // A trailing newline terminates the last line; it does not start another one.
        if lines.count > 1, lines.last == "" { lines.removeLast() }
        let row = max(0, lines.count - screenRows) + cursor.y
        while lines.count <= row { lines.append("") }
        while lines.count - 1 > row, lines.last?.trimmingCharacters(in: .whitespaces).isEmpty == true { lines.removeLast() }
        var characters = Array(lines[row])
        var cells = 0
        var index = 0
        while index < characters.count, cells + cellWidth(characters[index]) <= cursor.x { cells += cellWidth(characters[index]); index += 1 }
        // Past the end of the line: pad up to the column. On the second cell of a wide character, stay on that character.
        if index >= characters.count {
            if cells < cursor.x { characters.append(contentsOf: Array(repeating: " ", count: cursor.x - cells)) }
            index = characters.count
            characters.append(" ")
        }
        lines[row] = String(characters)
        let offset = lines[..<row].reduce(0) { $0 + $1.count + 1 } + index
        return TerminalScreen(text: lines.joined(separator: "\n"), cursorOffset: offset)
    }
}

public struct TerminalScreen: Sendable, Equatable {
    public let text: String
    /// Character offset of the cursor cell within `text`.
    public let cursorOffset: Int?
}

/// A `shell.output` result. `cursor`, `rows`, `cols` and `in_mode` are absent on older desktops.
public struct ShellOutput: Sendable, Equatable {
    public struct Cursor: Sendable, Equatable { public let x: Int; public let y: Int }
    public let shellID: String
    public let text: String
    public let cursor: Cursor?
    public let rows: Int?
    public let cols: Int?
    public let inMode: Bool
    /// The desktop's fingerprint of this screen. Present when the desktop can wait for a change (`if_changed`); absent on older ones.
    public let hash: String?
    /// How many scrollback lines lie above the screen on the desktop (`history_size`). Absent on desktops that cannot page history.
    public let historySize: Int?
    /// A full-screen program (vim, less, htop) is on the alternate screen (`alternate`). Absent on older desktops.
    public let alternate: Bool?

    public init(result: JSONValue) throws {
        guard let shellID = result["shell_id"].string, let text = result["output"].string else { throw RemoteError.protocolViolation("Session output identity mismatch.") }
        self.shellID = shellID; self.text = text
        // Optional fields are hints. A malformed one is ignored, never a reason to lose the screen.
        func count(_ value: JSONValue, maximum: Double) -> Int? {
            guard case .number(let number) = value, number.isFinite, number >= 0, number <= maximum, number.rounded() == number else { return nil }
            return Int(number)
        }
        let rows = count(result["rows"], maximum: 10_000).flatMap { $0 > 0 ? $0 : nil }
        self.rows = rows
        self.cols = count(result["cols"], maximum: 10_000).flatMap { $0 > 0 ? $0 : nil }
        if let x = count(result["cursor"]["x"], maximum: 10_000), let y = count(result["cursor"]["y"], maximum: 10_000), let rows, y < rows { cursor = Cursor(x: x, y: y) }
        else { cursor = nil }
        if case .bool(let mode) = result["in_mode"] { inMode = mode } else { inMode = false }
        hash = OutputReply.hash(result["hash"])
        let extras = OutputExtras(result: result)
        historySize = extras.historySize
        alternate = extras.alternate
    }
    public var screen: TerminalScreen { TerminalText.screen(text, cursor: cursor.map { ($0.x, $0.y) }, rows: rows) }
    /// The screen with its colors and attributes and text presentation forced on symbols. Pure and slow enough to keep off the main actor.
    public var styledScreen: StyledScreen { TerminalText.styledScreen(text, cursor: cursor.map { ($0.x, $0.y) }, rows: rows) }
    /// `styledScreen`, reusing the lines that `cache` already holds. The same screen, found with less work.
    public func styledScreen(cache: StyledLineCache?) -> StyledScreen { TerminalText.styledScreen(text, cursor: cursor.map { ($0.x, $0.y) }, rows: rows, cache: cache) }
}

/// How often to read the screen: fast right after keys were typed or sent, then slower, then the resting interval.
public struct PollCadence: Sendable, Equatable {
    public var resting: Duration
    public var fast: Duration = .milliseconds(300)
    public var fastWindow: TimeInterval = 2
    public var medium: Duration = .seconds(1)
    public var mediumWindow: TimeInterval = 10
    public init(resting: Duration) { self.resting = resting }

    /// `elapsed` is the time since the last typed or sent key, nil when there has been none.
    public func interval(sinceKeyActivity elapsed: TimeInterval?) -> Duration {
        guard let elapsed, elapsed >= 0 else { return resting }
        let wanted: Duration = elapsed < fastWindow ? fast : (elapsed < fastWindow + mediumWindow ? medium : resting)
        return min(wanted, resting)
    }
}
