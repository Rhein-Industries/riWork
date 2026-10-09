import Foundation

/// How one reply travelled, as `RelayClient` saw it. The link meter works out the network's share from this: the time between sending
/// the request and receiving the whole reply, less what the desktop itself spent (`server_ms`), less one round trip.
public struct ReplyTiming: Sendable, Equatable {
    /// Seconds from handing the request to the socket until the reply arrived whole.
    public var elapsed: Double
    /// What the desktop says it spent on the request (`server_ms`: from decrypting it to having the reply ready), in seconds. Nil for a
    /// desktop that does not report it.
    public var serverSeconds: Double?
    /// The reply as it came over the socket: the text of the encrypted envelope.
    public var wireBytes: Int
    /// The encrypted payload (the compressed or plain JSON, plus its tag).
    public var sealedBytes: Int
    /// The JSON the reply holds, whether or not it travelled compressed.
    public var jsonBytes: Int
    public var compressed: Bool
    /// Bytes of other replies that arrived while this one was outstanding. They shared the socket, so they took part of the time.
    public var concurrentBytes: Int
    public var concurrentReplies: Int
    public init(elapsed: Double, serverSeconds: Double? = nil, wireBytes: Int, sealedBytes: Int? = nil, jsonBytes: Int? = nil, compressed: Bool = false,
                concurrentBytes: Int = 0, concurrentReplies: Int = 0) {
        self.elapsed = elapsed; self.serverSeconds = serverSeconds; self.wireBytes = wireBytes
        self.sealedBytes = sealedBytes ?? wireBytes; self.jsonBytes = jsonBytes ?? sealedBytes ?? wireBytes
        self.compressed = compressed; self.concurrentBytes = concurrentBytes; self.concurrentReplies = concurrentReplies
    }
    /// What the network cost: the time less the desktop's own. Nil without `server_ms`.
    public var networkSeconds: Double? { serverSeconds.map { max(0, elapsed - $0) } }
}

/// A result together with how it travelled (nil when the transport does not measure, which is every test double).
public struct TimedReply: Sendable {
    public let value: JSONValue
    public let timing: ReplyTiming?
    public init(value: JSONValue, timing: ReplyTiming? = nil) { self.value = value; self.timing = timing }
}

/// What a desktop announces in the `features` of its first encrypted frame (`ready`). An older desktop has no `features`, and is
/// then treated as offering none of it.
public struct DesktopFeatures: Sendable, Equatable {
    /// Replies can be deflated if asked for with `link.configure`.
    public var deflate = false
    /// Replies shorter than this are never compressed.
    public var minimumCompressBytes = 2048
    /// The most a compressed reply may inflate to.
    public var maximumInflatedBytes = 0
    /// The most lines the desktop accepts in one `shell.history` page. 1000 until 2026-10-01, and so for a desktop that does not say.
    public var historyMaximumLines = HistoryLimits.legacyMaximumPageLines
    /// The desktop has native chats and answers `chats.list`, `chat.create`, `chat.events`, `chat.command` and `chat.stop` (`features.chat`).
    /// Without it the phone does not offer chats.
    public var chat = false
    public var tabs = false
    /// The desktop takes files from the phone (`features.upload`): `upload.*` and `shell.paste`. Without it the phone says the Mac needs
    /// an update instead of sending one.
    public var upload: UploadFeature?
    /// The desktop answers `orchestrator.create` (`features.orchestrator_create`), which opens the project's orchestrator or the global
    /// one, starting it if it is not there. Without it the phone does not offer to.
    public var orchestratorCreate = false
    /// An agent `shell.create` with `"as_settings": true` starts as the Mac's **Agent terminals run unrestricted** setting says
    /// (`features.shell_create_as_settings`). With it the phone offers no switch of its own for a terminal and sends that; without it, the
    /// phone keeps its switch and sends `unrestricted` only when it is on.
    public var shellCreateAsSettings = false
    /// A chat can go on with the other provider in place (`switch`, `features.chat_provider_switch`).
    public var chatProviderSwitch = false
    /// The desktop answers `chat.models` (`features.chat_models`): a provider's models, read from saved chats.
    public var chatModels = false
    public init() {}
    public init(ready: JSONValue) {
        let features = ready["features"]
        func count(_ value: JSONValue, below limit: Double = 1_000_000_000) -> Int? {
            guard case .number(let number) = value, number.isFinite, number >= 0, number <= limit, number.rounded() == number else { return nil }
            return Int(number)
        }
        if case .object(let deflate) = features["deflate"] {
            self.deflate = true
            minimumCompressBytes = count(.object(deflate)["min_bytes"]) ?? 2048
            maximumInflatedBytes = min(LinkFrame.maximumInflatedBytes, count(.object(deflate)["max_inflated"]) ?? LinkFrame.maximumInflatedBytes)
        }
        if case .bool(true) = features["chat"] { chat = true }
        if case .bool(true) = features["tabs"] { tabs = true }
        upload = UploadFeature(features["upload"])
        if case .bool(true) = features["orchestrator_create"] { orchestratorCreate = true }
        if case .bool(true) = features["shell_create_as_settings"] { shellCreateAsSettings = true }
        if case .bool(true) = features["chat_provider_switch"] { chatProviderSwitch = true }
        if case .bool(true) = features["chat_models"] { chatModels = true }
        if let lines = count(features["history_max_lines"]), lines >= 1 {
            historyMaximumLines = max(HistoryLimits.legacyMaximumPageLines, min(HistoryLimits.maximumPageLines, lines))
        }
    }
}

extension RemoteTransport {
    public func timedRequest(method: String, params: [String: JSONValue], id: String) async throws -> TimedReply {
        TimedReply(value: try await request(method: method, params: params, id: id))
    }
    public func desktopFeatures() async -> DesktopFeatures { DesktopFeatures() }
    public func setCompression(_ enabled: Bool) async {}
    public func compressionActive() async -> Bool { false }
}

extension RemoteTransport {
    /// `keys`, with how the acknowledgement travelled.
    public func timedKeys(shellID: String, batch: String, items: [KeyItem], id: String = UUID().uuidString.lowercased()) async throws -> (status: KeysStatus, timing: ReplyTiming?) {
        try KeyItem.validate(batch: items)
        let params: [String: JSONValue] = ["shell_id": .string(shellID), "batch": .string(batch), "items": .array(items.map(\.json))]
        let reply = try await timedRequest(method: "shell.keys", params: params, id: id)
        return (try KeysStatus.parse(reply.value, shellID: shellID, batch: batch), reply.timing)
    }
}
