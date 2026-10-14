import Foundation

// One free-text Markdown memo per project: the person's own notes, kept on the Mac beside the project and read and saved from the
// phone with `project.memo.get` and `project.memo.set` ("Project memo extension" in docs/remote-protocol.md). A save replaces the whole
// memo and the last one to reach the Mac wins: no merge. Text the Mac has not taken yet stays on this phone (`ProjectMemoDraftStore`)
// and is saved again once it can be, never dropped. The memo reaches a chat only when the person inserts it with `/memo`.

// MARK: - Text rules (the same as `riwork_core::project_memo` on the Mac)

public enum ProjectMemoText {
    /// The most a memo may hold, in UTF-8 bytes after `normalize`: 32 KiB. The whole memo goes in one request, which still fits the
    /// link's frame when JSON escapes double it.
    public static let maximumBytes = 32 * 1024
    /// The chat composer command that previews the project's memo.
    public static let command = "/memo"

    /// Line breaks as `\n` (`\r\n` and a lone `\r` become one), and no control character but tab and newline. Nothing else changes.
    public static func normalize(_ text: String) -> String {
        let scalars = Array(text.unicodeScalars)
        guard scalars.contains(where: { isControl($0) && $0 != "\n" && $0 != "\t" }) else { return text }
        var out = String.UnicodeScalarView()
        var index = 0
        while index < scalars.count {
            let scalar = scalars[index]
            if scalar == "\r" {
                out.append("\n")
                if index + 1 < scalars.count, scalars[index + 1] == "\n" { index += 1 }
            } else if scalar == "\n" || scalar == "\t" || !isControl(scalar) {
                out.append(scalar)
            }
            index += 1
        }
        return String(out)
    }

    /// Rust's `char::is_control`: the C0 and C1 controls and DEL.
    static func isControl(_ scalar: Unicode.Scalar) -> Bool {
        scalar.value < 0x20 || (0x7F...0x9F).contains(scalar.value)
    }

    /// How many bytes a normalized text is over the limit, if it is.
    public static func bytesOver(_ text: String) -> Int? {
        let over = text.utf8.count - maximumBytes
        return over > 0 ? over : nil
    }

    /// Why a normalized text cannot be saved, in words for the person, or nil.
    public static func tooLargeMessage(_ text: String) -> String? {
        guard bytesOver(text) != nil else { return nil }
        return "The memo is \(text.utf8.count) bytes; a memo holds at most \(maximumBytes) bytes (32 KB). Shorten it to save."
    }

    /// What is left of `draft` when it ends in `/memo` (alone, or after whitespace, with only whitespace after it), or nil.
    public static func commandRest(_ draft: String) -> String? {
        let trimmed = Array(trimEnd(draft).unicodeScalars)
        let command = Array(Self.command.unicodeScalars)
        guard trimmed.count >= command.count, Array(trimmed.suffix(command.count)) == command else { return nil }
        let rest = trimmed.dropLast(command.count)
        guard rest.isEmpty || rest.last!.properties.isWhitespace else { return nil }
        var view = String.UnicodeScalarView()
        view.append(contentsOf: rest)
        return trimEnd(String(view))
    }

    /// `draft` with the memo after a blank line, or the memo alone for an empty draft; the memo's trailing line breaks dropped.
    public static func appended(to draft: String, memo: String) -> String {
        var memoScalars = Array(memo.unicodeScalars)
        while let last = memoScalars.last, last == "\n" || last == "\r" { memoScalars.removeLast() }
        var memoView = String.UnicodeScalarView()
        memoView.append(contentsOf: memoScalars)
        let head = trimEnd(draft)
        return head.isEmpty ? String(memoView) : head + "\n\n" + String(memoView)
    }

    /// Rust's `str::trim_end`: trailing White_Space scalars removed.
    static func trimEnd(_ text: String) -> String {
        var scalars = Array(text.unicodeScalars)
        while let last = scalars.last, last.properties.isWhitespace { scalars.removeLast() }
        var view = String.UnicodeScalarView()
        view.append(contentsOf: scalars)
        return String(view)
    }

