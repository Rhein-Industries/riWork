import SwiftUI
import UIKit
@preconcurrency import AVFoundation
import Observation
import RiWorkCore

/// Which field a dictation is for. One dictation runs at a time, app-wide; the button of its owner shows it.
enum DictationOwner: Hashable, Sendable {
    case terminal
    case chat(String)
}

/// Runs dictations: one engine at a time, the rules of `DictationMachine`, and what stops one
/// from outside (a call, the app going to the background).
@MainActor @Observable final class DictationController {
    static let shared = DictationController()

    private(set) var phase: DictationPhase = .idle
    /// The owner of the current or the last dictation (whose button shows `phase`).
    private(set) var owner: DictationOwner?
    /// The microphone's level while listening, 0…1.
    private(set) var level: Float = 0
    /// The phase without its text, for views that should not follow every word (the key bar's mic).
    private(set) var stage = KeyBarView.Dictation.idle
    /// What the session on screen offers (`RemoteModel.speechSources`); set by the root view.
    @ObservationIgnored var sessionSources: @MainActor () -> SpeechVocabularySources = { SpeechVocabularySources() }
    /// Makes the engine for a dictation. Tests and the scripted debug mode put in their own.
    @ObservationIgnored var makeEngine: @MainActor () -> any SpeechEngine = { DictationController.defaultEngine() }
    /// The vocabulary the current dictation was given, for tests and the debug readout.
    @ObservationIgnored private(set) var vocabulary = SpeechVocabulary(terms: [])

    @ObservationIgnored private var machine = DictationMachine()
    @ObservationIgnored private var engine: (any SpeechEngine)?
    @ObservationIgnored private var deliver: ((String) -> Void)?
    @ObservationIgnored private var live: ((String) -> Void)?
    @ObservationIgnored private var observers: [any NSObjectProtocol] = []
    @ObservationIgnored private var completion: ((Bool) -> Void)?
    @ObservationIgnored private var stopping: Task<Void, Never>?
    /// Shared with the engines; injectable so tests control the deadline without sleeping.
    @ObservationIgnored var waitForFinal: @MainActor () async throws -> Void = {
        try await Task.sleep(for: DictationMachine.stoppingDeadline)
    }

    /// The desktop's mic setting (`DesktopStyle.mic`), kept by the root view. Off, no dictation starts (and so neither the microphone nor
    /// speech recognition is asked for), and one in progress is cancelled the moment it turns off.
    var isAllowed = false {
        didSet { if !isAllowed, oldValue, phase.isActive { cancel() } }
    }

    func isActive(for owner: DictationOwner) -> Bool { self.owner == owner && phase.isActive }
    func phase(for owner: DictationOwner) -> DictationPhase { self.owner == owner ? phase : .idle }

    /// The mic button: starts a dictation for `owner`, or stops the one it has. Another field keeps its words first.
    /// `live` sees the text while it is being heard (and "" when it is cancelled); `deliver` gets the final text, once.
    func toggle(for owner: DictationOwner, live: ((String) -> Void)? = nil, deliver: @escaping (String) -> Void) {
        if self.owner == owner, phase.isActive { stop(); return }
        guard isAllowed else { return }
        if phase.isActive { finishNow() }
        self.owner = owner
        self.deliver = deliver
        self.live = live
        vocabulary = SpeechVocabulary.build(sessionSources())
        send(.start)
    }
    func stop() { send(.stop) }
    /// Complete insertion before running the action, even for an empty result or an engine that never finishes.
    func stop(then action: @escaping (Bool) -> Void) {
        guard phase.isActive else { action(true); return }
        guard completion == nil else { action(false); return }
        completion = action
        stop()
    }
    func cancel() { send(.cancel) }
    /// Leaving a field keeps its latest text, but must never complete a pending send in another view.
    func finishNow() {
        let completion = completion
        self.completion = nil
        if phase.isActive { send(.finished(phase.text)) }
        completion?(false)
    }
    /// The failure was seen.
    func dismiss() { send(.dismiss) }

