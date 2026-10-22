@preconcurrency import AVFoundation
import Speech
import OSLog
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
    /// All of it, settled. Comes once, after `finish`.
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
            let status = await withCheckedContinuation { continuation in SFSpeechRecognizer.requestAuthorization { @Sendable status in continuation.resume(returning: status) } }
            return status == .authorized ? nil : .recognitionDenied
        }
    }
}

/// Visible in Console/device logs under com.riwork.dictation / latency; no dictated content is logged.
@MainActor private final class FirstWordLatency {
    private let logger = Logger(subsystem: "com.riwork.dictation", category: "latency")
    private var reported = false
    func partial(_ text: String, capture: MicrophoneTap) {
        guard !reported, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, let started = capture.captureStart else { return }
        reported = true
        let now = ProcessInfo.processInfo.systemUptime
        let milliseconds = (now - started) * 1_000
        logger.notice("first_volatile_ms=\(milliseconds, privacy: .public) reference=capture_start")
        if let speech = capture.firstNonSilent {
            let speechMilliseconds = (now - speech) * 1_000
            logger.notice("first_volatile_ms=\(speechMilliseconds, privacy: .public) reference=first_non_silent_buffer")
        } else {
            logger.notice("first_volatile_ms unavailable reference=first_non_silent_buffer")
        }
    }
}

// MARK: - The microphone

/// Audio-thread measurements, independent of actor isolation. Silence counts as a live buffer.
private final class CaptureActivity: @unchecked Sendable {
    let watchdog = AudioBufferWatchdog()
    private let lock = NSLock()
    private var latestLevel: Float = 0
    var level: Float { lock.lock(); defer { lock.unlock() }; return latestLevel }
    func received(_ buffer: AVAudioPCMBuffer) {
        var rms: Float = 0
        if let samples = buffer.floatChannelData?[0], buffer.frameLength > 0 {
            var sum: Float = 0
            for i in 0..<Int(buffer.frameLength) { sum += samples[i] * samples[i] }
            rms = (sum / Float(buffer.frameLength)).squareRoot()
        }
        let decibels = 20 * log10(max(rms, 0.000_01))
        watchdog.received(nonSilent: decibels > -45)
        lock.lock(); latestLevel = min(1, max(0, (decibels + 50) / 40)); lock.unlock()
    }
}

/// Device recovery replaces AVAudioEngine so its input node binds to the current input device.
@MainActor final class MicrophoneTap {
    private var engine = AVAudioEngine()
    private var running = false
    private var tapInstalled = false
    private var configuration: (any NSObjectProtocol)?
    private var route: (any NSObjectProtocol)?
    private var watch: Task<Void, Never>?
    private var recovery: Task<Void, Never>?
    private let activity = CaptureActivity()
    private(set) var captureStart: TimeInterval?
    private var sessionActive = false
    var inputFormat: AVAudioFormat { engine.inputNode.outputFormat(forBus: 0) }
    var firstNonSilent: TimeInterval? { activity.watchdog.firstNonSilent }
    var level: Float { activity.level }
    func start(lost: @escaping @Sendable () -> Void, _ handle: @escaping @Sendable (AVAudioPCMBuffer) -> Void) throws {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.record, mode: .measurement, options: [.duckOthers])
        try session.setActive(true, options: .notifyOthersOnDeactivation)
        sessionActive = true
        running = true
        do { try install(handle, lost: lost) } catch { stop(); throw error }
        route = NotificationCenter.default.addObserver(forName: AVAudioSession.routeChangeNotification, object: nil, queue: .main) { @Sendable [weak self] note in
            guard let raw = note.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
                  let reason = AVAudioSession.RouteChangeReason(rawValue: raw),
                  [.newDeviceAvailable, .oldDeviceUnavailable, .routeConfigurationChange].contains(reason) else { return }
            Task { @MainActor in self?.recover(handle, lost: lost) }
        }
        watch = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(250))
                guard let self, !Task.isCancelled, running else { return }
                switch activity.watchdog.poll() {
                case .none: break
                case .recover: recover(handle, lost: lost)
                case .lost: stop(); lost(); return
                }
            }
        }
    }
    // A nonisolated factory plus an explicit @Sendable block prevents Swift 6 actor assertions on the audio thread.
    nonisolated private static func tap(activity: CaptureActivity, handle: @escaping @Sendable (AVAudioPCMBuffer) -> Void) -> AVAudioNodeTapBlock {
        { @Sendable buffer, _ in
            activity.received(buffer)
            handle(buffer)
        }
    }
    private func install(_ handle: @escaping @Sendable (AVAudioPCMBuffer) -> Void, lost: @escaping @Sendable () -> Void) throws {
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw DictationProblem.microphoneBusy("no microphone is available") }
        input.installTap(onBus: 0, bufferSize: 1024, format: format, block: Self.tap(activity: activity, handle: handle))
        tapInstalled = true
        engine.prepare()
        let start = ProcessInfo.processInfo.systemUptime
        do { try engine.start() } catch { throw DictationProblem.microphoneBusy(error.localizedDescription) }
        if captureStart == nil { captureStart = start; activity.watchdog.start(at: start) }
        configuration = NotificationCenter.default.addObserver(forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main) { @Sendable [weak self] _ in
            Task { @MainActor in self?.recover(handle, lost: lost) }
        }
    }
    private func recover(_ handle: @escaping @Sendable (AVAudioPCMBuffer) -> Void, lost: @escaping @Sendable () -> Void) {
        guard running, recovery == nil else { return }
        activity.watchdog.beginRecovery()
        recovery = Task { [weak self] in
            // Coalesce device/engine notifications and allow the new route to settle.
            try? await Task.sleep(for: .milliseconds(150))
            guard let self, !Task.isCancelled, running else { return }
            defer { recovery = nil }
            releaseEngine()
            activity.watchdog.beginRecovery()
            engine = AVAudioEngine()
            do { try install(handle, lost: lost) } catch {
                releaseEngine()
                // Keep the watchdog alive: only fail after the recovery's three-second buffer grace expires.
            }
        }
    }
    private func releaseEngine() {
        if let configuration { NotificationCenter.default.removeObserver(configuration) }
        configuration = nil
        if tapInstalled { engine.inputNode.removeTap(onBus: 0); tapInstalled = false }
        engine.stop()
    }
    func stop() {
        running = false
        watch?.cancel(); watch = nil
        recovery?.cancel(); recovery = nil
        if let route { NotificationCenter.default.removeObserver(route) }
        route = nil
        releaseEngine()
        if sessionActive {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            sessionActive = false
        }
    }
}

