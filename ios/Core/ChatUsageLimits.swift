import Foundation

// The provider's usage windows (`rate_limits`, docs/chat-notices.md "Rate windows"): usage data, never a banner. The chat's row shows
// one compact chip for the fullest window at or past its own warning threshold ("⚠ weekly 87% · resets Thu 14:00"), bold from 90 %;
// a tap lists every window. A reached limit is a notice of its own (`rate_limit:*` at error level), shown as a banner.

/// One usage window of the provider (`RateWindow`).
public struct ChatRateWindow: Sendable, Equatable, Codable, Identifiable {
    /// Claude's window name (`five_hour`, `seven_day`, …) or Codex `primary` / `secondary`.
    public let id: String
    /// "5h", "weekly", "weekly Opus", …
    public let label: String
    /// 0–100.
    public let usedPercent: Double
    /// Unix seconds; nil when unknown.
    public let resetsAt: UInt64?
    /// From this share on the window is worth a chip (Claude 70, Codex 75 or 50).
    public let warnAt: Double

    public init(id: String, label: String, usedPercent: Double, resetsAt: UInt64? = nil, warnAt: Double = 70) {
        self.id = id; self.label = label; self.usedPercent = usedPercent; self.resetsAt = resetsAt; self.warnAt = warnAt
    }
    private enum Keys: String, CodingKey { case id, label, usedPercent = "used_percent", resetsAt = "resets_at", warnAt = "warn_at" }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        id = try c.decode(String.self, forKey: .id)
        let label = c.tolerant(String.self, forKey: .label)?.trimmingCharacters(in: .whitespaces) ?? ""
        self.label = label.isEmpty ? id : label
        let used = try c.decode(Double.self, forKey: .usedPercent)
        guard used.isFinite else { throw DecodingError.dataCorruptedError(forKey: .usedPercent, in: c, debugDescription: "not a share") }
        usedPercent = min(max(used, 0), 100)
        resetsAt = c.tolerant(UInt64.self, forKey: .resetsAt) ?? c.tolerant(Double.self, forKey: .resetsAt).flatMap { $0 >= 0 && $0 < 1e15 ? UInt64($0) : nil }
        warnAt = c.tolerant(Double.self, forKey: .warnAt).flatMap { $0.isFinite ? $0 : nil } ?? 70
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(id, forKey: .id); try c.encode(label, forKey: .label); try c.encode(usedPercent, forKey: .usedPercent)
        try c.encodeIfPresent(resetsAt, forKey: .resetsAt); try c.encode(warnAt, forKey: .warnAt)
    }
    /// A window whose reset has passed is the provider's old data: kept, not shown.
    public func isExpired(now: Date) -> Bool { resetsAt.map { TimeInterval($0) <= now.timeIntervalSince1970 } ?? false }
    /// "87%".
    public var percentText: String { "\(Int(usedPercent.rounded()))%" }
}

public enum ChatUsageLimits {
    /// The chip of the chat's row: the fullest live window at or past its warning threshold; nil when none is.
    public struct Chip: Sendable, Equatable {
        public let window: ChatRateWindow
        /// "weekly 87%".
        public let text: String
        /// "resets Thu 14:00", when the reset is known.
        public let resetText: String?
        /// 90 % or more.
        public let bold: Bool
        public var spoken: String { [text.replacingOccurrences(of: "%", with: " percent used"), resetText].compactMap { $0 }.joined(separator: ", ") }
    }
    public static func chip(_ windows: [ChatRateWindow], now: Date = .now, calendar: Calendar = .current, locale: Locale = .current) -> Chip? {
        guard let window = windows.filter({ !$0.isExpired(now: now) && $0.usedPercent >= $0.warnAt }).max(by: { $0.usedPercent < $1.usedPercent }) else { return nil }
        return Chip(window: window, text: "\(window.label) \(window.percentText)", resetText: window.resetsAt.map { "resets " + resetTime($0, calendar: calendar, locale: locale) },
                    bold: window.usedPercent >= 90)
    }
    /// Every window worth listing (the chip's detail), fullest first.
    public static func live(_ windows: [ChatRateWindow], now: Date = .now) -> [ChatRateWindow] {
        windows.filter { !$0.isExpired(now: now) }.sorted { $0.usedPercent > $1.usedPercent }
    }
    /// "Thu 14:00", in local time.
    public static func resetTime(_ seconds: UInt64, calendar: Calendar = .current, locale: Locale = .current) -> String {
        let formatter = DateFormatter()
        formatter.calendar = calendar; formatter.locale = locale; formatter.timeZone = calendar.timeZone
        formatter.setLocalizedDateFormatFromTemplate("EEE HH:mm")
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(seconds)))
    }
}
