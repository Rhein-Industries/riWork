import Foundation
import RiWorkCore

/// Whether the desktop understands `shell.keys`, learned from the first call.
enum KeysSupport: Equatable { case unknown, supported, unsupported }
enum KeyAcceptance: Equatable { case accepted, bufferFull, unavailable }

struct KeyBufferKey: Hashable {
    let desktopID: String
    let shellID: String
}

/// The small "pending input" chip: what has not reached the shell yet, and why.
struct KeyPreview: Equatable {
    enum Tone: Equatable { case sending, offline, full, blocked }
    let text: String
    let label: String
    let tone: Tone
}

// Direct typing: keystrokes go to the shell as they are typed. Only when the connection cannot take them right now
// are they held in a small ordered buffer, shown as a chip, and sent in order once it can.
extension RemoteModel {
    static let previewCharacters = 60
    static let blockRetryAfter: TimeInterval = 1

    /// Keys go straight to the shell unless the user chose the line composer or the desktop is too old.
    var directTyping: Bool { !preferLineComposer && keysSupport != .unsupported }
    var typingKey: KeyBufferKey? {
        guard let desktopID = selectedDesktopID, let shellID = sessionID else { return nil }
        return KeyBufferKey(desktopID: desktopID, shellID: shellID)
    }
    var pendingKeyBuffer: KeyBuffer? { typingKey.flatMap { keyBuffers[$0] } }

    /// Non-nil only when something is worth showing: input older than the preview delay, or the connection is down,
    /// or the buffer is full or refused. A shell that echoes quickly never shows it.
    var keyPreview: KeyPreview? {
        _ = keyRevealTick
        guard directTyping, let key = typingKey, let buffer = keyBuffers[key], !buffer.isEmpty else { return nil }
        let offline = state != .connected
        let full = keysFull.contains(key)
        let old = buffer.oldestPendingAt.map { Date().timeIntervalSince($0) >= previewDelay.timeInterval } ?? false
        guard offline || full || buffer.block != nil || old else { return nil }
        let text = buffer.preview(limit: Self.previewCharacters)
        if let block = buffer.block { return KeyPreview(text: text, label: block.reason, tone: .blocked) }
        if full { return KeyPreview(text: text, label: offline ? "Buffer full · Offline" : "Buffer full", tone: .full) }
        if offline { return KeyPreview(text: text, label: "Offline — will send when reconnected", tone: .offline) }
        return KeyPreview(text: text, label: "Sending…", tone: .sending)
    }

    // MARK: Preference

    func setPreferLineComposer(_ value: Bool) {
        preferLineComposer = value
        defaults.set(value, forKey: Self.lineComposerKey)
    }

    // MARK: Layout, font size, focus

    var focusMode: Bool { sessionID.map { focusedSessionIDs.contains($0) } ?? false }
    var terminalLayout: TerminalLayout { focusMode ? .focus : .normal }

    /// The terminal pane changed size (rotation, keyboard, focus mode, safe area). The grid follows.
    func reportTerminalArea(_ size: CGSize) {
        terminalArea = size
        recomputeViewport()
    }
    /// Columns and rows for the current area, layout and font. Requests to the desktop are debounced by `reportViewport`.
    func recomputeViewport() {
        guard let area = terminalArea else { return }
        let cell = cellMetrics(terminalFontSize)
        reportViewport(terminalLayout.viewport(width: area.width, height: area.height, cellWidth: cell.width, lineHeight: cell.height))
    }
    func setTerminalFontSize(_ size: Double) {
        let clamped = TerminalFontSize.clamped(size)
        guard clamped != terminalFontSize else { return }
        terminalFontSize = clamped
        defaults.set(clamped, forKey: Self.fontSizeKey)
        recomputeViewport()
    }
    /// The screen stays awake only while a focused terminal is on screen.
    func updateKeepAwake() { keepAwake(terminalVisible && focusMode) }
    func stepTerminalFontSize(_ steps: Int) { setTerminalFontSize(TerminalFontSize.stepped(terminalFontSize, by: steps)) }
    func setFocusMode(_ on: Bool) {
        guard let id = sessionID, focusedSessionIDs.contains(id) != on else { return }
        if on { focusedSessionIDs.insert(id) } else { focusedSessionIDs.remove(id) }
        updateKeepAwake()
        recomputeViewport()
    }

    // MARK: Typing

