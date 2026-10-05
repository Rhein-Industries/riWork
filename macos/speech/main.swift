import Foundation
import Speech

// riwork-speech: dictation for RiWork's chat on the Mac, in a process of its own beside the app (Contents/MacOS in the bundle).
//
// Apple's `SpeechAnalyzer` is a Swift-only API, so the Rust app runs this helper and talks to it over stdio, one JSON object per
// line. macOS attributes the microphone to the app that started the helper (RiWork), as it does for a program in a terminal.
//
//   riwork-speech listen            dictate from the microphone
//   riwork-speech scripted WORDS    "hear" WORDS a word at a time without a microphone (screenshots, tests)
//
// Both read the session's names and text first, one line: {"names": [...], "paths": [...], "text": "..."}. They then write
//   {"event":"note","text":"Downloading the speech model (once)…"}   getting ready takes a while
//   {"event":"ready"}                                                   the microphone is open
//   {"event":"heard","text":"..."}                                      everything heard so far, rewritten onto the vocabulary
//   {"event":"finished","text":"..."}                                   all of it; the helper exits after it
//   {"event":"failed","problem":"microphone-denied","detail":"..."}     the helper exits after it
// and take `finish` (settle the last words, then report `finished`) or `cancel` on stdin. Stdin closing is `cancel`: the helper
// never outlives the app.
//
//   riwork-speech transcribe FILE [SOURCES.json]   an audio file through the same transcriber, context and rewrite
//   riwork-speech rewrite                          stdin {"names", "paths", "text", "heard"}: the vocabulary and the rewrite

/// What the app says about the session: the same as the phone's `SpeechVocabularySources`.
struct Sources: Decodable {
    var names: [String] = []
    var paths: [String] = []
    var text: String = ""
    var heard: String?

    var vocabulary: SpeechVocabulary {
        SpeechVocabulary.build(SpeechVocabularySources(names: names, paths: paths, text: text))
    }
    static func read(_ data: Data?) -> Sources {
        guard let data, !data.isEmpty else { return Sources() }
        do { return try JSONDecoder().decode(Sources.self, from: data) } catch {
            FileHandle.standardError.write(Data("riwork-speech: unreadable sources: \(error)\n".utf8))
            return Sources()
        }
    }
}

extension Sources {
    enum CodingKeys: String, CodingKey { case names, paths, text, heard }
    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        names = try container.decodeIfPresent([String].self, forKey: .names) ?? []
        paths = try container.decodeIfPresent([String].self, forKey: .paths) ?? []
        text = try container.decodeIfPresent(String.self, forKey: .text) ?? ""
        heard = try container.decodeIfPresent(String.self, forKey: .heard)
    }
}

/// One line of output, written at once.
func emit(_ object: [String: Any]) {
    guard var data = try? JSONSerialization.data(withJSONObject: object, options: [.withoutEscapingSlashes]) else { return }
    data.append(0x0A)
    FileHandle.standardOutput.write(data)
}

func emit(_ event: EngineEvent) {
    switch event {
    case .note(let text): emit(["event": "note", "text": text])
    case .ready: emit(["event": "ready"])
    case .heard(let text): emit(["event": "heard", "text": text])
    case .finished(let text): emit(["event": "finished", "text": text])
    case .failed(let problem): emit(["event": "failed", "problem": problem.code, "detail": problem.detail])
    }
}

/// A dictation driven by the app over stdin.
@MainActor final class Session {
    let engine: any SpeechEngine
    init(engine: any SpeechEngine) { self.engine = engine }

    func run() {
        // The first line is the session's sources; everything after it is a command.
        let sources = Sources.read(readLine(strippingNewline: true).map { Data($0.utf8) })
        engine.onEvent = { event in
            emit(event)
            switch event {
            case .finished, .failed: exit(0)
            default: break
            }
        }
        Thread.detachNewThread {
            while let line = readLine(strippingNewline: true) {
                let command = line.trimmingCharacters(in: .whitespaces)
                DispatchQueue.main.async { MainActor.assumeIsolated { self.command(command) } }
            }
            DispatchQueue.main.async { MainActor.assumeIsolated { self.command("cancel") } }
        }
        // Belt and braces: an app that died without closing the pipe leaves this process to launchd.
        Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { _ in if getppid() == 1 { exit(0) } }
        let vocabulary = sources.vocabulary
        Task { await self.engine.start(vocabulary: vocabulary) }
    }
    func command(_ command: String) {
        switch command {
        case "finish": engine.finish()
        case "cancel": engine.cancel(); exit(0)
        default: FileHandle.standardError.write(Data("riwork-speech: unknown command \(command)\n".utf8))
        }
    }
}

func usage() -> Never {
    FileHandle.standardError.write(Data("usage: riwork-speech listen | scripted WORDS | transcribe FILE [SOURCES.json] | rewrite\n".utf8))
    exit(64)
}

let arguments = Array(CommandLine.arguments.dropFirst())
switch arguments.first {
case "listen":
    MainActor.assumeIsolated { Session(engine: SpeechEngines.make()).run() }
case "scripted":
    let script = arguments.dropFirst().joined(separator: " ")
    MainActor.assumeIsolated { Session(engine: ScriptedSpeechEngine(script: script)).run() }
case "rewrite":
    let sources = Sources.read(FileHandle.standardInput.readDataToEndOfFile())
    let vocabulary = sources.vocabulary
    emit(["terms": vocabulary.terms, "text": vocabulary.rewrite(sources.heard ?? "")])
    exit(0)
case "transcribe":
    guard arguments.count >= 2 else { usage() }
    let file = URL(fileURLWithPath: arguments[1])
    let sources = Sources.read(arguments.count >= 3 ? FileManager.default.contents(atPath: arguments[2]) : nil)
    Task { @MainActor in
        guard #available(macOS 26, *), SpeechTranscriber.isAvailable else {
            emit(["error": "SpeechTranscriber is not available on this Mac"]); exit(1)
        }
        do {
            let vocabulary = sources.vocabulary
            let heard = try await AnalyzerSpeechEngine.transcribe(file: file, vocabulary: vocabulary)
            emit(["heard": heard, "text": vocabulary.rewrite(heard)])
            exit(0)
        } catch {
            emit(["error": (error as? Problem)?.code ?? error.localizedDescription]); exit(1)
        }
    }
default:
    usage()
}
dispatchMain()