    private func send(_ event: DictationMachine.Event) {
        let before = machine.phase
        let effect = machine.handle(event)
        if machine.phase != phase { phase = machine.phase }
        let stage: KeyBarView.Dictation = switch phase {
        case .listening: .listening
        case .preparing, .finishing: .busy
        case .idle, .failed: .idle
        }
        if stage != self.stage { self.stage = stage }
        if before.text != phase.text, phase.isActive { live?(phase.text) }
        switch effect {
        case .none: break
        case .beginEngine: begin()
        case .finishEngine:
            let waitForFinal = waitForFinal
            stopping = Task { [weak self] in
                try? await waitForFinal()
                guard !Task.isCancelled, let self, case .finishing = self.phase else { return }
                self.send(.finished(self.phase.text))
            }
            engine?.finish()
        case .cancelEngine: live?(""); tearDown()
        case .deliver(let text): let deliver = deliver; tearDown(); deliver?(text)
        }
        // Finished with nothing heard: whatever was shown goes, and so does the engine.
        if !phase.isActive, engine != nil, effect == .none { live?(""); tearDown() }
        if !phase.isActive, let completion {
            self.completion = nil
            // A recognizer failure keeps text but does not authorize sending it.
            if case .finished = event { completion(true) } else { completion(false) }
        }
    }

    private func begin() {
        let engine = makeEngine()
        self.engine = engine
        engine.onEvent = { [weak self, weak engine] event in
            guard let self, let engine, self.engine === engine else { return }
            self.received(event)
        }
        observeInterruptions()
        let vocabulary = vocabulary
        Task { await engine.start(vocabulary: vocabulary) }
    }
    private func received(_ event: SpeechEngineEvent) {
        switch event {
        case .note(let note): send(.note(note))
        case .ready: send(.ready)
        case .heard(let text): if text != phase.text { send(.heard(text)) }
        case .finished(let text): send(.finished(text))
        case .failed(let problem): send(.failed(problem))
        case .level(let value): if abs(value - level) > 0.02 { level = value }
        }
    }
    private func tearDown() {
        stopping?.cancel(); stopping = nil
        engine?.onEvent = nil
        engine?.cancel()
        engine = nil
        deliver = nil
        live = nil
        level = 0
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
    }

    /// A call or Siri takes the microphone: what was said so far is kept. Leaving the app ends the dictation the same way as Stop.
    private func observeInterruptions() {
        guard observers.isEmpty else { return }
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) { [weak self] note in
            let began = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt).flatMap(AVAudioSession.InterruptionType.init) == .began
            MainActor.assumeIsolated { if began { self?.send(.failed(.interrupted)) } }
        })
        observers.append(center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.stop() }
        })
    }

    /// The phone's recognizer, or the scripted one when the app was launched to show dictation without a microphone
    /// (`-RiWorkScriptedDictation "words to hear"`, Debug builds only).
    static func defaultEngine() -> any SpeechEngine {
        #if DEBUG
        if let script = UserDefaults.standard.string(forKey: "RiWorkScriptedDictation") { return ScriptedSpeechEngine(script: script) }
        #endif
        return SpeechEngines.make()
    }
}

/// An engine that "hears" a fixed text a word at a time and never touches the microphone: for tests, screenshots and the simulator.
@MainActor final class ScriptedSpeechEngine: SpeechEngine {
    var onEvent: ((SpeechEngineEvent) -> Void)?
    let words: [String]
    let interval: Duration
    /// Stop after the last word without waiting for `finish` (simulated engine completion).
    let holdOpen: Bool
    private(set) var started = false, finished = false, cancelled = false
    private(set) var vocabulary: SpeechVocabulary?
    private var heard = 0
    private var task: Task<Void, Never>?

