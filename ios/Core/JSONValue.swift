import Foundation

public enum JSONValue: Codable, Sendable, Equatable {
    case object([String: JSONValue]), array([JSONValue]), string(String), number(Double), bool(Bool), null

    public init(from decoder: any Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let v = try? c.decode(Bool.self) { self = .bool(v) }
        else if let v = try? c.decode(String.self) { self = .string(v) }
        else if let v = try? c.decode(Double.self) { self = .number(v) }
        else if let v = try? c.decode([String: JSONValue].self) { self = .object(v) }
        else { self = .array(try c.decode([JSONValue].self)) }
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .object(let v): try c.encode(v)
        case .array(let v): try c.encode(v)
        case .string(let v): try c.encode(v)
        case .number(let v): try c.encode(v)
        case .bool(let v): try c.encode(v)
        case .null: try c.encodeNil()
        }
    }
    public subscript(_ key: String) -> JSONValue { if case .object(let v) = self { v[key] ?? .null } else { .null } }
    public var string: String? { if case .string(let v) = self { v } else { nil } }
    public var array: [JSONValue] { if case .array(let v) = self { v } else { [] } }
    public func decode<T: Decodable>(_ type: T.Type) throws -> T { try JSONDecoder().decode(type, from: JSONEncoder().encode(self)) }
}

public enum RemoteError: Error, LocalizedError, Sendable {
    case invalidPairing(String), protocolViolation(String), disconnected, timeout, remote(String), rpc(code: String, message: String), uncertainDelivery
    /// The relay closed the WebSocket. `code` is the RFC 6455 close code; `reason` is relay-supplied text.
    case relayClosed(code: Int, reason: String?)
    public var errorDescription: String? {
        switch self {
        case .invalidPairing(let v): "Invalid pairing: \(v)"
        case .protocolViolation(let v): "Connection rejected: \(v)"
        case .disconnected: "Desktop disconnected. Reconnect to refresh output."
        case .timeout: "The desktop did not respond in time."
        case .remote(let v): v
        case .rpc(let code, let message): code == "outcome_unknown" ? "Delivery is uncertain. Review the session output before sending again." : message
        case .uncertainDelivery: "Delivery is uncertain. Check the session output before sending again. This input will not be retried."
        case .relayClosed(let code, let reason): Self.describeClose(code: code, reason: reason)
        }
    }
    static func describeClose(code: Int, reason: String?) -> String {
        let base: String = switch code {
        case 1000: "The relay closed the connection."
        case 1001: "The relay is restarting or shutting down. Try again in a moment."
        case 1002, 1003, 1007: "The relay rejected a malformed frame (code \(code))."
        // The relay closes without a status for a bad token, a duplicate device socket, an offline peer or a full queue.
        case 1005: "The relay closed the connection. This device may no longer be authorized, another connection for it may already be open, or the relay could not deliver a message."
        case 1006: "The connection to the relay dropped unexpectedly."
        case 1008: "The relay rejected this device. Check that its pairing is still authorized."
        case 1009: "The relay refused an oversized message."
        case 1011: "The relay hit an internal error. Try again shortly."
        case 1012, 1013: "The relay is restarting or busy. Try again shortly."
        case 1015: "The secure connection to the relay failed."
        default: "The relay closed the connection (code \(code))."
        }
        // Relay text is untrusted: show it as bounded, printable diagnostics only.
        let detail = reason?.unicodeScalars.filter { $0.value >= 32 && $0.value != 127 }.prefix(120).map(String.init).joined().trimmingCharacters(in: .whitespaces)
        guard let detail, !detail.isEmpty else { return base }
        return "\(base) Relay said: “\(detail)”"
    }
}