/// Converts the microphone's buffers to the format a recognizer wants. Used from the audio thread only.
final class BufferConverter: @unchecked Sendable {
    private var converter: AVAudioConverter?
    private var sourceFormat: AVAudioFormat
    private let output: AVAudioFormat
    init(from input: AVAudioFormat, to output: AVAudioFormat) throws {
        self.output = output
        sourceFormat = input
        converter = input == output ? nil : AVAudioConverter(from: input, to: output)
        if input != output, converter == nil { throw DictationProblem.failed("The microphone audio format cannot be converted.") }
    }
    func convert(_ buffer: AVAudioPCMBuffer) throws -> AVAudioPCMBuffer? {
        if buffer.format != sourceFormat {
            sourceFormat = buffer.format
            converter = sourceFormat == output ? nil : AVAudioConverter(from: sourceFormat, to: output)
        }
        if sourceFormat != output, converter == nil { throw DictationProblem.failed("The microphone audio format cannot be converted.") }
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

/// A bounded handoff between continuous capture and replaceable analyzers. During a restart it owns copies of arriving buffers.
@available(iOS 26, *)
private final class AnalyzerAudioSink: @unchecked Sendable {
    private let lock = NSLock()
    private var input: AsyncStream<AnalyzerInput>.Continuation?
    private var converter: BufferConverter?
    private var pending: [AVAudioPCMBuffer] = []
    private var accepting = true
    private var cancelled = false
    private let failed: @Sendable (DictationProblem) -> Void
    init(failed: @escaping @Sendable (DictationProblem) -> Void) { self.failed = failed }
    func append(_ buffer: AVAudioPCMBuffer) {
        var problem: DictationProblem?
        lock.lock()
        if accepting, !cancelled {
            do {
                let delivered: Bool
                if let input, let converter { delivered = try send(buffer, to: input, converter: converter) }
                else { delivered = false }
                if !delivered {
                    self.input = nil
                    guard pending.count < 256, let copy = AVAudioPCMBuffer(pcmFormat: buffer.format, frameCapacity: buffer.frameLength) else {
                        throw DictationProblem.failed("Speech processing could not keep up with the microphone.")
                    }
                    copy.frameLength = buffer.frameLength
                    let source = UnsafeMutableAudioBufferListPointer(buffer.mutableAudioBufferList)
                    let destination = UnsafeMutableAudioBufferListPointer(copy.mutableAudioBufferList)
                    for (from, to) in zip(source, destination) {
                        if let fromData = from.mData, let toData = to.mData { memcpy(toData, fromData, min(Int(from.mDataByteSize), Int(to.mDataByteSize))) }
                    }
                    pending.append(copy)
                }
            } catch { problem = error as? DictationProblem ?? .failed(error.localizedDescription) }
        }
        lock.unlock()
        if let problem { failed(problem) }
    }
    private func send(_ buffer: AVAudioPCMBuffer, to input: AsyncStream<AnalyzerInput>.Continuation, converter: BufferConverter) throws -> Bool {
        guard let converted = try converter.convert(buffer) else { throw DictationProblem.failed("The microphone audio could not be converted.") }
        switch input.yield(AnalyzerInput(buffer: converted)) {
        case .enqueued: return true
        case .terminated: return false
        case .dropped: throw DictationProblem.failed("Speech processing could not keep up with the microphone.")
        @unknown default: return false
        }
    }
    func resume(_ input: AsyncStream<AnalyzerInput>.Continuation, converter: BufferConverter) throws {
        lock.lock(); defer { lock.unlock() }
        guard !cancelled else { input.finish(); return }
        self.input = input
        self.converter = converter
        for buffer in pending {
            guard try send(buffer, to: input, converter: converter) else { throw DictationProblem.failed("The speech analyzer ended during recovery.") }
        }
        pending.removeAll(keepingCapacity: true)
        if !accepting { input.finish() }
    }
    func pause() {
        lock.lock(); defer { lock.unlock() }
        input?.finish(); input = nil
    }
    @discardableResult func finish() -> Bool {
        lock.lock(); defer { lock.unlock() }
        accepting = false
        input?.finish()
        return input != nil
    }
    func cancel() {
        lock.lock(); defer { lock.unlock() }
        cancelled = true; accepting = false
        input?.finish(); input = nil
        pending.removeAll()
    }
}

@available(iOS 26, *)
@MainActor final class AnalyzerSpeechEngine: SpeechEngine {
    var onEvent: ((SpeechEngineEvent) -> Void)?
    private let authorize: @MainActor (Bool) async -> DictationProblem?
    init(authorize: @escaping @MainActor (Bool) async -> DictationProblem? = { await SpeechEngines.authorize(recognition: $0) }) {
        self.authorize = authorize
    }

    private let microphone = MicrophoneTap()
    private let meter = LevelMeter()
    private lazy var audio = AnalyzerAudioSink { @Sendable [weak self] problem in
        Task { @MainActor in self?.fail(problem) }
    }
    private var analyzer: SpeechAnalyzer?
    private var results: Task<Void, Never>?
    private var stopping: Task<Void, Never>?
    private var cancelled = false
    private var finishing = false
    private var ended = false
    private var capturing = false
    private var generation = 0
    private var latest = ""
    private var vocabulary = SpeechVocabulary(terms: [])
    private var text = DictationTextCache(vocabulary: SpeechVocabulary(terms: []))
    private var rapidRestarts = 0
    private let latency = FirstWordLatency()

    func start(vocabulary: SpeechVocabulary) async {
        guard !cancelled, !ended else { return }
        if let problem = await authorize(false) { fail(problem); return }
        guard !cancelled, !ended else { return }
        self.vocabulary = vocabulary
        text = DictationTextCache(vocabulary: vocabulary)
        await prepare()
    }
    private func prepare() async {
        generation += 1
        let current = generation
        do {
            let transcriber = try await Self.transcriber(note: { [weak self] in self?.report(.note($0)) })
            guard !cancelled, !ended, generation == current else { return }
            let analyzer = SpeechAnalyzer(modules: [transcriber], options: .init(priority: .userInitiated, modelRetention: .lingering))
            self.analyzer = analyzer
            try await analyzer.setContext(Self.context(vocabulary))
            let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber], considering: microphone.inputFormat) ?? microphone.inputFormat
            try await analyzer.prepareToAnalyze(in: format)
            guard !cancelled, !ended, generation == current else { await analyzer.cancelAndFinishNow(); return }
            let (stream, input) = AsyncStream.makeStream(of: AnalyzerInput.self, bufferingPolicy: .bufferingNewest(256))
            results = Task { [weak self] in await self?.read(transcriber, generation: current) }
            try await analyzer.start(inputSequence: stream)
            guard !cancelled, !ended, generation == current else { await analyzer.cancelAndFinishNow(); return }
            try audio.resume(input, converter: BufferConverter(from: microphone.inputFormat, to: format))
            if finishing { finalize(analyzer) }
            else if !capturing {
                let audio = audio
                try microphone.start(lost: { @Sendable [weak self] in Task { @MainActor in self?.fail(.interrupted) } }) { @Sendable buffer in audio.append(buffer) }
                capturing = true
                meter.start(microphone) { [weak self] in self?.report(.level($0)) }
                report(.ready)
            }
        } catch { fail(error as? DictationProblem ?? .failed(error.localizedDescription)) }
    }
    func finish() {
        guard !finishing, !ended, !cancelled else { return }
        finishing = true
        meter.stop()
        microphone.stop()
        stopping = Task { [weak self] in
            try? await Task.sleep(for: DictationMachine.stoppingDeadline)
            guard !Task.isCancelled else { return }
            self?.complete()
        }
        if audio.finish(), let analyzer { finalize(analyzer) }
        // If a replacement analyzer is still preparing, it drains the queued buffers and finalizes when ready.
    }
    private func finalize(_ analyzer: SpeechAnalyzer) {
        Task { [weak self] in
            do { try await analyzer.finalizeAndFinishThroughEndOfInput() }
            catch { self?.fail(error as? DictationProblem ?? .failed(error.localizedDescription)) }
        }
    }
    func cancel() {
        cancelled = true
        onEvent = nil
        cleanUp()
    }
    private func cleanUp() {
        stopping?.cancel(); stopping = nil
        meter.stop()
        microphone.stop()
        audio.cancel()
        results?.cancel()
        if let analyzer { Task { await analyzer.cancelAndFinishNow() } }
    }
    private func read(_ transcriber: SpeechTranscriber, generation current: Int) async {
        var open = ""
        let streamStarted = ProcessInfo.processInfo.systemUptime
        do {
            for try await result in transcriber.results {
                guard !cancelled, !ended, generation == current else { return }
                let heard = String(result.text.characters)
                let empty = heard.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                if result.isFinal { text.settle(empty ? open : heard); open = "" }
                else if !empty { open = heard; latency.partial(heard, capture: microphone) }
                latest = text.text(open: open)
                report(.heard(latest))
            }
            guard !cancelled, !ended, generation == current else { return }
            if finishing { complete() }
            else {
                rapidRestarts = ProcessInfo.processInfo.systemUptime - streamStarted < 1 ? rapidRestarts + 1 : 0
                guard rapidRestarts <= 3 else { fail(.failed("The speech analyzer repeatedly ended without results.")); return }
                text.settle(open)
                text.beginSegment()
                audio.pause()
                let previous = analyzer
                analyzer = nil
                if let previous { await previous.cancelAndFinishNow() }
                guard !cancelled, !ended else { return }
                await prepare() // Capture and the iOS audio session stay active throughout this handoff.
            }
        } catch {
            if !Task.isCancelled, generation == current { fail(error as? DictationProblem ?? .failed(error.localizedDescription)) }
        }
    }
    private func complete() {
        guard !cancelled, !ended else { return }
        ended = true
        cleanUp()
        report(.finished(latest))
    }
    private func fail(_ problem: DictationProblem) {
        guard !cancelled, !ended else { return }
        ended = true
        cleanUp()
        report(.failed(problem))
    }
    private func report(_ event: SpeechEngineEvent) { onEvent?(event) }