    /// Queues keystrokes for the selected shell. They are sent at once when the connection is up and idle.
    @discardableResult
    func type(_ items: [KeyItem]) -> KeyAcceptance {
        guard directTyping, let key = typingKey, let session, session.alive, !missingSessionIDs.contains(session.id) else { return .unavailable }
        // Whatever the caller hands over, only contract-valid items are queued: literal text loses control characters
        // (newlines become Enter), unknown keys are dropped.
        let items = items.flatMap { item -> [KeyItem] in
            switch item {
            case .text(let text): KeyMapper.items(for: text)
            case .key(let key): key.isValid ? [item] : []
            }
        }
        guard !items.isEmpty else { return .accepted }
        var buffer = keyBuffers[key] ?? KeyBuffer()
        let now = Date()
        // A refused buffer gets another try once a moment has passed: the desktop may have been fixed meanwhile.
        if let block = buffer.block, now.timeIntervalSince(block.at) >= Self.blockRetryAfter { buffer.block = nil }
        guard buffer.append(items, now: now) else {
            keysFull.insert(key)
            keyRevealTick &+= 1
            return .bufferFull
        }
        keysFull.remove(key)
        keyBuffers[key] = buffer
        typedCount &+= 1
        noteKeyActivity(now: now)
        scheduleReveal()
        kickKeySender()
        scheduleReconnectIfNeeded()
        return .accepted
    }
    /// Throws away what has not been delivered for the selected shell (the chip's dismiss button).
    func discardPendingKeys() {
        guard let key = typingKey else { return }
        keyBuffers[key] = nil
        keysFull.remove(key)
    }
    func discardKeyBuffers(exceptDesktop desktopID: String) {
        for key in keyBuffers.keys where key.desktopID != desktopID { keyBuffers[key] = nil; keysFull.remove(key) }
    }
    func discardKeyBuffers(forDesktop desktopID: String) {
        for key in keyBuffers.keys where key.desktopID == desktopID { keyBuffers[key] = nil; keysFull.remove(key) }
    }
    func hasPendingKeys(forDesktop desktopID: String?) -> Bool {
        guard let desktopID else { return false }
        return keyBuffers.contains { $0.key.desktopID == desktopID && !$0.value.isEmpty && $0.value.block == nil }
    }

    func scheduleReveal() {
        guard revealTask == nil else { return }
        // Ticks while anything is still pending, so input that lingers past the delay shows its chip without another keystroke.
        revealTask = Task { [weak self, delay = previewDelay] in
            try? await Task.sleep(for: delay)
            guard let self, !Task.isCancelled else { return }
            self.revealTask = nil
            self.keyRevealTick &+= 1
            // Only while connected: offline the chip is already shown, and a reconnect re-evaluates it.
            if self.state == .connected, self.keyBuffers.values.contains(where: { !$0.isEmpty && $0.block == nil }) { self.scheduleReveal() }
        }
    }

    // MARK: Sending

    /// The buffer to send next: the oldest pending input of this desktop that the desktop has not refused.
    private func nextSendableKey() -> KeyBufferKey? {
        guard let desktopID = selectedDesktopID else { return nil }
        return keyBuffers.filter { $0.key.desktopID == desktopID && !$0.value.isEmpty && $0.value.block == nil }
            .min { ($0.value.oldestPendingAt ?? .distantFuture) < ($1.value.oldestPendingAt ?? .distantFuture) }?.key
    }
    private var hasSendableKeys: Bool { nextSendableKey() != nil }

    func kickKeySender() {
        guard keySender == nil, state == .connected, keysSupport != .unsupported, hasSendableKeys else { return }
        let token = generation
        let id = UUID()
        keySenderID = id
        keySender = Task { [weak self] in await self?.runKeySender(token: token, id: id) }
    }