    /// Whether a memo's text is empty for the person (nothing but whitespace).
    public static func isBlank(_ text: String) -> Bool { trimEnd(text).isEmpty }
}

// MARK: - Wire

/// A project's memo as the Mac has it.
public struct ProjectMemo: Sendable, Equatable {
    public let projectID: String
    public let text: String
    /// Unix seconds of the save that last changed it; nil for a memo never saved.
    public let updatedAt: UInt64?
    public init(projectID: String, text: String, updatedAt: UInt64?) { self.projectID = projectID; self.text = text; self.updatedAt = updatedAt }
    public var isEmpty: Bool { ProjectMemoText.isBlank(text) }

    /// The reply to `project.memo.get` or `project.memo.set` about `projectID`.
    public static func parse(_ result: JSONValue, projectID: String) throws -> ProjectMemo {
        guard case .object(let fields) = result, fields["project_id"]?.string == projectID, case .string(let text)? = fields["text"],
              text.utf8.count <= ProjectMemoText.maximumBytes else { throw ProjectMemoError.unreadableReply }
        var updatedAt: UInt64?
        if let raw = fields["updated_at"] {
            guard case .number(let number) = raw, number.isFinite, number >= 1, number <= 9_007_199_254_740_991, number.rounded() == number
            else { throw ProjectMemoError.unreadableReply }
            updatedAt = UInt64(number)
        }
        return ProjectMemo(projectID: projectID, text: text, updatedAt: updatedAt)
    }
}

public enum ProjectMemoRequests {
    public static func get(projectID: String) -> [String: JSONValue] { ["project_id": .string(projectID)] }
    /// Throws for a text over the limit: what is sent is always one the Mac keeps.
    public static func set(projectID: String, text: String) throws -> [String: JSONValue] {
        let text = ProjectMemoText.normalize(text)
        if let message = ProjectMemoText.tooLargeMessage(text) { throw ProjectMemoError.tooLarge(message) }
        return ["project_id": .string(projectID), "text": .string(text)]
    }
    /// The transport's check of every memo request (`RequestValidation`).
    static func validate(method: String, params: [String: JSONValue]) throws {
        guard method == "project.memo.set" else { return }
        guard case .string(let text)? = params["text"], text.utf8.count <= ProjectMemoText.maximumBytes else {
            throw RemoteError.protocolViolation("A memo must be text of at most \(ProjectMemoText.maximumBytes) bytes.")
        }
    }
}

/// What went wrong with reading or saving a memo, in words for the person.
public enum ProjectMemoError: Error, Equatable, Sendable, LocalizedError {
    /// The Mac's RiWork predates memos.
    case unsupported
    case notConnected
    /// The link dropped or timed out: the save may or may not have reached the Mac. Saving the same text again is harmless.
    case connection
    case tooLarge(String)
    case notFound(String)
    case rejected(String)
    case unreadableReply
    case failed(String)

    public static let unsupportedMessage = "Update RiWork on your Mac to use project memos from the phone."

    public var errorDescription: String? { message }
    public var message: String {
        switch self {
        case .unsupported: Self.unsupportedMessage
        case .notConnected: "The Mac isn’t connected."
        case .connection: "The connection to the Mac dropped."
        case .tooLarge(let text): text
        case .notFound(let text): text
        case .rejected(let text): "The Mac refused the memo: \(text)"
        case .unreadableReply: "The Mac’s answer wasn’t understood."
        case .failed(let text): text
        }
    }
    /// The text may simply not have arrived: kept and sent again once the Mac is connected.
    public var isConnectionProblem: Bool { self == .notConnected || self == .connection }

    public static func from(_ error: any Error) -> ProjectMemoError {
        if let known = error as? ProjectMemoError { return known }
        if RemoteError.isUnsupportedMethod(error) { return .unsupported }
        if error is CancellationError || error is URLError { return .connection }
        if let remote = error as? RemoteError {
            switch remote {
            case .rpc(let code, let message):
                switch code {
                case "not_found": return .notFound(TerminalControlError.readable(message))
                case "invalid_request": return .rejected(TerminalControlError.readable(message))
                default: return .failed(TerminalControlError.readable(message))
                }
            case .timeout, .disconnected, .relayClosed, .uncertainDelivery: return .connection
            default: return .failed(TerminalControlError.readable(remote.localizedDescription))
            }
        }
        return .failed(TerminalControlError.readable(error.localizedDescription))
    }
}

