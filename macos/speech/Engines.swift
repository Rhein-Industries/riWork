@preconcurrency import AVFoundation
import Speech

// Dictation on the Mac, with Apple's on-device recognizers only: speech never leaves the Mac and nothing is sent to a server to
// recognize it. macOS 26 and later use `SpeechAnalyzer` with `SpeechTranscriber`; older systems use `SFSpeechRecognizer` with
// `requiresOnDeviceRecognition`. These are the phone's engines (ios/RiWorkRemote/SpeechEngine.swift) without the iPhone's audio
// session: both are handed the session's vocabulary as contextual strings, and what they hear is rewritten onto it
// (`SpeechVocabulary.rewrite`, shared with the phone from ios/Core).

/// Why dictation could not go on, as the app is told it.
enum Problem: Error, Equatable {
    case microphoneDenied
    case recognitionDenied
    /// No on-device recognizer for the language.
    case unsupported
    /// The speech model is not on the Mac yet and could not be fetched.
    case modelUnavailable
    /// The microphone could not be opened: another app holds it, or there is none.
    case microphoneBusy(String)
    /// The microphone went away while listening (unplugged, or the input device changed).
    case interrupted
    case failed(String)

    var code: String {
        switch self {
        case .microphoneDenied: "microphone-denied"
        case .recognitionDenied: "recognition-denied"
        case .unsupported: "unsupported"
        case .modelUnavailable: "model-unavailable"
        case .microphoneBusy: "microphone-busy"
        case .interrupted: "interrupted"
        case .failed: "failed"
        }
    }
    var detail: String {
        switch self {
        case .microphoneBusy(let detail), .failed(let detail): detail
        default: ""
        }
    }
}

/// What an engine reports, always on the main actor.
enum EngineEvent: Equatable {
    case note(String)
    /// The microphone is open.
    case ready
    /// Everything heard so far, the last words still open to change.
    case heard(String)
    /// All of it, settled. Comes once, after `finish` or when the recognizer stops by itself.
    case finished(String)
    case failed(Problem)
}

/// One dictation: the microphone and a recognizer, made for it and thrown away after.
@MainActor protocol SpeechEngine: AnyObject {
    var onEvent: ((EngineEvent) -> Void)? { get set }
    func start(vocabulary: SpeechVocabulary) async
    /// Stop listening; the recognizer settles the last words and reports `finished`.
    func finish()
    /// Stop at once; nothing more is reported.
    func cancel()
}

enum SpeechEngines {
    /// The engine for this Mac.
    @MainActor static func make() -> any SpeechEngine {
        if #available(macOS 26, *), SpeechTranscriber.isAvailable { return AnalyzerSpeechEngine() }
        return RecognizerSpeechEngine()
    }
    /// The Mac's language when there is an on-device recognizer for it, else US English.
    static let fallbackLocale = Locale(identifier: "en-US")

    /// Asks for the microphone, and for speech recognition when the engine needs it. Nil when both are granted. macOS asks on
    /// behalf of the app that started this helper, RiWork, and remembers the answer for it.
    @MainActor static func authorize(recognition: Bool) async -> Problem? {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: break
        case .notDetermined: if !(await AVCaptureDevice.requestAccess(for: .audio)) { return .microphoneDenied }
        default: return .microphoneDenied
        }
        guard recognition else { return nil }
        switch SFSpeechRecognizer.authorizationStatus() {
        case .authorized: return nil
        case .denied, .restricted: return .recognitionDenied
        default:
            let status = await withCheckedContinuation { continuation in SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0) } }
            return status == .authorized ? nil : .recognitionDenied
        }
    }
}

// MARK: - The microphone

/// The microphone, through `AVAudioEngine`. Buffers arrive on the audio thread.
final class MicrophoneTap: @unchecked Sendable {
    private let engine = AVAudioEngine()
    private var running = false
    private var observer: (any NSObjectProtocol)?

    var inputFormat: AVAudioFormat { engine.inputNode.outputFormat(forBus: 0) }

    /// Starts handing over buffers. `lost` is called on the main queue when the input device goes away while running.
    func start(lost: @escaping @Sendable () -> Void, _ handle: @escaping @Sendable (AVAudioPCMBuffer) -> Void) throws {
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw Problem.microphoneBusy("no microphone is available") }
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in handle(buffer) }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            // Another app holding the input exclusively, or a device that refuses to start.
            throw Problem.microphoneBusy(error.localizedDescription)
        }
        running = true
        observer = NotificationCenter.default.addObserver(forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main) { _ in lost() }
    }
    func stop() {
        guard running else { return }
        running = false
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = nil
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
    }
}

