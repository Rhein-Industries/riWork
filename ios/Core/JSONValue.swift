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
    public var errorDescription: String? {
        switch self {
        case .invalidPairing(let v): "Invalid pairing: \(v)"
        case .protocolViolation(let v): "Connection rejected: \(v)"
        case .disconnected: "Desktop disconnected. Reconnect to refresh output."
        case .timeout: "The desktop did not respond in time."
        case .remote(let v): v
        case .rpc(let code, let message): code == "outcome_unknown" ? "Delivery is uncertain. Review the session output before sending again." : message
        case .uncertainDelivery: "Delivery is uncertain. Check the session output before sending again. This input will not be retried."
        }
    }
}