    init(script: String, interval: Duration = .milliseconds(350), holdOpen: Bool = true) {
        words = script.split(separator: " ").map(String.init)
        self.interval = interval
        self.holdOpen = holdOpen
    }
    func start(vocabulary: SpeechVocabulary) async {
        started = true
        self.vocabulary = vocabulary
        onEvent?(.ready)
        task = Task { [weak self] in
            guard let self else { return }
            while heard < words.count, !Task.isCancelled {
                try? await Task.sleep(for: interval)
                guard !Task.isCancelled else { return }
                heard += 1
                onEvent?(.level(Float(heard % 3 + 1) / 3))
                onEvent?(.heard(text))
            }
            if !holdOpen, !Task.isCancelled { onEvent?(.finished(text)) }
        }
    }
    var text: String { vocabulary?.rewrite(words.prefix(heard).joined(separator: " ")) ?? words.prefix(heard).joined(separator: " ") }
    func finish() {
        finished = true
        task?.cancel()
        let text = text
        Task { @MainActor [weak self] in self?.onEvent?(.finished(text)) }
    }
    func cancel() { cancelled = true; task?.cancel(); onEvent = nil }
}

// MARK: - Putting the text in a text view

/// Dictation into a text view at its caret: the words appear as they are heard, and the final text replaces them. Cancelling takes
/// them out again. The view's delegate is told of each change as if it had been typed, so its binding follows.
@MainActor final class TextInsertion {
    weak var view: UITextView?
    private var base: String?
    private var anchor = NSRange(location: 0, length: 0)
    private var written: String?
    private var writtenRange = NSRange(location: 0, length: 0)
    private var userTookOver = false

    /// Shows `text` at the caret in place of what was shown before; "" takes it out.
    func show(_ text: String) {
        if text.isEmpty { discard(); return }
        guard !userTookOver, let view else { return }
        let selection = view.selectedRange
        let updating = written != nil
        if let written, view.text != written { rebaseTyping(from: written, to: view.text ?? "") }
        guard !userTookOver else { return }
        if base == nil || view.text != written {
            let text = view.text ?? ""
            base = text
            // A composer that never had the keyboard has its caret at the start: dictation then adds to the end.
            anchor = !view.isFirstResponder && view.selectedRange.location == 0 && !text.isEmpty ? NSRange(location: (text as NSString).length, length: 0) : view.selectedRange
        }
        guard let base else { return }
        let result = text.isEmpty ? (text: base, caret: anchor.location + anchor.length) : DictatedText.insert(text, into: base, at: anchor)
        let oldEnd = NSMaxRange(writtenRange)
        let newLength = (result.text as NSString).length - (base as NSString).length + anchor.length
        let delta = newLength - writtenRange.length
        writtenRange = NSRange(location: anchor.location, length: newLength)
        let caret: NSRange
        if updating {
            func shifted(_ offset: Int) -> Int { offset >= oldEnd ? offset + delta : offset }
            caret = NSRange(location: shifted(selection.location), length: max(0, shifted(NSMaxRange(selection)) - shifted(selection.location)))
        } else { caret = NSRange(location: result.caret, length: 0) }
        apply((result.text, caret.location), to: view, selection: caret)
    }
    /// A typed edit before or after the live span belongs to the base, not to a second copy of the transcript.
    private func rebaseTyping(from old: String, to new: String) {
        guard let base else { return }
        let a = Array(old.utf16), b = Array(new.utf16)
        var prefix = 0, suffix = 0
        while prefix < min(a.count, b.count), a[prefix] == b[prefix] { prefix += 1 }
        while suffix < min(a.count, b.count) - prefix, a[a.count - 1 - suffix] == b[b.count - 1 - suffix] { suffix += 1 }
        let removed = a.count - prefix - suffix
        let replacement = (new as NSString).substring(with: NSRange(location: prefix, length: b.count - prefix - suffix))
        let location: Int
        if prefix + removed <= writtenRange.location {
            location = prefix
            anchor.location += b.count - a.count
            writtenRange.location += b.count - a.count
        } else if prefix >= NSMaxRange(writtenRange) {
            location = prefix - writtenRange.length + anchor.length
        } else { userTookOver = true; return }
        self.base = (base as NSString).replacingCharacters(in: NSRange(location: location, length: removed), with: replacement)
        written = new
    }
    /// The final text, at the caret. The next dictation starts from where this one left the caret.
    func commit(_ text: String) {
        show(text)
        base = nil; written = nil; userTookOver = false
    }
    /// Takes out what dictation showed.
    func discard() {
        guard base != nil else { return }
        if let view, let base, view.text == written {
            apply((text: base, caret: anchor.location + anchor.length), to: view)
        }
        base = nil; written = nil; userTookOver = false
    }
    private func apply(_ result: (text: String, caret: Int), to view: UITextView, selection: NSRange? = nil) {
        guard view.text != result.text else { return }
        let composer = view as? ChatComposerTextView
        composer?.applyingDictation = true
        defer { composer?.applyingDictation = false }
        view.text = result.text
        let length = (result.text as NSString).length
        let requested = selection ?? NSRange(location: result.caret, length: 0)
        let location = min(max(0, requested.location), length)
        view.selectedRange = NSRange(location: location, length: min(requested.length, length - location))
        written = view.text
        view.delegate?.textViewDidChange?(view)
        view.scrollRangeToVisible(view.selectedRange)
    }
}

