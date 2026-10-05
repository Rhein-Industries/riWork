@preconcurrency import AVFoundation
import Speech
import RiWorkCore

// Dictation on the phone, with Apple's on-device recognizers only: speech never leaves the phone and nothing is sent to a server to
// recognize it. iOS 26 and later use `SpeechAnalyzer` with `SpeechTranscriber` (fast, streaming, the newer model); iOS 18 to 25 use
// `SFSpeechRecognizer` with `requiresOnDeviceRecognition`. Both are handed the session's vocabulary as contextual strings, and what they
// hear is rewritten onto it (`SpeechVocabulary.rewrite`), which is what turns "key bar view" into `KeyBarView`.

/// What an engine reports, always on the main actor.
enum SpeechEngineEvent: Sendable, Equatable {
    case note(String)
    /// The microphone is open.
    case ready
    /// Everything heard so far, the last words still open to change.
    case heard(String)
    /// All of it, settled. Comes once, after `finish` or when the recognizer stops by itself.
    case finished(String)
    case failed(DictationProblem)
    /// How loud the microphone is, 0…1, a few times a second.
    case level(Float)
}

/// One dictation: the microphone and a recognizer, made for it and thrown away after.
@MainActor protocol SpeechEngine: AnyObject {
    var onEvent: ((SpeechEngineEvent) -> Void)? { get set }
    func start(vocabulary: SpeechVocabulary) async
    /// Stop listening; the recognizer settles the last words and reports `finished`.
    func finish()
    /// Stop at once; nothing more is reported.
    func cancel()
}

enum SpeechEngines {
    /// The engine for this phone.
    @MainActor static func make() -> any SpeechEngine {
        if #available(iOS 26, *), SpeechTranscriber.isAvailable { return AnalyzerSpeechEngine() }
        return RecognizerSpeechEngine()
    }
    /// The phone's language when there is an on-device recognizer for it, else US English.
    static let fallbackLocale = Locale(identifier: "en-US")

