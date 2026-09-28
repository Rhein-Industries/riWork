import Foundation
import CryptoKit

public enum Base64URL {
    public static func encode(_ data: Data) -> String { data.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "") }
    public static func decode(_ value: String, bytes: Int? = nil) throws -> Data {
        guard value.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }), value.count % 4 != 1 else { throw RemoteError.protocolViolation("Invalid base64url.") }
        let padded = value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/") + String(repeating: "=", count: (4 - value.count % 4) % 4)
        guard let data = Data(base64Encoded: padded), encode(data) == value, bytes == nil || data.count == bytes else { throw RemoteError.protocolViolation("Invalid encoded length.") }
        return data
    }
}

public struct Pairing: Codable, Sendable, Equatable {
    public let v: Int
    public let relay_url: String
    public let desktop_id: String
    public let device_id: String
    public let route_id: String
    public let device_name: String
    public let pairing_secret: String
    public let relay_token: String

    public static func parse(_ text: String, allowLocalDevelopment: Bool = false) throws -> Pairing {
        let input = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard input.utf8.count <= 16384 else { throw RemoteError.invalidPairing("Code is too large.") }
        let data: Data
        // Any case reaches the link parser so an uppercase scheme fails with a clear message, not a JSON one.
        if input.lowercased().hasPrefix("riwork:") {
            // Reject anything a differential parser could read differently: userinfo, port, percent-encoded host, path or query names.
            guard let components = URLComponents(string: input), components.scheme == "riwork", components.percentEncodedHost == "pair",
                  components.user == nil, components.password == nil, components.port == nil,
                  components.percentEncodedPath == "" || components.percentEncodedPath == "/", components.fragment == nil,
                  let items = components.percentEncodedQueryItems, items.count == 2, !items.contains(where: { $0.name.contains("%") || ($0.value ?? "").contains("%") }),
                  items.filter({ $0.name == "v" }).count == 1, items.first(where: { $0.name == "v" })?.value == "1",
                  items.filter({ $0.name == "data" }).count == 1, let value = items.first(where: { $0.name == "data" })?.value else { throw RemoteError.invalidPairing("Expected riwork://pair?v=1&data=…") }
            data = try Base64URL.decode(value)
        } else { data = Data(input.utf8) }
        let pairing: Pairing
        do { pairing = try JSONDecoder().decode(Pairing.self, from: data) }
        catch { throw RemoteError.invalidPairing("Paste the complete desktop pairing JSON or link.") }
        try pairing.validate(allowLocalDevelopment: allowLocalDevelopment)
        return pairing
    }
    public func validate(allowLocalDevelopment: Bool = false) throws {
        guard v == 1 else { throw RemoteError.invalidPairing("Unsupported version \(v).") }
        for id in [desktop_id, device_id, route_id] { guard UUID(uuidString: id)?.uuidString.lowercased() == id else { throw RemoteError.invalidPairing("IDs must be full lowercase UUIDs.") } }
        _ = try Base64URL.decode(pairing_secret, bytes: 32)
        _ = try Base64URL.decode(relay_token, bytes: 32)
        guard let url = URLComponents(string: relay_url), url.path == "/v1/ws", url.query == nil, url.fragment == nil, url.user == nil, url.password == nil, let host = url.host, !host.isEmpty else { throw RemoteError.invalidPairing("Relay URL must end with /v1/ws and contain no credentials or query.") }
        if url.scheme == "wss" { return }
        guard allowLocalDevelopment, url.scheme == "ws", ["127.0.0.1", "localhost", "[::1]", "::1"].contains(host) else { throw RemoteError.invalidPairing("Use a secure wss:// relay. Local ws:// requires the development switch.") }
    }
    public var relayHost: String { URLComponents(string: relay_url)?.host ?? "Relay" }
    /// The device name is chosen on the desktop (or by whoever made the link), so show it as bounded printable text.
    public var displayDeviceName: String {
        let cleaned = String(device_name.unicodeScalars.filter { $0.value >= 32 && $0.value != 127 && !CharacterSet.controlCharacters.contains($0) }.prefix(80).map { Character($0) }).trimmingCharacters(in: .whitespaces)
        return cleaned.isEmpty ? "Unnamed device" : cleaned
    }
    public var desktopShortID: String { String(desktop_id.prefix(8)) }
    public var usesLocalDevelopmentRelay: Bool { URLComponents(string: relay_url)?.scheme == "ws" }
}

public struct PendingInput: Codable, Sendable, Equatable {
    public let id: String
    public let shellID: String
    public let line: String
    public let createdAt: Date
    public init(shellID: String, line: String, id: String = UUID().uuidString.lowercased()) throws {
        try InputValidation.validate(line)
        self.id = id; self.shellID = shellID; self.line = line; self.createdAt = Date()
    }
}

public enum InputValidation {
    /// Control characters and line/paragraph separators can never be part of a single submitted line.
    public static func isForbidden(_ scalar: Unicode.Scalar) -> Bool { CharacterSet.controlCharacters.contains(scalar) || scalar.value == 0x2028 || scalar.value == 0x2029 }
    public static func validate(_ line: String) throws {
        guard !line.trimmingCharacters(in: .whitespaces).isEmpty else { throw RemoteError.remote("Enter a continuation prompt or command.") }
        guard line.utf8.count <= 8192, !line.unicodeScalars.contains(where: isForbidden) else { throw RemoteError.remote("Input must be one line, at most 8192 UTF-8 bytes, with no control characters.") }
    }
}

public struct SavedDesktop: Codable, Sendable, Identifiable {
    public var id: String { pairing.route_id }
    public var name: String
    public let pairing: Pairing
    public let allowLocalDevelopment: Bool
    public var selectedProjectID: String?
    public var selectedSessionID: String?
    public var projectSessionIDs: [String: String]?
    public var pendingInput: PendingInput?
    public init(name: String, pairing: Pairing, allowLocalDevelopment: Bool) {
        self.name = name; self.pairing = pairing; self.allowLocalDevelopment = allowLocalDevelopment
    }
}
