import Foundation

/// Why dictation could not go on.
public enum DictationProblem: Error, Sendable, Equatable {
    case microphoneDenied
    case recognitionDenied
    /// No on-device recognizer for the language (the phone never sends speech to a server for this).
    case unsupported
    /// The speech model is not on the phone yet and could not be fetched.
    case modelUnavailable
    case microphoneBusy(String)
    /// A call, Siri or another app took the microphone.
    case interrupted
    case failed(String)

    public var message: String {
        switch self {
        case .microphoneDenied: "Microphone access is off for RiWork. Turn it on in Settings to dictate."
        case .recognitionDenied: "Speech recognition is off for RiWork. Turn it on in Settings to dictate."
        case .unsupported: "On-device dictation is not available for this language on this iPhone."
        case .modelUnavailable: "The speech model could not be installed. Check the connection and try again."
        case .microphoneBusy(let reason): "The microphone could not be opened: \(reason)"
        case .interrupted: "Dictation stopped: the microphone is unavailable or was interrupted."
        case .failed(let reason): "Dictation stopped: \(reason)"
        }
    }
    /// The fix is in the Settings app.
    public var opensSettings: Bool { self == .microphoneDenied || self == .recognitionDenied }
}

/// Where one dictation is.
public enum DictationPhase: Sendable, Equatable {
    case idle
    /// Permissions, the speech model and the microphone are being got ready. `note` says what takes long ("Installing speech model…").
    case preparing(note: String?)
    /// Listening; `text` is what has been heard so far, the last words of it still open to change.
    case listening(text: String)
    /// Stopped by the person: the last words are being settled.
    case finishing(text: String)
    case failed(DictationProblem)

    public var isActive: Bool {
        switch self {
        case .preparing, .listening, .finishing: true
        case .idle, .failed: false
        }
    }
    public var text: String {
        switch self {
        case .listening(let text), .finishing(let text): text
        default: ""
        }
    }
}

/// The rules of one dictation, apart from audio and recognizers so they can be tested: tap to start, tap to stop, cancel at any time,
/// and the text is handed over exactly once, when it is final.
public struct DictationMachine: Sendable, Equatable {
    public enum Event: Sendable, Equatable {
        case start
        /// Getting ready takes a while for a reason worth saying.
        case note(String)
        /// The microphone is open.
        case ready
        /// What has been heard so far.
        case heard(String)
        /// The person tapped stop.
        case stop
        /// The recognizer has finished; this is all of it.
        case finished(String)
        case cancel
        case failed(DictationProblem)
        /// The message about a failure was seen.
        case dismiss
    }
    public enum Effect: Sendable, Equatable {
        case none
        case beginEngine
        /// Stop listening and settle the text.
        case finishEngine
        case cancelEngine
        /// The text for the field. Never empty.
        case deliver(String)
    }

    public private(set) var phase: DictationPhase = .idle
    public init() {}

    public mutating func handle(_ event: Event) -> Effect {
        switch (phase, event) {
        case (.idle, .start), (.failed, .start):
            phase = .preparing(note: nil); return .beginEngine
        case (.preparing, .note(let note)):
            phase = .preparing(note: note); return .none
        case (.preparing, .ready):
            phase = .listening(text: ""); return .none
        case (.preparing, .heard(let text)), (.listening, .heard(let text)):
            phase = .listening(text: text); return .none
        case (.finishing, .heard(let text)):
            phase = .finishing(text: text); return .none
        case (.preparing, .stop):
            // Nothing can have been heard yet: stopping is cancelling.
            phase = .idle; return .cancelEngine
        case (.listening(let text), .stop):
            phase = .finishing(text: text); return .finishEngine
        case (.preparing, .finished(let text)), (.listening, .finished(let text)), (.finishing, .finished(let text)):
            phase = .idle
            let final = text.trimmingCharacters(in: .whitespacesAndNewlines)
            return final.isEmpty ? .none : .deliver(final)
        case (_, .cancel) where phase.isActive:
            phase = .idle; return .cancelEngine
        case (.listening(let text), .failed(let problem)), (.finishing(let text), .failed(let problem)):
            // Every failure keeps what was said before it.
            let kept = text.trimmingCharacters(in: .whitespacesAndNewlines)
            phase = .failed(problem)
            return kept.isEmpty ? .cancelEngine : .deliver(kept)
        case (_, .failed(let problem)) where phase.isActive:
            phase = .failed(problem); return .cancelEngine
        case (.failed, .dismiss):
            phase = .idle; return .none
        default:
            return .none
        }
    }
}