    /// Asks for the microphone, and for speech recognition when the engine needs it. Nil when both are granted.
    @MainActor static func authorize(recognition: Bool) async -> DictationProblem? {
        switch AVAudioApplication.shared.recordPermission {
        case .denied: return .microphoneDenied
        case .undetermined: if !(await AVAudioApplication.requestRecordPermission()) { return .microphoneDenied }
        default: break
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
    private var tapInstalled = false
    private var sessionActive = false
    /// The level of the latest buffer, read by the main actor a few times a second.
    private let levelLock = NSLock()
    private var latestLevel: Float = 0
    var level: Float { levelLock.lock(); defer { levelLock.unlock() }; return latestLevel }

    var inputFormat: AVAudioFormat { engine.inputNode.outputFormat(forBus: 0) }

    /// Sets up the audio session for recording and starts handing over buffers.
    func start(_ handle: @escaping @Sendable (AVAudioPCMBuffer) -> Void) throws {
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.record, mode: .measurement, options: [.duckOthers])
            try session.setActive(true, options: .notifyOthersOnDeactivation)
            sessionActive = true
            let input = engine.inputNode
            let format = input.outputFormat(forBus: 0)
            guard format.sampleRate > 0, format.channelCount > 0 else { throw MicrophoneError.noInput }
            input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, _ in
                self?.measure(buffer)
                handle(buffer)
            }
            tapInstalled = true
            engine.prepare()
            try engine.start()
            running = true
        } catch {
            stop()
            throw error
        }
    }
    func stop() {
        if tapInstalled { engine.inputNode.removeTap(onBus: 0); tapInstalled = false }
        if running { engine.stop(); running = false }
        if sessionActive {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            sessionActive = false
        }
    }
    private func measure(_ buffer: AVAudioPCMBuffer) {
        guard let samples = buffer.floatChannelData?[0], buffer.frameLength > 0 else { return }
        var sum: Float = 0
        for i in 0..<Int(buffer.frameLength) { sum += samples[i] * samples[i] }
        let rms = (sum / Float(buffer.frameLength)).squareRoot()
        // About -50 dB is silence and -10 dB loud speech.
        let decibels = 20 * log10(max(rms, 0.000_01))
        let level = min(1, max(0, (decibels + 50) / 40))
        levelLock.lock(); latestLevel = level; levelLock.unlock()
    }
    enum MicrophoneError: LocalizedError {
        case noInput
        var errorDescription: String? { "no microphone is available" }
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

/// Polls the microphone's level for the listening indicator.
@MainActor final class LevelMeter {
    private var task: Task<Void, Never>?
    func start(_ tap: MicrophoneTap, report: @escaping @MainActor (Float) -> Void) {
        task?.cancel()
        task = Task { @MainActor [weak tap] in
            while !Task.isCancelled, let tap {
                report(tap.level)
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
    }
    func stop() { task?.cancel(); task = nil }
}

// MARK: - iOS 26: SpeechAnalyzer

@available(iOS 26, *)
@MainActor final class AnalyzerSpeechEngine: SpeechEngine {
    var onEvent: ((SpeechEngineEvent) -> Void)?
    private let authorize: @MainActor (Bool) async -> DictationProblem?
    init(authorize: @escaping @MainActor (Bool) async -> DictationProblem? = { await SpeechEngines.authorize(recognition: $0) }) {
        self.authorize = authorize
    }
    private let microphone = MicrophoneTap()
    private let meter = LevelMeter()
    private var analyzer: SpeechAnalyzer?
    private var input: AsyncStream<AnalyzerInput>.Continuation?
    private var results: Task<Void, Never>?
    private var cancelled = false

    func start(vocabulary: SpeechVocabulary) async {
        guard !cancelled else { return }
        if let problem = await authorize(false) { report(.failed(problem)); return }
        guard !cancelled else { return }
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
            try microphone.start { buffer in
                guard let converted = converter.map({ $0.convert(buffer) }) ?? buffer else { return }
                continuation.yield(AnalyzerInput(buffer: converted))
            }
            meter.start(microphone) { [weak self] in self?.report(.level($0)) }
            report(.ready)
        } catch let problem as DictationProblem {
            fail(problem)
        } catch {
            fail(.failed(error.localizedDescription))
        }
    }
    func finish() {
        meter.stop()
        microphone.stop()
        input?.finish(); input = nil
        guard let analyzer else { return }
        Task { try? await analyzer.finalizeAndFinishThroughEndOfInput() }
    }
    func cancel() {
        cancelled = true
        onEvent = nil
        meter.stop()
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
    private func fail(_ problem: DictationProblem) {
        meter.stop()
        microphone.stop()
        input?.finish(); input = nil
        report(.failed(problem))
    }
    private func report(_ event: SpeechEngineEvent) { onEvent?(event) }

    /// The transcriber for the phone's language, with its model installed (fetched by the system the first time, then on the phone).
    static func transcriber(note: @escaping @MainActor (String) -> Void = { _ in }) async throws -> SpeechTranscriber {
        let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale.current) ?? SpeechEngines.fallbackLocale
        let transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [.volatileResults], attributeOptions: [])
        switch await AssetInventory.status(forModules: [transcriber]) {
        case .installed: break
        case .unsupported: throw DictationProblem.unsupported
        default:
            note("Installing the speech model…")
            do {
                if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) { try await request.downloadAndInstall() }
            } catch { throw DictationProblem.modelUnavailable }
        }
        return transcriber
    }
    static func context(_ vocabulary: SpeechVocabulary) -> AnalysisContext {
        let context = AnalysisContext()
        context.contextualStrings[.general] = vocabulary.terms
        return context
    }

    /// The whole of an audio file, for tests and the debug path: the same transcriber, context and rewrite as live dictation.
    static func transcribe(file url: URL, vocabulary: SpeechVocabulary, rewrite: Bool = true) async throws -> String {
        let transcriber = try await transcriber()
        let file = try AVAudioFile(forReading: url)
        let analyzer = try await SpeechAnalyzer(inputAudioFile: file, modules: [transcriber], analysisContext: context(vocabulary), finishAfterFile: true)
        _ = analyzer
        var text = ""
        for try await result in transcriber.results where result.isFinal { text += String(result.text.characters) }
        let heard = text.trimmingCharacters(in: .whitespaces)
        return rewrite ? vocabulary.rewrite(heard) : heard
    }
}