    /// Exactly one batch in flight. A batch is formed from the head of a buffer, keeps its UUID until it succeeds, and is
    /// retried unchanged (new request id, same batch UUID) after a lost connection. The desktop drops the repeat.
    private func runKeySender(token: UUID, id: UUID) async {
        defer { if keySenderID == id { keySender = nil } }
        var failures = 0
        while !Task.isCancelled, generation == token, state == .connected, keysSupport != .unsupported {
            guard let key = nextSendableKey() else { return }
            // Idle: send at once. Busy: keys typed meanwhile ride together in the next batch, at most one per interval.
            if let last = lastKeyBatchStart {
                let wait = keyFlushInterval - (ContinuousClock.now - last)
                if wait > .zero { try? await Task.sleep(for: wait); continue }
            }
            guard var buffer = keyBuffers[key], let batch = buffer.nextBatch() else { return }
            keyBuffers[key] = buffer
            lastKeyBatchStart = .now
            noteKeyActivity()
            do {
                // Echo latency starts when the batch leaves; the pending long poll returns on the echo by itself.
                let sentAt = ProcessInfo.processInfo.systemUptime
                latency.keysSent(at: sentAt)
                let status = try await client.keys(shellID: key.shellID, batch: batch.id, items: batch.items)
                latency.keysAnswered(seconds: ProcessInfo.processInfo.systemUptime - sentAt)
                keysSupport = .supported
                failures = 0
                finishBatch(batch.id, for: key)
                if status == .uncertain { announce("Some input may not have arrived.") }
            } catch is CancellationError {
                return
            } catch {
                // Cancelled or superseded by a new connection: the frozen batch is simply sent again by whoever runs next.
                if Task.isCancelled || generation != token { return }
                if RemoteError.isUnsupportedMethod(error) { keysUnsupported(); return }
                if case RemoteError.rpc(let code, let message) = error {
                    switch code {
                    case "outcome_unknown", "request_conflict":
                        finishBatch(batch.id, for: key); announce("Some input may not have arrived.")
                    case "invalid_request":
                        finishBatch(batch.id, for: key); announce("The desktop rejected some input.")
                    case "input_unavailable": hold(key, "Input is disabled for this terminal.")
                    case "not_found": hold(key, "This terminal is no longer available.")
                    default: hold(key, message)
                    }
                } else if await client.isConnected() {
                    if case RemoteError.protocolViolation = error {
                        // The answer was unusable: what happened is unknown, so the batch is not repeated.
                        finishBatch(batch.id, for: key); announce("Some input may not have arrived.")
                    } else {
                        // A link that died and was replaced under us: the same batch goes out again, a few times at most.
                        failures += 1
                        if failures >= 5 { hold(key, error.localizedDescription) }
                        else { try? await Task.sleep(for: .milliseconds(250)) }
                    }
                } else {
                    // Connection lost, timed out or closed. The batch stays frozen at the head and goes out again, same UUID, after reconnect.
                    guard generation == token else { return }
                    if state == .connected { state = .failed; snapshotStale = true; polling?.cancel(); pollSleeper?.cancel(); self.error = error.localizedDescription }
                    scheduleReconnectIfNeeded()
                    return
                }
            }
        }
    }
    private func finishBatch(_ batchID: String, for key: KeyBufferKey) {
        guard var buffer = keyBuffers[key], buffer.finish(batchID) else { return }
        keysFull.remove(key)
        keyBuffers[key] = buffer.isEmpty ? nil : buffer
        noteKeyActivity()
        if !buffer.isEmpty { scheduleReveal() }
    }
    private func hold(_ key: KeyBufferKey, _ reason: String) {
        keyBuffers[key]?.block = KeyBuffer.Block(reason: reason, at: Date())
        keyRevealTick &+= 1
    }
    /// The desktop predates `shell.keys`: what was typed goes back to the line composer, which takes over.
    private func keysUnsupported() {
        keysSupport = .unsupported
        if let key = typingKey, let text = keyBuffers[key]?.plainText, !text.isEmpty { draft += text }
        if let desktopID = selectedDesktopID { discardKeyBuffers(forDesktop: desktopID) }
        announce("This desktop cannot take direct typing yet. Using the line composer.")
    }
    /// A short note that fades on its own, so it never blocks typing.
    func announce(_ text: String) {
        deliveryNotice = text
        let id = UUID()
        noticeID = id
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(6))
            guard let self, self.noticeID == id, self.deliveryNotice == text else { return }
            self.deliveryNotice = nil
        }
    }

    /// Lets queued keys reach the desktop before the app leaves the foreground.
    func drainKeys(timeout: Duration) async {
        let end = ContinuousClock.now + timeout
        while ContinuousClock.now < end, hasSendableKeys, await client.isConnected() {
            kickKeySender()
            try? await Task.sleep(for: .milliseconds(10))
        }
    }

    // MARK: Reconnect

    /// 1×, 2×, 4×, 8×, then 15× the base, for every attempt after (the shift is capped: it must not overflow into 0).
    static func reconnectDelay(base: Duration, attempt: Int) -> Duration { base * min(1 << min(max(attempt, 0), 4), 15) }

    /// While input is waiting, a dropped connection is re-established on its own (1 s, 2 s, 4 s … up to 15 s).
    func scheduleReconnectIfNeeded() {
        guard reconnectTask == nil, wantsConnection, state == .failed, hasPendingKeys(forDesktop: selectedDesktopID) else { return }
        let id = UUID()
        reconnectID = id
        let base = reconnectBackoff
        reconnectTask = Task { [weak self] in
            var attempt = 0
            while !Task.isCancelled {
                try? await Task.sleep(for: Self.reconnectDelay(base: base, attempt: attempt))
                guard !Task.isCancelled, let self, self.reconnectID == id, self.wantsConnection, self.hasPendingKeys(forDesktop: self.selectedDesktopID) else { break }
                if self.state == .failed { await self.connect() }
                if self.state == .connected || self.state == .connecting { break }
                attempt += 1
            }
            if let self, self.reconnectID == id { self.reconnectTask = nil }
        }
    }
}