// MARK: - The mic button (SwiftUI)

/// The mic beside a Send button: tap to dictate, tap again to stop; outside the compact chat, a cancel button sits beside it. Text goes into
/// `insertion`'s text view at the caret as it is heard.
struct DictationButton: View {
    @Environment(\.desktopStyle) private var style
    let owner: DictationOwner
    let insertion: TextInsertion
    var controller = DictationController.shared
    var isEnabled = true
    /// Inside the chat composer's field: a smaller glyph, in the column of the field's buttons (a 30-point circle and 8 points) at the
    /// trailing end of its 44-point target.
    var compact = false
    /// Compact: how far the mic's target reaches out past the column (and the field's edge), to the screen's edge.
    var reach: CGFloat = 0
    private var column: CGFloat { style.pt(30) + style.pt(8) }
    /// Says a failure in an alert of its own; off where the screen says it in its banner row (the chat).
    var alerts = true
    @State private var hardware = HardwareKeyboardMonitor()

    var body: some View {
        let phase = controller.phase(for: owner)
        HStack(spacing: 0) {
            if phase.isActive && !compact {
                Button { controller.cancel() } label: {
                    Image(systemName: "xmark.circle").font(.system(size: style.pt(compact ? 17 : 20))).foregroundStyle(style.muted)
                        .frame(width: compact ? style.pt(30) : nil).frame(width: compact ? column : nil, alignment: .leading)
                }
                .buttonStyle(TargetButtonStyle(dims: false, alignment: compact ? .trailing : .center))
                .accessibilityLabel("Cancel dictation").accessibilityHint("Takes out what was dictated")
                .transition(.opacity)
            }
            Button(action: toggle) {
                DictationGlyph(phase: phase, level: controller.level, size: compact ? 20 : 24).frame(width: compact ? style.pt(30) : nil)
                    .frame(width: compact ? column + reach : nil, alignment: .leading)
            }
                .buttonStyle(TargetButtonStyle(dims: false, alignment: compact ? .trailing : .center))
                .chatLayoutProbe("dictation-target", action: toggle)
                .padding(.trailing, compact ? -reach : 0)
                .disabled(!isEnabled && !phase.isActive)
                .contextMenu {
                    if phase.isActive { Button("Cancel dictation", systemImage: "xmark", action: cancel) }
                }
                .accessibilityAction(named: "Cancel dictation", cancel)
                .chatLayoutProbe("dictation-cancel", visible: phase.isActive, action: cancel)
                .accessibilityLabel(phase.isActive ? "Stop dictation" : "Dictate")
                .accessibilityValue(phase.isActive ? "Listening" : "")
                .accessibilityHint(Self.hint(active: phase.isActive, compact: compact, hardware: hardware.isAttached))
                .accessibilityIdentifier("dictation.\(owner.identifier)")
        }
        .animation(.easeInOut(duration: 0.15), value: phase.isActive)
        .dictationAlert(controller, owner: owner, enabled: alerts)
        // Another chat in the same composer, or the chat gone from the screen: its words must not land in the field now shown.
        .onChange(of: owner) { old, _ in if controller.isActive(for: old) { controller.finishNow() } }
        .onDisappear { if controller.isActive(for: owner) { controller.finishNow() } }
    }
    static func hint(active: Bool, compact: Bool, hardware: Bool) -> String {
        guard active else { return "Speak a message; it is recognized on this iPhone" }
        guard compact else { return "Keeps what was heard" }
        return hardware ? "Keeps what was heard. Escape stops; Return or Send stops and sends." : "Keeps what was heard. Send stops and sends. Touch and hold to cancel."
    }
    private func cancel() { if controller.isActive(for: owner) { controller.cancel() } }
    private func toggle() {
        let insertion = insertion
        if !controller.isActive(for: owner), controller.isAllowed { (insertion.view as? ChatComposerTextView)?.holdDictationHeight() }
        controller.toggle(for: owner, live: { insertion.show($0) }, deliver: { insertion.commit($0) })
    }
}

