import Foundation
import RiWorkCore

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

/// Long tool-call transcript shared by the pure projection and hosted Simulator regressions.
enum ToolUIPerformanceFixture {
    static let itemCount = 2200
    static let imageBase64 = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAlklEQVR4nO3PsQ2AMBTEUMbJiIyTERmBBfgFlWPJ0pUu7l3r3p979vrcaf112qEA9KEA9KEA9KHfAMvRqQ9A9wHoPgDd+wGWo1MfgO4D0H0AuvcDLEenPgDdB6D7AHTvB1iOTn0Aug9A9wHo3g+wHJ36AHQfgO4D0L0fYDk69QHoPgDdB6B7P8BydOoD0H0Aug9A93rAC0TkwVrNiEm+AAAAAElFTkSuQmCC"
    static func items() -> [ChatItem] {
        let output = String(String(repeating: "result: 0123456789 abcdefghijklmnopqrstuvwxyz\n", count: 1600).prefix(65_536))
        return (0..<itemCount).map { index in
            let turn = index < 1200 ? "perf-dense" : "perf-turn-\((index - 1200) / 22)"
            let body: ChatItemBody
            switch index < 1200 ? -1 : (index - 1200) % 22 {
            case -1: body = .toolCall(server: nil, tool: "Read", input: .object(["file_path": .string("src/file-\(index).rs")]), output: output)
            case 0: body = .userMessage("Inspect source files and show the result for turn \(index / 22).")
            case 21: body = .agentMessage("Completed inspection of turn \(index / 22).")
            default: body = .command(command: "cat src/file-\(index).rs", cwd: "/fixture", output: output, exitCode: 0)
            }
            return ChatItem(id: "perf-item-\(index)", turnID: turn, status: .completed, body: body,
                            presentation: ChatPresentation(images: index % 22 == 20 ? [.init(label: "fixture.png", source: .data(mime: "image/png", base64: imageBase64))] : [],
                                                           semantics: index < 1200 ? .init(kind: .read, target: "src/file-\(index).rs") : nil),
                            startedAt: UInt64(index * 100), endedAt: UInt64(index * 100 + 50))
        }
    }
    static func transcript() -> ChatTranscript {
        var transcript = ChatTranscript()
        for item in items() { transcript.apply(.itemCompleted(item)) }
        transcript.apply(.turnStarted(turnID: "perf-live", startedAt: 1_000))
        transcript.apply(.itemStarted(.init(id: "perf-live", turnID: "perf-live", body: .agentMessage("Streaming"))))
        return transcript
    }
}

/// A chat of long Markdown answers, as an agent writes them: a heading, prose with emphasis, code spans and links, a list, a table, a
/// code block and a quote. Line `n` differs from every other answer's, so nothing is shared between messages but the shape.
enum MarkdownPerformanceFixture {
    static let turns = 150
    static func message(_ n: Int) -> String {
        """
        ## Step \(n): tighten the **render** path

        The `ChatMarkdown` parser for turn \(n) reads *blocks* first, then [inline text](https://example.com/\(n)) with `code spans`, **bold** and ~~struck~~ words, so a long answer stays readable on a phone.

        - Parse `src/render_\(n).rs` once per **change**
        - Keep the [docs](https://example.com/docs/\(n)) next to the code
        - [x] Check the `List` identity for row \(n)
        - Measure *before* and *after* on a synthetic chat

        | File | Lines | Status |
        | --- | ---: | :---: |
        | `src/a_\(n).rs` | \(n * 3) | **ok** |
        | `src/b_\(n).rs` | \(n * 7) | *slow* |

        ```swift
        let value\(n) = ChatMarkdown.parse(source)
        print(value\(n).count)
        ```

        > Note \(n): the quote keeps its own **muted** text and a `span`.

        Finally, turn \(n) ends with a paragraph that mentions [a link](https://example.com/end/\(n)) and `more code`.
        """
    }
    /// A question and its answer per turn.
    static func items() -> [ChatItem] {
        (0..<turns).flatMap { n in
            [ChatItem(id: "md-user-\(n)", turnID: "md-turn-\(n)", status: .completed, body: .userMessage("Explain step \(n) of the render path.")),
             ChatItem(id: "md-agent-\(n)", turnID: "md-turn-\(n)", status: .completed, body: .agentMessage(message(n)))]
        }
    }
    /// The `k`th delta of an answer that streams in: words that grow a paragraph, new paragraphs, headings and list items.
    static func chunk(_ k: Int) -> String {
        switch k % 6 {
        case 0: "\n\n### Part \(k)\n\n"
        case 1: "The **delta** \(k) adds `code_\(k)` and [a link](https://example.com/\(k)) to the paragraph. "
        case 2: "More *streamed* words for part \(k), still in the same paragraph. "
        case 3: "\n\n- item \(k) with a `span`\n"
        case 4: "- another **item** \(k)\n"
        default: "\nClosing sentence \(k) of the block.\n"
        }
    }
}
