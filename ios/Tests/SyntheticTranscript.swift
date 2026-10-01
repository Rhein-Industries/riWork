import Foundation

/// Lines the way a styled terminal capture holds them (`capture-pane -e`): prose, an agent's tool calls and their results, diffs with
/// filled backgrounds, build output with colored words, box drawing, and the symbols the agents use. Deterministic: line `n` is always
/// the same text, so a benchmark compares like with like. Lines are at most `columns` cells wide (a phone's grid is about 50).
///
/// This file is compiled into both test bundles (the core tests and the app tests), so the benchmarks of the parse and the painting
/// work on the same content.
enum SyntheticTranscript {
    static let columns = 50

    /// A small, fixed hash: a line's pick of template and numbers.
    private static func mix(_ n: Int) -> Int {
        var x = UInt64(truncatingIfNeeded: n) &* 0x9E37_79B9_7F4A_7C15
        x ^= x >> 29; x = x &* 0xBF58_476D_1CE4_E5B9; x ^= x >> 32
        return Int(truncatingIfNeeded: x & 0x7FFF_FFFF)
    }

    private static let words = ["build", "compile", "render", "scroll", "buffer", "history", "parse", "cursor", "session", "relay", "socket",
                                "token", "palette", "glyph", "kerning", "baseline", "terminal", "attributes", "inverse", "viewport", "prefetch"]
    private static func prose(_ n: Int, length: Int) -> String {
        var text = ""
        var k = n
        while text.count < length {
            k = mix(k)
            text += (text.isEmpty ? "" : " ") + words[k % words.count]
        }
        return String(text.prefix(length))
    }

    /// The raw text of line `serial`, SGR sequences included, no line break.
    static func line(_ serial: Int) -> String {
        let h = mix(serial)
        let esc = "\u{1B}["
        switch h % 12 {
        case 0, 1: return prose(serial, length: 28 + h % 20)
        case 2: return "\(esc)2m│\(esc)0m \(esc)1m>\(esc)0m \(prose(serial, length: 30))\(esc)2m│\(esc)0m"
        case 3: return "\(esc)1;35m⏺\(esc)0m \(esc)1mRead\(esc)0m(\(esc)36msrc/\(prose(serial, length: 12).replacingOccurrences(of: " ", with: "_")).rs\(esc)0m)"
        case 4: return "  \(esc)2m⎿\(esc)0m  Read \(h % 400) lines"
        case 5: return "\(esc)48;5;22m\(esc)38;5;120m+ \(prose(serial, length: 40))\(esc)0m"
        case 6: return "\(esc)48;5;52m\(esc)38;5;217m- \(prose(serial, length: 40))\(esc)0m"
        case 7: return "\(esc)1;32m   Compiling\(esc)0m \(words[h % words.count]) v0.\(h % 9).\(h % 31) (/Users/dev/\(words[(h >> 3) % words.count]))"
        case 8: return ""
        case 9: return "\(esc)38;2;215;119;87m✻\(esc)0m \(esc)3mThinking…\(esc)0m \(esc)2m(\(h % 90)s · ↑ \(h % 9).\(h % 10)k tokens)\(esc)0m"
        case 10: return "\(esc)32m✅\(esc)0m \(words[h % words.count]) \(esc)2m… ok\(esc)0m  \(esc)33m⚠\(esc)0m \(h % 5) warnings"
        default: return "\(esc)34m\(esc)1mtest\(esc)0m \(words[(h >> 5) % words.count])::\(words[(h >> 9) % words.count]) ... \(esc)32mok\(esc)0m"
        }
    }

    /// `count` lines starting at `first`, joined by line breaks (no break after the last).
    static func lines(_ first: Int, _ count: Int) -> [String] { (first..<(first + count)).map(line) }
}