/// The mic in its states: a plain mic, a filled one with a ring that follows the voice while listening, a spinner while getting ready
/// or settling.
struct DictationGlyph: View {
    @Environment(\.desktopStyle) private var style
    let phase: DictationPhase
    let level: Float
    var size: CGFloat = 24
    var body: some View {
        ZStack {
            switch phase {
            case .listening:
                Circle().fill(style.accent.opacity(0.18)).frame(width: style.pt(size + 6), height: style.pt(size + 6))
                    .scaleEffect(1 + CGFloat(level) * 0.35).animation(.easeOut(duration: 0.1), value: level)
                Image(systemName: "mic.fill").font(.system(size: style.pt(size * 0.75))).foregroundStyle(style.accent)
            case .preparing, .finishing:
                ProgressView().controlSize(.small).tint(style.accent)
            case .failed:
                Image(systemName: "mic.slash").font(.system(size: style.pt(size * 0.75))).foregroundStyle(style.muted)
            case .idle:
                Image(systemName: "mic").font(.system(size: style.pt(size * 0.75))).foregroundStyle(style.text)
            }
        }
        .frame(width: style.pt(size + 10), height: style.pt(size + 10))
    }
}

extension DictationOwner {
    var identifier: String {
        switch self {
        case .terminal: "terminal"
        case .chat: "chat"
        }
    }
}

extension View {
    /// Says why a dictation of `owner` stopped, with the way to Settings when a permission is off.
    func dictationAlert(_ controller: DictationController, owner: DictationOwner, enabled: Bool = true) -> some View {
        modifier(DictationAlert(controller: controller, owner: owner, enabled: enabled))
    }
}
private struct DictationAlert: ViewModifier {
    let controller: DictationController
    let owner: DictationOwner
    let enabled: Bool
    func body(content: Content) -> some View {
        let problem: DictationProblem? = if enabled, case .failed(let problem) = controller.phase(for: owner) { problem } else { nil }
        content.alert("Dictation", isPresented: Binding(get: { problem != nil }, set: { if !$0 { controller.dismiss() } })) {
            if problem?.opensSettings == true {
                Button("Open Settings") {
                    controller.dismiss()
                    if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                }
            }
            Button("OK", role: .cancel) { controller.dismiss() }
        } message: {
            Text(problem?.message ?? "")
        }
    }
}