extension RemoteTransport {
    public func readProjectMemo(projectID: String, id: String = UUID().uuidString.lowercased()) async throws -> ProjectMemo {
        do { return try ProjectMemo.parse(try await request(method: "project.memo.get", params: ProjectMemoRequests.get(projectID: projectID), id: id), projectID: projectID) }
        catch { throw ProjectMemoError.from(error) }
    }
    /// Replaces the memo with `text`. Safe to send again after a lost answer: the same text saved twice is the same memo.
    public func saveProjectMemo(projectID: String, text: String, id: String = UUID().uuidString.lowercased()) async throws -> ProjectMemo {
        do {
            let params = try ProjectMemoRequests.set(projectID: projectID, text: text)
            return try ProjectMemo.parse(try await request(method: "project.memo.set", params: params, id: id), projectID: projectID)
        } catch { throw ProjectMemoError.from(error) }
    }
}

// MARK: - The editor's saving

/// The memo editor of one project: what it shows, what the Mac has, and what the line under it says about saving. It never claims a
/// save that the Mac has not confirmed.
public struct ProjectMemoEditor: Sendable, Equatable {
    public enum Phase: Sendable, Equatable {
        /// The memo is being read.
        case loading
        /// It could not be read (nothing typed here is at risk: editing waits for a read, unless text from before was kept).
        case loadFailed(String)
        /// What the editor shows is what the Mac has.
        case saved
        /// Typed: a save follows when typing pauses.
        case unsaved
        /// A save is on its way.
        case saving
        /// The Mac could not be reached: kept on this phone and saved once it can be.
        case queued
        /// The Mac refused the save; the text stays here.
        case failed(String)
        /// Longer than a memo may be: nothing is saved until it is shorter.
        case tooLarge(String)
        /// Text kept from earlier, while the Mac's memo changed since it was written: nothing is saved until the person chooses
        /// (`keepMine` or `useMacVersion`), so neither text is lost without being seen.
        case conflict
        /// The Mac's RiWork has no memos.
        case unsupported
    }

    public let projectID: String
    public let projectName: String
    /// This editor, apart from an earlier or later one of the same project: an answer meant for one never lands in another.
    public let session = UUID()
    public private(set) var text: String
    /// The memo as the Mac last said it is; nil until it is read.
    public private(set) var saved: ProjectMemo?
    public private(set) var phase: Phase
    /// Text from an earlier session that never reached the Mac.
    public private(set) var restored: Bool
    /// The `updated_at` of the Mac's memo the kept text was written over (nil: a memo never saved).
    private var restoredBase: UInt64?

    public init(projectID: String, projectName: String, restoredText: String? = nil, restoredBase: UInt64? = nil) {
        self.projectID = projectID; self.projectName = projectName
        text = restoredText ?? ""; restored = restoredText != nil; self.restoredBase = restoredBase
        phase = .loading
    }

    /// What the text on screen was written over: the Mac's memo as last read, or, before a read, the kept text's.
    public var base: UInt64? { saved.map(\.updatedAt) ?? restoredBase }

    /// Whether the text differs from the Mac's (or, before a read, whether there is kept text at all).
    public var hasUnsavedText: Bool {
        guard let saved else { return restored }
        return ProjectMemoText.normalize(text) != saved.text
    }
    /// The editor takes typing once the memo was read, or when it holds kept text.
    public var isEditable: Bool { phase != .unsupported && (saved != nil || restored) }
    /// What to keep on this phone: the text while the Mac does not have it, else nothing.
    public var textToKeep: String? { hasUnsavedText ? text : nil }

