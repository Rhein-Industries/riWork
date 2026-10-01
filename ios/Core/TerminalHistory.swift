import Foundation

/// What scrolling added to `shell.output` results, in both the full and the `unchanged` form. Both fields are absent on older
/// desktops; a malformed one is ignored, never a reason to lose the screen.
public struct OutputExtras: Sendable, Equatable {
    /// The number of scrollback lines above the screen.
    public let historySize: Int?
    /// A full-screen program is on the alternate screen, which has no scrollback.
    public let alternate: Bool?
    public init(result: JSONValue) {
        if case .number(let number) = result["history_size"], number.isFinite, number >= 0, number <= 1_000_000_000, number.rounded() == number { historySize = Int(number) }
        else { historySize = nil }
        if case .bool(let value) = result["alternate"] { alternate = value } else { alternate = nil }
    }
}

public enum HistoryLimits {
    /// Lines asked for per `shell.history` page when nothing is known about the link yet (and by the manual fetch the tests use).
    public static let pageLines = 300
    /// The smallest page tried after `response_too_large`.
    public static let minimumPageLines = 10
    /// The most the protocol allows per page.
    public static let maximumPageLines = 1000
    /// Scrollback lines kept on the phone. Older ones are not asked for, and the oldest held go first when live output pushes past it.
    ///
    /// A held line costs about 285 bytes on the heap (measured: 50,000 styled lines of ordinary build and test output came to 13.6 MB,
    /// 32 of those bytes the array slot), so the cap is about 14 MB for one shell and 28 MB at the desktop's own limit of 100,000. It
    /// travels as roughly 3 MB of text (about 57 bytes a line, styled).
    public static let heldLines = 50_000
    /// The desktop keeps at most this many lines of history; one that has reached it drops its oldest as new ones arrive.
    public static let desktopHistoryLines = 100_000
    /// Lines of the loaded history that a page overlaps, once the desktop's history has been seen dropping lines it cannot announce.
    public static let verifyLines = 8
    /// Lines of scrollback every live answer asks for until the phone knows it can page (and always on a desktop that cannot): the
    /// first answer then fills the screen with recent history at once.
    public static let fullScrollbackLines = 500
    /// Lines of scrollback a live answer asks for once older lines come from `shell.history`. They only have to bridge the output
    /// that arrives between two answers (a burst beyond that is a hole, fetched back as history); the rest of the 500 was sent, parsed
    /// and compared on every keystroke's echo for nothing.
    public static let liveScrollbackLines = 120
    /// The oldest lines are kept past the cap for this long (in lines) while the reader is looking at them, so they do not go from under
    /// the eyes; past it they go anyway.
    public static let trimDeferralLines = 12_500

    /// The `lines` of the next live answer. `fullAgain` is set when the history is full on the desktop (its shift cannot be announced,
    /// so the answer has to reach back far enough to match lines).
    public static func liveLines(pagingProved: Bool, fullAgain: Bool) -> Int {
        pagingProved && !fullAgain ? liveScrollbackLines : fullScrollbackLines
    }
}

/// One `shell.history` request: the scrollback page just above the lines the phone already has.
///
/// `end` is how many scrollback lines directly above the screen to skip (0: the page ends right above the screen); the page covers
/// the lines `-(end+lines)` … `-(end+1)` counted from the top of the screen, top to bottom, clamped at the top of history.
public struct HistoryRequest: Sendable, Equatable {
    public var shellID: String
    public var end: Int
    public var lines: Int
    public var styled: Bool
    public init(shellID: String, end: Int, lines: Int, styled: Bool = true) {
        self.shellID = shellID
        self.end = max(0, end)
        self.lines = max(1, min(HistoryLimits.maximumPageLines, lines))
        self.styled = styled
    }
    /// `styled` is left out when false (plain text), like `OutputRequest`.
    public var params: [String: JSONValue] {
        var params: [String: JSONValue] = ["shell_id": .string(shellID), "end": .number(Double(end)), "lines": .number(Double(lines))]
        if styled { params["styled"] = .bool(true) }
        return params
    }
    /// The same request without the newer `styled` field, for a desktop that rejects it.
    public var plain: HistoryRequest { var copy = self; copy.styled = false; return copy }
}

/// What the desktop answered to `shell.history`: `{"shell_id","output","line_count","history_size","complete"}`.
public struct HistoryReply: Sendable, Equatable {
    public let shellID: String
    public let text: String
    /// How many lines `text` holds, as the desktop counted them.
    public let lineCount: Int?
    /// The desktop's `history_size` when the page was taken. It is larger than the phone's when lines scrolled in meanwhile.
    public let historySize: Int?
    /// No older lines exist above this page.
    public let complete: Bool
    /// About what the page weighed on the wire: its text, with every escape character counted as the six bytes `\u001b` that JSON
    /// makes of it. This is what the link was measured with.
    public let wireBytes: Int

    public init(result: JSONValue) throws {
        guard let shellID = result["shell_id"].string, let text = result["output"].string else { throw RemoteError.protocolViolation("Session history identity mismatch.") }
        self.shellID = shellID; self.text = text
        func count(_ value: JSONValue) -> Int? {
            guard case .number(let number) = value, number.isFinite, number >= 0, number <= 1_000_000_000, number.rounded() == number else { return nil }
            return Int(number)
        }
        lineCount = count(result["line_count"])
        historySize = count(result["history_size"])
        if case .bool(let done) = result["complete"] { complete = done } else { complete = false }
        wireBytes = Self.wireBytes(of: text)
    }

    /// The text's UTF-8 bytes, plus five for each ESC (JSON writes it as `\u001b`).
    public static func wireBytes(of text: String) -> Int {
        var escapes = 0
        var total = 0
        for byte in text.utf8 { total += 1; if byte == 0x1B { escapes += 1 } }
        return total + 5 * escapes
    }
}

extension RemoteError {
    /// `response_too_large`: the page did not fit the reply cap, so ask for half the lines.
    public var asksForFewerLines: Bool {
        if case .rpc(let code, _) = self { return code == "response_too_large" }
        return false
    }
}