    /// The transcriber for the phone's language, with its model installed (fetched by the system the first time, then on the phone).
    static func transcriber(note: @escaping @MainActor (String) -> Void = { _ in }) async throws -> SpeechTranscriber {
        let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale.current) ?? SpeechEngines.fallbackLocale
        let transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [.volatileResults, .fastResults], attributeOptions: [])
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

/// Speech's completion callback can run on any queue; take the continuation exactly once under a lock.
private final class FileRecognitionCompletion: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<String, any Error>?
    init(_ continuation: CheckedContinuation<String, any Error>) { self.continuation = continuation }
    func received(_ result: SFSpeechRecognitionResult?, error: (any Error)?) {
        guard result?.isFinal == true || error != nil else { return }
        lock.lock()
        let pending = continuation
        continuation = nil
        lock.unlock()
        if let result, result.isFinal { pending?.resume(returning: result.bestTranscription.formattedString) }
        else if let error { pending?.resume(throwing: error) }
    }
}

/// The audio thread always feeds the current request; a roll swaps it without restarting capture or replaying audio.
private final class RecognitionSink: @unchecked Sendable {
    private let lock = NSLock()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    func replace(_ request: SFSpeechAudioBufferRecognitionRequest?) {
        lock.lock(); defer { lock.unlock() }
        self.request?.endAudio()
        self.request = request
    }
    func append(_ buffer: AVAudioPCMBuffer) {
        lock.lock(); defer { lock.unlock() }
        request?.append(buffer)
    }
}

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
    private let sink = RecognitionSink()
    private let latency = FirstWordLatency()
    private var task: SFSpeechRecognitionTask?
    private var transcript = RollingDictationTranscript()
    private var recognizer: SFSpeechRecognizer?
    private var vocabulary = SpeechVocabulary(terms: [])
    private var rapidRestarts = 0
    private var requestStarted: TimeInterval = 0
    private var stopping: Task<Void, Never>?
    private var finishing = false
    private var done = false
    private var cancelled = false

    func start(vocabulary: SpeechVocabulary) async {
        guard !cancelled else { return }
        if let problem = await authorize(true) { report(.failed(problem)); return }
        guard !cancelled else { return }
        guard let recognizer = makeRecognizer() else { report(.failed(.unsupported)); return }
        self.recognizer = recognizer
        self.vocabulary = vocabulary
        transcript = RollingDictationTranscript(vocabulary: vocabulary)
        beginRequest()
        do {
            let sink = sink
            try microphone.start(lost: { [weak self] in Task { @MainActor in self?.lost() } }) { buffer in sink.append(buffer) }
            meter.start(microphone) { [weak self] in self?.report(.level($0)) }
            report(.ready)
        } catch {
            lost(problem: error as? DictationProblem ?? .failed(error.localizedDescription))
        }
    }
    private func beginRequest() {
        guard !done, let recognizer else { return }
        let request = Self.request(SFSpeechAudioBufferRecognitionRequest(), vocabulary: vocabulary)
        requestStarted = ProcessInfo.processInfo.systemUptime
        let generation = transcript.generation
        let previous = task
        task = recognizer.recognitionTask(with: request) { @Sendable [weak self] result, error in
            let text = result?.bestTranscription.formattedString
            let isFinal = result?.isFinal ?? false
            let failure = error.map { ($0 as NSError).localizedDescription }
            Task { @MainActor in self?.received(text: text, isFinal: isFinal, failure: failure, generation: generation) }
        }
        sink.replace(request)
        previous?.cancel()
    }
    func finish() {
        guard !done, !finishing else { return }
        finishing = true
        meter.stop()
        microphone.stop()
        sink.replace(nil)
        stopping = Task { [weak self] in
            try? await Task.sleep(for: DictationMachine.stoppingDeadline)
            guard !Task.isCancelled else { return }
            self?.complete()
        }
    }
    private func complete() {
        guard !done, !cancelled else { return }
        done = true
        stopping?.cancel(); stopping = nil
        task?.cancel()
        report(.finished(transcript.text))
    }
    func cancel() {
        cancelled = true
        done = true
        stopping?.cancel(); stopping = nil
        onEvent = nil
        meter.stop()
        microphone.stop()
        sink.replace(nil)
        task?.cancel()
    }
    private func lost(problem: DictationProblem = .interrupted) {
        guard !done else { return }
        done = true
        stopping?.cancel(); stopping = nil
        meter.stop()
        microphone.stop()
        sink.replace(nil)
        task?.cancel()
        report(.failed(problem))
    }
    private func received(text: String?, isFinal: Bool, failure: String?, generation: Int) {
        guard !done, generation == transcript.generation else { return }
        if let text {
            transcript.update(text, generation: generation)
            if !isFinal { latency.partial(text, capture: microphone) }
            report(.heard(transcript.text))
        }
        guard isFinal || failure != nil else { return }
        if finishing {
            complete()
        } else {
            // Final/error terminates a request, never a live dictation. Ignore late callbacks from the old generation.
            // Long quiet requests may end normally forever. Bound only immediate restart loops from a broken recognizer.
            rapidRestarts = ProcessInfo.processInfo.systemUptime - requestStarted < 1 ? rapidRestarts + 1 : 0
            guard rapidRestarts <= 3 else {
                lost(problem: .failed(failure ?? "The speech recognizer repeatedly ended without results.")); return
            }
            transcript.roll()
            beginRequest()
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
            let completion = FileRecognitionCompletion(continuation)
            recognizer.recognitionTask(with: request) { @Sendable result, error in completion.received(result, error: error) }
        }
        return rewrite ? vocabulary.rewrite(text) : text
    }
}