// MARK: - iOS 18 to 25: SFSpeechRecognizer on the device

@MainActor final class RecognizerSpeechEngine: SpeechEngine {
    var onEvent: ((SpeechEngineEvent) -> Void)?
    private let authorize: @MainActor (Bool) async -> DictationProblem?
    private let makeRecognizer: @MainActor () -> SFSpeechRecognizer?
    init(authorize: @escaping @MainActor (Bool) async -> DictationProblem? = { await SpeechEngines.authorize(recognition: $0) },
         makeRecognizer: @escaping @MainActor () -> SFSpeechRecognizer? = { RecognizerSpeechEngine.recognizer() }) {
        self.authorize = authorize
        self.makeRecognizer = makeRecognizer
    }
    private let microphone = MicrophoneTap()
    private let meter = LevelMeter()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var latest = ""
    private var done = false
    private var cancelled = false

    func start(vocabulary: SpeechVocabulary) async {
        guard !cancelled else { return }
        if let problem = await authorize(true) { report(.failed(problem)); return }
        guard !cancelled else { return }
        guard let recognizer = makeRecognizer() else { report(.failed(.unsupported)); return }
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
            try microphone.start { buffer in sink.append(buffer) }
            meter.start(microphone) { [weak self] in self?.report(.level($0)) }
            report(.ready)
        } catch {
            task?.cancel()
            report(.failed(.failed(error.localizedDescription)))
        }
    }
    func finish() {
        meter.stop()
        microphone.stop()
        request?.endAudio()
    }
    func cancel() {
        cancelled = true
        done = true
        onEvent = nil
        meter.stop()
        microphone.stop()
        task?.cancel()
    }
    private func received(text: String?, isFinal: Bool, failure: String?, vocabulary: SpeechVocabulary) {
        guard !done else { return }
        if let text { latest = vocabulary.rewrite(text) }
        if isFinal {
            done = true
            report(.finished(latest))
        } else if let failure {
            done = true
            meter.stop(); microphone.stop()
            // Ending with nothing said is reported as an error ("No speech detected"); that is just an empty dictation.
            if latest.isEmpty || request == nil { report(.finished(latest)) } else { report(.failed(.failed(failure))) }
        } else if text != nil {
            report(.heard(latest))
        }
    }
    private func report(_ event: SpeechEngineEvent) { onEvent?(event) }

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

    /// The whole of an audio file, for tests and the debug path.
    static func transcribe(file url: URL, vocabulary: SpeechVocabulary, rewrite: Bool = true) async throws -> String {
        guard let recognizer = recognizer() else { throw DictationProblem.unsupported }
        let request = Self.request(SFSpeechURLRecognitionRequest(url: url), vocabulary: vocabulary)
        request.shouldReportPartialResults = false
        let text: String = try await withCheckedThrowingContinuation { continuation in
            var resumed = false
            recognizer.recognitionTask(with: request) { result, error in
                guard !resumed else { return }
                if let result, result.isFinal { resumed = true; continuation.resume(returning: result.bestTranscription.formattedString) }
                else if let error { resumed = true; continuation.resume(throwing: error) }
            }
        }
        return rewrite ? vocabulary.rewrite(text) : text
    }
}

extension DictationProblem: Error {}
