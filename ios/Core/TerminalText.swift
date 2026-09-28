import Foundation

public enum TerminalText {
    /// Render terminal snapshots as readable text, removing ANSI control strings and CR overprints.
    public static func readable(_ input: String) -> String {
        var text = input
        // OSC strings end at the first BEL or ESC \; a greedy body would swallow visible text up to a later terminator.
        for pattern in ["\\u001B\\](?:(?!\\u0007|\\u001B\\\\)[^\\n])*(?:\\u0007|\\u001B\\\\)", "\\u001B\\[[0-?]*[ -/]*[@-~]", "\\u001B[()][0-2A-Z]", "\\u001B[@-_]"] {
            if let regex = try? NSRegularExpression(pattern: pattern) {
                text = regex.stringByReplacingMatches(in: text, range: NSRange(text.startIndex..., in: text), withTemplate: "")
            }
        }
        let readable = text.replacingOccurrences(of: "\r\n", with: "\n")
            .components(separatedBy: "\n").map { $0.components(separatedBy: "\r").last ?? "" }.joined(separator: "\n")
            .filter { $0 == "\n" || $0 == "\t" || $0.unicodeScalars.allSatisfy { $0.value >= 32 && $0.value != 127 && $0.properties.generalCategory != .privateUse } }
        // tmux snapshots include empty screen rows and font-specific prompt glyphs.
        var lines = readable.components(separatedBy: "\n")
        while lines.last?.trimmingCharacters(in: .whitespaces).isEmpty == true { lines.removeLast() }
        return lines.joined(separator: "\n")
    }
}
