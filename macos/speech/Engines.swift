@preconcurrency import AVFoundation
import Speech
import OSLog
import CoreAudio

// Dictation on the Mac, with Apple's on-device recognizers only: speech never leaves the Mac and nothing is sent to a server to
// recognize it. macOS 26 and later use `SpeechAnalyzer` with `SpeechTranscriber`; older systems use `SFSpeechRecognizer` with
// `requiresOnDeviceRecognition`. These are the phone's engines (ios/RiWorkRemote/SpeechEngine.swift) without the iPhone's audio
// session: both are handed the session's vocabulary as contextual strings, and what they hear is rewritten onto it
// (`SpeechVocabulary.rewrite`, shared with the phone from ios/Core).

/// Why dictation could not go on, as the app is told it.
enum Problem: Error, Equatable, Sendable {
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
    /// All of it, settled. Comes once, after `finish`.
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
    private var deviceListener: AudioObjectPropertyListenerBlock?
    private var defaultInput = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultInputDevice,
                                                        mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    private var watch: Task<Void, Never>?
    private var recovery: Task<Void, Never>?
    private let activity = CaptureActivity()
    private(set) var captureStart: TimeInterval?
    var inputFormat: AVAudioFormat { engine.inputNode.outputFormat(forBus: 0) }
    var firstNonSilent: TimeInterval? { activity.watchdog.firstNonSilent }
    var level: Float { activity.level }
    func start(lost: @escaping @Sendable () -> Void, _ handle: @escaping @Sendable (AVAudioPCMBuffer) -> Void) throws {
        running = true
        do { try install(handle, lost: lost) } catch { stop(); throw error }
        let changed: AudioObjectPropertyListenerBlock = { @Sendable [weak self] _, _ in
            Task { @MainActor in self?.recover(handle, lost: lost) }
        }
        if AudioObjectAddPropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &defaultInput, .main, changed) == noErr {
            deviceListener = changed
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
        guard format.sampleRate > 0, format.channelCount > 0 else { throw Problem.microphoneBusy("no microphone is available") }
        input.installTap(onBus: 0, bufferSize: 1024, format: format, block: Self.tap(activity: activity, handle: handle))
        tapInstalled = true
        engine.prepare()
        let start = ProcessInfo.processInfo.systemUptime
        do { try engine.start() } catch { throw Problem.microphoneBusy(error.localizedDescription) }
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
        if let deviceListener { AudioObjectRemovePropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &defaultInput, .main, deviceListener) }
        deviceListener = nil
        releaseEngine()

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
        if input != output, converter == nil { throw Problem.failed("The microphone audio format cannot be converted.") }
    }
    func convert(_ buffer: AVAudioPCMBuffer) throws -> AVAudioPCMBuffer? {
        if buffer.format != sourceFormat {
            sourceFormat = buffer.format
            converter = sourceFormat == output ? nil : AVAudioConverter(from: sourceFormat, to: output)
        }
        if sourceFormat != output, converter == nil { throw Problem.failed("The microphone audio format cannot be converted.") }
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

/// A bounded handoff between continuous capture and replaceable analyzers. During a restart it owns copies of arriving buffers.
@available(macOS 26, *)
private final class AnalyzerAudioSink: @unchecked Sendable {
    private let lock = NSLock()
    private var input: AsyncStream<AnalyzerInput>.Continuation?
    private var converter: BufferConverter?
    private var pending: [AVAudioPCMBuffer] = []
    private var accepting = true
    private var cancelled = false
    private let failed: @Sendable (Problem) -> Void
    init(failed: @escaping @Sendable (Problem) -> Void) { self.failed = failed }
    func append(_ buffer: AVAudioPCMBuffer) {
        var problem: Problem?
        lock.lock()
        if accepting, !cancelled {
            do {
                let delivered: Bool
                if let input, let converter { delivered = try send(buffer, to: input, converter: converter) }
                else { delivered = false }
                if !delivered {
                    self.input = nil
                    guard pending.count < 256, let copy = AVAudioPCMBuffer(pcmFormat: buffer.format, frameCapacity: buffer.frameLength) else {
                        throw Problem.failed("Speech processing could not keep up with the microphone.")
                    }
                    copy.frameLength = buffer.frameLength
                    let source = UnsafeMutableAudioBufferListPointer(buffer.mutableAudioBufferList)
                    let destination = UnsafeMutableAudioBufferListPointer(copy.mutableAudioBufferList)
                    for (from, to) in zip(source, destination) {
                        if let fromData = from.mData, let toData = to.mData { memcpy(toData, fromData, min(Int(from.mDataByteSize), Int(to.mDataByteSize))) }
                    }
                    pending.append(copy)
                }
            } catch { problem = error as? Problem ?? .failed(error.localizedDescription) }
        }
        lock.unlock()
        if let problem { failed(problem) }
    }
    private func send(_ buffer: AVAudioPCMBuffer, to input: AsyncStream<AnalyzerInput>.Continuation, converter: BufferConverter) throws -> Bool {
        guard let converted = try converter.convert(buffer) else { throw Problem.failed("The microphone audio could not be converted.") }
        switch input.yield(AnalyzerInput(buffer: converted)) {
        case .enqueued: return true
        case .terminated: return false
        case .dropped: throw Problem.failed("Speech processing could not keep up with the microphone.")
        @unknown default: return false
        }
    }
    func resume(_ input: AsyncStream<AnalyzerInput>.Continuation, converter: BufferConverter) throws {
        lock.lock(); defer { lock.unlock() }
        guard !cancelled else { input.finish(); return }
        self.input = input
        self.converter = converter
        for buffer in pending {
            guard try send(buffer, to: input, converter: converter) else { throw Problem.failed("The speech analyzer ended during recovery.") }
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

@available(macOS 26, *)
@MainActor final class AnalyzerSpeechEngine: SpeechEngine {
    var onEvent: ((EngineEvent) -> Void)?
    private let microphone = MicrophoneTap()
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
        if let problem = await SpeechEngines.authorize(recognition: false) { fail(problem); return }
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
                report(.ready)
            }
        } catch { fail(error as? Problem ?? .failed(error.localizedDescription)) }
    }
    func finish() {
        guard !finishing, !ended, !cancelled else { return }
        finishing = true
        microphone.stop()
        stopping = Task { [weak self] in
            try? await Task.sleep(for: .seconds(5))
            guard !Task.isCancelled else { return }
            self?.complete()
        }
        if audio.finish(), let analyzer { finalize(analyzer) }
        // If a replacement analyzer is still preparing, it drains the queued buffers and finalizes when ready.
    }
    private func finalize(_ analyzer: SpeechAnalyzer) {
        Task { [weak self] in
            do { try await analyzer.finalizeAndFinishThroughEndOfInput() }
            catch { self?.fail(error as? Problem ?? .failed(error.localizedDescription)) }
        }
    }
    func cancel() {
        cancelled = true
        onEvent = nil
        cleanUp()
    }
    private func cleanUp() {
        stopping?.cancel(); stopping = nil
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
                if result.isFinal { text.settle(heard); open = "" }
                else { open = heard; latency.partial(heard, capture: microphone) }
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
            if !Task.isCancelled, generation == current { fail(error as? Problem ?? .failed(error.localizedDescription)) }
        }
    }
    private func complete() {
        guard !cancelled, !ended else { return }
        ended = true
        cleanUp()
        report(.finished(latest))
    }
    private func fail(_ problem: Problem) {
        guard !cancelled, !ended else { return }
        ended = true
        cleanUp()
        report(.failed(problem))
    }
    private func report(_ event: EngineEvent) { onEvent?(event) }

    /// The transcriber for the Mac's language, with its model installed (fetched by the system the first time, then on the Mac).
    static func transcriber(note: @escaping @MainActor (String) -> Void = { _ in }) async throws -> SpeechTranscriber {
        let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale.current) ?? SpeechEngines.fallbackLocale
        let transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [.volatileResults, .fastResults], attributeOptions: [])
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
    var onEvent: ((EngineEvent) -> Void)?
    private let microphone = MicrophoneTap()
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
        if let problem = await SpeechEngines.authorize(recognition: true) { report(.failed(problem)); return }
        guard !cancelled else { return }
        guard let recognizer = Self.recognizer() else { report(.failed(.unsupported)); return }
        self.recognizer = recognizer
        self.vocabulary = vocabulary
        transcript = RollingDictationTranscript(vocabulary: vocabulary)
        beginRequest()
        do {
            let sink = sink
            try microphone.start(lost: { [weak self] in Task { @MainActor in self?.lost() } }) { buffer in sink.append(buffer) }
            report(.ready)
        } catch {
            lost(problem: error as? Problem ?? .failed(error.localizedDescription))
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
        microphone.stop()
        sink.replace(nil)
        stopping = Task { [weak self] in
            try? await Task.sleep(for: .seconds(5))
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
        microphone.stop()
        sink.replace(nil)
        task?.cancel()
    }
    private func lost(problem: Problem = .interrupted) {
        guard !done else { return }
        done = true
        stopping?.cancel(); stopping = nil
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

// MARK: - Capture for RiWork's MLX speech models

/// What the MLX speech models take: 16 kHz mono float32.
let captureFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!

/// Writes converted audio as `{"event":"audio","data":"<base64 float32 little-endian>"}` lines. Used from the audio thread;
/// `close` waits for a buffer in flight, so no audio follows `finished`.
final class CaptureSink: @unchecked Sendable {
    private let lock = NSLock()
    private let converter: BufferConverter
    private var open = true
    init(converter: BufferConverter) { self.converter = converter }
    func append(_ buffer: AVAudioPCMBuffer) {
        lock.lock(); defer { lock.unlock() }
        guard open, let converted = try? converter.convert(buffer), let samples = converted.floatChannelData?[0],
              converted.frameLength > 0 else { return }
        let data = Data(bytes: samples, count: Int(converted.frameLength) * MemoryLayout<Float>.size)
        emit(["event": "audio", "data": data.base64EncodedString()])
    }
    func close() { lock.lock(); open = false; lock.unlock() }
}

/// The microphone for RiWork's own speech worker, which does the recognizing: this engine only listens, so it needs microphone
/// access and never speech recognition. `heard` never comes from here; `finished` is empty and only says the audio has ended.
@MainActor final class CaptureSpeechEngine: SpeechEngine {
    var onEvent: ((EngineEvent) -> Void)?
    private let microphone = MicrophoneTap()
    private var sink: CaptureSink?
    func start(vocabulary: SpeechVocabulary) async {
        if let problem = await SpeechEngines.authorize(recognition: false) { onEvent?(.failed(problem)); return }
        do {
            let sink = CaptureSink(converter: try BufferConverter(from: microphone.inputFormat, to: captureFormat))
            self.sink = sink
            try microphone.start(lost: { @Sendable [weak self] in
                Task { @MainActor in self?.stop(); self?.onEvent?(.failed(.interrupted)) }
            }) { buffer in sink.append(buffer) }
            onEvent?(.ready)
        } catch {
            stop()
            onEvent?(.failed(error as? Problem ?? .failed(error.localizedDescription)))
        }
    }
    private func stop() { microphone.stop(); sink?.close() }
    func finish() { stop(); onEvent?(.finished("")) }
    func cancel() { stop(); onEvent = nil }
}

/// A recording in place of the microphone, at the pace it was spoken, for tests of the whole path without a microphone. After the
/// recording it sends silence, as a person who stopped talking, until `finish` (at most a minute).
@MainActor final class FileCaptureEngine: SpeechEngine {
    var onEvent: ((EngineEvent) -> Void)?
    let url: URL
    private var task: Task<Void, Never>?
    private var sink: CaptureSink?
    init(url: URL) { self.url = url }
    func start(vocabulary: SpeechVocabulary) async {
        do {
            let file = try AVAudioFile(forReading: url)
            let format = file.processingFormat
            let sink = CaptureSink(converter: try BufferConverter(from: format, to: captureFormat))
            self.sink = sink
            let frames = AVAudioFrameCount(format.sampleRate / 10)
            onEvent?(.ready)
            task = Task { @MainActor in
                var silent = 0
                while !Task.isCancelled, silent < 600 {
                    guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames) else { return }
                    if file.framePosition < file.length {
                        try? file.read(into: buffer, frameCount: frames)
                    } else {
                        buffer.frameLength = frames
                        for channel in 0..<Int(format.channelCount) {
                            if let samples = buffer.floatChannelData?[channel] { samples.update(repeating: 0, count: Int(frames)) }
                        }
                        silent += 1
                    }
                    sink.append(buffer)
                    try? await Task.sleep(for: .milliseconds(100))
                }
            }
        } catch {
            onEvent?(.failed(.failed("The recording could not be read: \(error.localizedDescription)")))
        }
    }
    func finish() { task?.cancel(); sink?.close(); onEvent?(.finished("")) }
    func cancel() { task?.cancel(); sink?.close(); onEvent = nil }
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