/// Converts the microphone's buffers to the format a recognizer wants. Used from the audio thread only.
final class BufferConverter: @unchecked Sendable {
    private let converter: AVAudioConverter?
    private let output: AVAudioFormat
    init(from input: AVAudioFormat, to output: AVAudioFormat) {
        self.output = output
        converter = input == output ? nil : AVAudioConverter(from: input, to: output)
    }
    func convert(_ buffer: AVAudioPCMBuffer) -> AVAudioPCMBuffer? {
        guard let converter else { return buffer }
        let ratio = output.sampleRate / buffer.format.sampleRate
        let capacity = AVAudioFrameCount((Double(buffer.frameLength) * ratio).rounded(.up)) + 16
        guard let converted = AVAudioPCMBuffer(pcmFormat: output, frameCapacity: capacity) else { return nil }
        var consumed = false
        var error: NSError?
        converter.convert(to: converted, error: &error) { _, status in
            if consumed { status.pointee = .noDataNow; return nil }
            consumed = true
            status.pointee = .haveData
            return buffer
        }
        return error == nil ? converted : nil
    }
}

// MARK: - macOS 26: SpeechAnalyzer

@available(macOS 26, *)
@MainActor final class AnalyzerSpeechEngine: SpeechEngine {
    var onEvent: ((EngineEvent) -> Void)?
    private let microphone = MicrophoneTap()
    private var analyzer: SpeechAnalyzer?
    private var input: AsyncStream<AnalyzerInput>.Continuation?
    private var results: Task<Void, Never>?
    private var cancelled = false

    func start(vocabulary: SpeechVocabulary) async {
        if let problem = await SpeechEngines.authorize(recognition: false) { report(.failed(problem)); return }
        do {
            let transcriber = try await Self.transcriber(note: { [weak self] in self?.report(.note($0)) })
            guard !cancelled else { return }
            let analyzer = SpeechAnalyzer(modules: [transcriber], options: .init(priority: .userInitiated, modelRetention: .lingering))
            self.analyzer = analyzer
            try await analyzer.setContext(Self.context(vocabulary))
            let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber], considering: microphone.inputFormat)
            try await analyzer.prepareToAnalyze(in: format)
            guard !cancelled else { await analyzer.cancelAndFinishNow(); return }
            results = Task { [weak self] in await self?.read(transcriber, vocabulary: vocabulary) }
            let (stream, continuation) = AsyncStream.makeStream(of: AnalyzerInput.self)
            input = continuation
            try await analyzer.start(inputSequence: stream)
            let converter = format.map { BufferConverter(from: microphone.inputFormat, to: $0) }
            try microphone.start(lost: { [weak self] in Task { @MainActor in self?.fail(.interrupted) } }) { buffer in
                guard let converted = converter.map({ $0.convert(buffer) }) ?? buffer else { return }
                continuation.yield(AnalyzerInput(buffer: converted))
            }
            report(.ready)
        } catch let problem as Problem {
            fail(problem)
        } catch {
            fail(.failed(error.localizedDescription))
        }
    }
    func finish() {
        microphone.stop()
        input?.finish(); input = nil
        guard let analyzer else { return }
        Task { try? await analyzer.finalizeAndFinishThroughEndOfInput() }
    }
    func cancel() {
        cancelled = true
        onEvent = nil
        microphone.stop()
        input?.finish(); input = nil
        results?.cancel()
        if let analyzer { Task { await analyzer.cancelAndFinishNow() } }
    }

    private func read(_ transcriber: SpeechTranscriber, vocabulary: SpeechVocabulary) async {
        var settled = "", open = ""
        do {
            for try await result in transcriber.results {
                let text = String(result.text.characters)
                if result.isFinal { settled += text; open = "" } else { open = text }
                report(.heard(vocabulary.rewrite(settled + open)))
            }
            report(.finished(vocabulary.rewrite(settled + open)))
        } catch {
            if !Task.isCancelled { fail(.failed(error.localizedDescription)) }
        }
    }
    private func fail(_ problem: Problem) {
        microphone.stop()
        input?.finish(); input = nil
        report(.failed(problem))
    }
    private func report(_ event: EngineEvent) { onEvent?(event) }

    /// The transcriber for the Mac's language, with its model installed (fetched by the system the first time, then on the Mac).
    static func transcriber(note: @escaping @MainActor (String) -> Void = { _ in }) async throws -> SpeechTranscriber {
        let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale.current) ?? SpeechEngines.fallbackLocale
        let transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [.volatileResults], attributeOptions: [])
        switch await AssetInventory.status(forModules: [transcriber]) {
        case .installed: break
        case .unsupported: throw Problem.unsupported
        default:
            note("Downloading the speech model (once)…")
            do {
                if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) { try await request.downloadAndInstall() }
            } catch { throw Problem.modelUnavailable }
        }
        return transcriber
    }
    static func context(_ vocabulary: SpeechVocabulary) -> AnalysisContext {
        let context = AnalysisContext()
        context.contextualStrings[.general] = vocabulary.terms
        return context
    }

    /// The whole of an audio file: the same transcriber and context as live dictation. Returns what was heard, before the rewrite.
    static func transcribe(file url: URL, vocabulary: SpeechVocabulary) async throws -> String {
        let transcriber = try await transcriber()
        let file = try AVAudioFile(forReading: url)
        let analyzer = try await SpeechAnalyzer(inputAudioFile: file, modules: [transcriber], analysisContext: context(vocabulary), finishAfterFile: true)
        _ = analyzer
        var text = ""
        for try await result in transcriber.results where result.isFinal { text += String(result.text.characters) }
        return text.trimmingCharacters(in: .whitespaces)
    }
}