    /// The Mac's memo was read. An editor with nothing unsaved shows it; one with unsaved text keeps that text, and its save, the
    /// later one, wins. Kept text from earlier over a memo that changed on the Mac since waits for the person (`conflict`).
    public mutating func loaded(_ memo: ProjectMemo) {
        let unsavedBefore = hasUnsavedText
        let changedSince = restored && saved == nil && memo.updatedAt != restoredBase
        saved = memo
        if case .saving = phase { return }
        if unsavedBefore, hasUnsavedText {
            if case .tooLarge = phase { return }
            if case .conflict = phase { return }
            phase = changedSince ? .conflict : .unsaved
        } else {
            text = memo.text; restored = false; phase = .saved
        }
    }

    /// The person keeps the text on screen over the Mac's newer memo: it is saved (the last save wins).
    public mutating func keepMine() {
        guard case .conflict = phase else { return }
        restored = false
        phase = .unsaved
    }

    /// The person takes the Mac's memo: the kept text goes.
    public mutating func useMacVersion() {
        guard case .conflict = phase, let saved else { return }
        text = saved.text; restored = false; phase = .saved
    }

    public mutating func loadFailed(_ error: ProjectMemoError) {
        if error == .unsupported { phase = .unsupported; return }
        guard saved == nil else { return }
        if !restored {
            phase = .loadFailed(error.message)
        } else {
            phase = error.isConnectionProblem ? .queued : .failed(error.message)
        }
    }

    public mutating func edit(_ text: String) {
        guard isEditable else { return }
        self.text = text
        if case .saving = phase { return }
        if case .conflict = phase { return }
        // Before a read nothing is sent: the phase keeps saying why the text waits.
        guard saved != nil else { return }
        if let message = ProjectMemoText.tooLargeMessage(ProjectMemoText.normalize(text)) {
            phase = .tooLarge(message)
        } else {
            phase = hasUnsavedText ? .unsaved : .saved
        }
    }

    /// The text to send now, if a save is due: the editor holds text the Mac does not have, within the limit, and none is on its way.
    /// A save always follows a read, so that what it replaces is known.
    public var pendingSave: String? {
        guard saved != nil, isEditable, hasUnsavedText, phase != .saving, phase != .conflict else { return nil }
        if case .tooLarge = phase { return nil }
        let text = ProjectMemoText.normalize(self.text)
        return ProjectMemoText.tooLargeMessage(text) == nil ? text : nil
    }

    public mutating func beginSave() { phase = .saving }

    public mutating func saveSucceeded(_ memo: ProjectMemo) {
        saved = memo; restored = false
        if let message = ProjectMemoText.tooLargeMessage(ProjectMemoText.normalize(text)) {
            phase = .tooLarge(message)
        } else {
            phase = hasUnsavedText ? .unsaved : .saved
        }
    }

    public mutating func saveFailed(_ error: ProjectMemoError) {
        switch error {
        case .unsupported: phase = .unsupported
        case .tooLarge(let message): phase = .tooLarge(message)
        case _ where error.isConnectionProblem: phase = .queued
        default: phase = .failed(error.message)
        }
    }

    /// What the line under the editor says.
    public var status: String {
        switch phase {
        case .loading: return restored ? "Reading the memo… Your earlier text is kept on this iPhone." : "Reading the memo…"
        case .loadFailed(let message): return "Couldn’t read the memo: \(message)"
        case .saved: return "Saved automatically"
        case .unsaved: return restored ? "Not saved yet: your text from earlier is kept on this iPhone and saves now." : "Not saved yet: saves when you pause."
        case .saving: return "Saving to your Mac…"
        case .queued: return "Not saved yet: the Mac isn’t connected. Your text is kept on this iPhone and saves when it reconnects."
        case .failed(let message): return "Not saved: \(message) Your text is kept on this iPhone."
        case .tooLarge(let message): return message
        case .conflict: return "Not saved: the memo changed on your Mac since this text was written on this iPhone. Keep yours to replace the Mac’s, or use the Mac’s."
        case .unsupported: return ProjectMemoError.unsupportedMessage
        }
    }
    /// Retry is offered: a refusal, a read that failed, or text that waits for a read.
    public var canRetry: Bool {
        switch phase {
        case .failed, .loadFailed: true
        case .queued: saved == nil
        default: false
        }
    }
    /// The status is a problem the person should see.
    public var statusIsWarning: Bool {
        switch phase {
        case .loadFailed, .queued, .failed, .tooLarge, .conflict, .unsupported: true
        default: false
        }
    }
}