// MARK: - Before macOS 26: SFSpeechRecognizer on the device

@MainActor final class RecognizerSpeechEngine: SpeechEngine {
    var onEvent: ((EngineEvent) -> Void)?
    private let microphone = MicrophoneTap()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var latest = ""
    private var done = false

    func start(vocabulary: SpeechVocabulary) async {
        if let problem = await SpeechEngines.authorize(recognition: true) { report(.failed(problem)); return }
        guard let recognizer = Self.recognizer() else { report(.failed(.unsupported)); return }
        let request = Self.request(SFSpeechAudioBufferRecognitionRequest(), vocabulary: vocabulary)
        self.request = request
        task = recognizer.recognitionTask(with: request) { [weak self] result, error in
            let text = result?.bestTranscription.formattedString
            let isFinal = result?.isFinal ?? false
            let failure = error.map { ($0 as NSError).localizedDescription }
            Task { @MainActor in self?.received(text: text, isFinal: isFinal, failure: failure, vocabulary: vocabulary) }
        }
        do {
            // The request takes audio from any thread.
            nonisolated(unsafe) let sink = request
            try microphone.start(lost: { [weak self] in Task { @MainActor in self?.lost() } }) { buffer in sink.append(buffer) }
            report(.ready)
        } catch {
            task?.cancel()
            report(.failed(error as? Problem ?? .failed(error.localizedDescription)))
        }
    }
    func finish() {
        microphone.stop()
        request?.endAudio()
    }
    func cancel() {
        onEvent = nil
        microphone.stop()
        task?.cancel()
    }
    private func lost() {
        guard !done else { return }
        done = true
        microphone.stop()
        task?.cancel()
        report(.failed(.interrupted))
    }
    private func received(text: String?, isFinal: Bool, failure: String?, vocabulary: SpeechVocabulary) {
        guard !done else { return }
        if let text { latest = vocabulary.rewrite(text) }
        if isFinal {
            done = true
            report(.finished(latest))
        } else if let failure {
            done = true
            microphone.stop()
            // Ending with nothing said is reported as an error ("No speech detected"); that is just an empty dictation.
            if latest.isEmpty || request == nil { report(.finished(latest)) } else { report(.failed(.failed(failure))) }
        } else if text != nil {
            report(.heard(latest))
        }
    }
    private func report(_ event: EngineEvent) { onEvent?(event) }

    static func recognizer() -> SFSpeechRecognizer? {
        let recognizer = SFSpeechRecognizer(locale: Locale.current).flatMap { $0.supportsOnDeviceRecognition ? $0 : nil }
            ?? SFSpeechRecognizer(locale: SpeechEngines.fallbackLocale)
        guard let recognizer, recognizer.supportsOnDeviceRecognition else { return nil }
        return recognizer
    }
    static func request<R: SFSpeechRecognitionRequest>(_ request: R, vocabulary: SpeechVocabulary) -> R {
        request.requiresOnDeviceRecognition = true
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        request.taskHint = .dictation
        request.contextualStrings = vocabulary.terms
        return request
    }
}

// MARK: - Scripted

/// An engine that "hears" a fixed text a word at a time and never touches the microphone: for screenshots and tests of the app's
/// side, through the same process, vocabulary and rewrite as the real one.
@MainActor final class ScriptedSpeechEngine: SpeechEngine {
    var onEvent: ((EngineEvent) -> Void)?
    let words: [String]
    let interval: Duration
    private var vocabulary: SpeechVocabulary?
    private var heard = 0
    private var task: Task<Void, Never>?

    init(script: String, interval: Duration = .milliseconds(350)) {
        words = script.split(separator: " ").map(String.init)
        self.interval = interval
    }
    func start(vocabulary: SpeechVocabulary) async {
        self.vocabulary = vocabulary
        onEvent?(.ready)
        task = Task { [weak self] in
            guard let self else { return }
            while heard < words.count, !Task.isCancelled {
                try? await Task.sleep(for: interval)
                guard !Task.isCancelled else { return }
                heard += 1
                onEvent?(.heard(text))
            }
        }
    }
    var text: String {
        let said = words.prefix(heard).joined(separator: " ")
        return vocabulary?.rewrite(said) ?? said
    }
    func finish() {
        task?.cancel()
        let text = text
        Task { @MainActor [weak self] in self?.onEvent?(.finished(text)) }
    }
    func cancel() { task?.cancel(); onEvent = nil }
}