// MARK: - Text kept on the phone

/// Memo text the Mac has not confirmed, per desktop and project, in `UserDefaults`, so a relaunch or a dropped link loses nothing.
@MainActor public final class ProjectMemoDraftStore {
    public static let key = "project.memo.drafts.v1"
    /// Kept at most; the oldest go first.
    public static let maximumDrafts = 64
    /// `base`: the `updated_at` of the Mac's memo the text was written over (nil: one never saved).
    private struct Entry: Codable { var text: String; var base: UInt64?; var updatedAt: Date }
    private let defaults: UserDefaults
    public init(defaults: UserDefaults) { self.defaults = defaults }

    private static func key(desktop: String, project: String) -> String { "\(desktop)|\(project)" }
    private func read() -> [String: Entry] {
        guard let data = defaults.data(forKey: Self.key), let entries = try? JSONDecoder().decode([String: Entry].self, from: data) else { return [:] }
        return entries
    }
    private func write(_ entries: [String: Entry]) {
        if entries.isEmpty { defaults.removeObject(forKey: Self.key); return }
        if let data = try? JSONEncoder().encode(entries) { defaults.set(data, forKey: Self.key) }
    }

    public func text(desktop: String, project: String) -> String? { entry(desktop: desktop, project: project)?.text }

    public func entry(desktop: String, project: String) -> (text: String, base: UInt64?)? {
        read()[Self.key(desktop: desktop, project: project)].map { ($0.text, $0.base) }
    }

    /// Every project of `desktop` with kept text, the oldest first.
    public func keptProjects(desktop: String) -> [String] {
        read().filter { $0.key.hasPrefix(desktop + "|") }.sorted { $0.value.updatedAt < $1.value.updatedAt }
            .map { String($0.key.dropFirst(desktop.count + 1)) }
    }

    /// Keeps `text` written over the Mac's memo of `base`, or forgets the entry for nil.
    public func keep(_ text: String?, base: UInt64? = nil, desktop: String, project: String, now: Date = .now) {
        var entries = read()
        let key = Self.key(desktop: desktop, project: project)
        if let text {
            entries[key] = Entry(text: text, base: base, updatedAt: now)
            if entries.count > Self.maximumDrafts {
                let oldest = entries.sorted { $0.value.updatedAt < $1.value.updatedAt }.prefix(entries.count - Self.maximumDrafts).map(\.key)
                for key in oldest { entries[key] = nil }
            }
        } else {
            guard entries[key] != nil else { return }
            entries[key] = nil
        }
        write(entries)
    }
}

// MARK: - `/memo` in a chat

/// What `/memo` shows over a chat: its project's memo with **Insert into composer** and **Edit**, or why there is none.
public enum ProjectMemoPreview: Sendable, Equatable {
    /// The chat belongs to no project (the global orchestrator).
    case noProject
    case loading(projectID: String)
    case memo(ProjectMemo)
    case failed(projectID: String, message: String)
    case unsupported

    /// The project of the chat `info`, if it has one.
    public static func projectID(of info: ChatInfo?) -> String? { info?.projectID }

    /// Insert is offered for a memo with text only: an empty memo offers Edit instead.
    public var canInsert: Bool { if case .memo(let memo) = self { !memo.isEmpty } else { false } }
    public var canEdit: Bool {
        switch self {
        case .memo, .failed: true
        default: false
        }
    }
    public var projectID: String? {
        switch self {
        case .loading(let id), .failed(let id, _): id
        case .memo(let memo): memo.projectID
        case .noProject, .unsupported: nil
        }
    }
    public var message: String? {
        switch self {
        case .noProject: "This chat isn’t part of a project, so there is no project memo here. Use /memo in a project’s chat."
        case .loading: "Reading the project’s memo…"
        case .memo(let memo) where memo.isEmpty: "This project’s memo is empty. Edit it to write notes you can insert into its chats."
        case .memo: nil
        case .failed(_, let message): "Couldn’t read the memo: \(message)"
        case .unsupported: ProjectMemoError.unsupportedMessage
        }
    }
}
