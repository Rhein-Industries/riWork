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
    /// v1 long-term PSK. Empty on v2, which uses a single-use invite and then a root key.
    public let pairing_secret: String
    public let relay_token: String
    public let invite_id: String?
    public let invite_secret: String?
    public let expires_at: UInt64?
    public let root_key: String?
    /// `pending`, `established` or `expired`. Absent on v1 records.
    public let invite_state: String?

    public init(v: Int, relay_url: String, desktop_id: String, device_id: String, route_id: String, device_name: String, pairing_secret: String, relay_token: String, invite_id: String? = nil, invite_secret: String? = nil, expires_at: UInt64? = nil, root_key: String? = nil, invite_state: String? = nil) {
        self.v = v
        self.relay_url = relay_url
        self.desktop_id = desktop_id
        self.device_id = device_id
        self.route_id = route_id
        self.device_name = device_name
        self.pairing_secret = pairing_secret
        self.relay_token = relay_token
        self.invite_id = invite_id
        self.invite_secret = invite_secret
        self.expires_at = expires_at
        self.root_key = root_key
        self.invite_state = invite_state
    }
    private enum CodingKeys: String, CodingKey {
        case v, relay_url, desktop_id, device_id, route_id, device_name, pairing_secret, relay_token
        case invite_id, invite_secret, expires_at, root_key, invite_state
    }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        v = try c.decode(Int.self, forKey: .v)
        relay_url = try c.decode(String.self, forKey: .relay_url)
        desktop_id = try c.decode(String.self, forKey: .desktop_id)
        device_id = try c.decode(String.self, forKey: .device_id)
        route_id = try c.decode(String.self, forKey: .route_id)
        device_name = try c.decode(String.self, forKey: .device_name)
        pairing_secret = try c.decodeIfPresent(String.self, forKey: .pairing_secret) ?? ""
        relay_token = try c.decode(String.self, forKey: .relay_token)
        invite_id = try c.decodeIfPresent(String.self, forKey: .invite_id)
        invite_secret = try c.decodeIfPresent(String.self, forKey: .invite_secret)
        expires_at = try c.decodeIfPresent(UInt64.self, forKey: .expires_at)
        root_key = try c.decodeIfPresent(String.self, forKey: .root_key)
        invite_state = try c.decodeIfPresent(String.self, forKey: .invite_state)
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(v, forKey: .v)
        try c.encode(relay_url, forKey: .relay_url)
        try c.encode(desktop_id, forKey: .desktop_id)
        try c.encode(device_id, forKey: .device_id)
        try c.encode(route_id, forKey: .route_id)
        try c.encode(device_name, forKey: .device_name)
        if !pairing_secret.isEmpty { try c.encode(pairing_secret, forKey: .pairing_secret) }
        try c.encode(relay_token, forKey: .relay_token)
        try c.encodeIfPresent(invite_id, forKey: .invite_id)
        try c.encodeIfPresent(invite_secret, forKey: .invite_secret)
        try c.encodeIfPresent(expires_at, forKey: .expires_at)
        try c.encodeIfPresent(root_key, forKey: .root_key)
        try c.encodeIfPresent(invite_state, forKey: .invite_state)
    }
    /// Replaces a redeemed invite with the root key. The invite secret is not copied.
    public func established(rootKey: String) -> Pairing {
        Pairing(v: v, relay_url: relay_url, desktop_id: desktop_id, device_id: device_id, route_id: route_id, device_name: device_name, pairing_secret: "", relay_token: relay_token, invite_id: invite_id, invite_secret: nil, expires_at: expires_at, root_key: rootKey, invite_state: "established")
    }
    /// 48 raw UUID bytes in displayed order: desktop, device, then route.
    public func identityBytes() throws -> Data {
        var bytes = Data()
        for id in [desktop_id, device_id, route_id] {
            guard let uuid = UUID(uuidString: id) else { throw RemoteError.protocolViolation("Invalid identity.") }
            var raw = uuid.uuid
            bytes.append(withUnsafeBytes(of: &raw) { Data($0) })
        }
        guard bytes.count == 48 else { throw RemoteError.protocolViolation("Invalid identity.") }
        return bytes
    }

    public static func parse(_ text: String, allowLocalDevelopment: Bool = false) throws -> Pairing {
        let input = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard input.utf8.count <= 16384 else { throw RemoteError.invalidPairing("Code is too large.") }
        let data: Data
        var linkVersion: Int?
        // Any case reaches the link parser so an uppercase scheme fails with a clear message, not a JSON one.
        if input.lowercased().hasPrefix("riwork:") {
            // Reject anything a differential parser could read differently: userinfo, port, percent-encoded host, path or query names.
            guard let components = URLComponents(string: input), components.scheme == "riwork", components.percentEncodedHost == "pair",
                  components.user == nil, components.password == nil, components.port == nil,
                  components.percentEncodedPath == "" || components.percentEncodedPath == "/", components.fragment == nil,
                  let items = components.percentEncodedQueryItems, items.count == 2, !items.contains(where: { $0.name.contains("%") || ($0.value ?? "").contains("%") }),
                  items.filter({ $0.name == "v" }).count == 1, let version = items.first(where: { $0.name == "v" })?.value, version == "1" || version == "2",
                  items.filter({ $0.name == "data" }).count == 1, let value = items.first(where: { $0.name == "data" })?.value else { throw RemoteError.invalidPairing("Expected riwork://pair?v=1&data=… or v=2.") }
            linkVersion = Int(version)
            data = try Base64URL.decode(value)
        } else { data = Data(input.utf8) }
        let pairing: Pairing
        do { pairing = try JSONDecoder().decode(Pairing.self, from: data) }
        catch { throw RemoteError.invalidPairing("Paste the complete desktop pairing JSON or link.") }
        if let linkVersion, pairing.v != linkVersion { throw RemoteError.invalidPairing("Link version does not match the pairing.") }
        try pairing.validate(allowLocalDevelopment: allowLocalDevelopment)
        return pairing
    }
    public func validate(allowLocalDevelopment: Bool = false) throws {
        guard v == 1 || v == 2 else { throw RemoteError.invalidPairing("Unsupported version \(v).") }
        for id in [desktop_id, device_id, route_id] { guard UUID(uuidString: id)?.uuidString.lowercased() == id else { throw RemoteError.invalidPairing("IDs must be full lowercase UUIDs.") } }
        _ = try Base64URL.decode(relay_token, bytes: 32)
        if v == 1 {
            guard invite_id == nil, invite_secret == nil, expires_at == nil, root_key == nil, invite_state == nil else { throw RemoteError.invalidPairing("Version 1 pairing cannot carry invite fields.") }
            _ = try Base64URL.decode(pairing_secret, bytes: 32)
        } else {
            guard pairing_secret.isEmpty else { throw RemoteError.invalidPairing("Version 2 pairing has no long-term secret.") }
            guard let invite_id, UUID(uuidString: invite_id)?.uuidString.lowercased() == invite_id else { throw RemoteError.invalidPairing("Invite ID must be a full lowercase UUID.") }
            guard let expires_at, expires_at > 0 else { throw RemoteError.invalidPairing("Version 2 invite needs an expiry.") }
            switch invite_state {
            case "pending":
                guard root_key == nil, let invite_secret else { throw RemoteError.invalidPairing("A pending invite needs its secret and no root key.") }
                _ = try Base64URL.decode(invite_secret, bytes: 32)
            case "established":
                guard invite_secret == nil, let root_key else { throw RemoteError.invalidPairing("An established device needs a root key and no invite secret.") }
                _ = try Base64URL.decode(root_key, bytes: 32)
            case "expired":
                guard invite_secret == nil, root_key == nil else { throw RemoteError.invalidPairing("An expired invite must not keep key material.") }
            default:
                throw RemoteError.invalidPairing("Invalid invite state.")
            }
        }
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
    public var pairing: Pairing
    public let allowLocalDevelopment: Bool
    public var selectedProjectID: String?
    public var selectedSessionID: String?
    public var projectSessionIDs: [String: String]?
    /// The shared tab (`chat:<id>` or `shell:<id>`) last on screen in each project, by project id: entering the project, relaunching
    /// and reconnecting put it back while the desktop still lists it.
    public var projectTabKeys: [String: String]?
    public var pendingInput: PendingInput?
    public init(name: String, pairing: Pairing, allowLocalDevelopment: Bool) {
        self.name = name; self.pairing = pairing; self.allowLocalDevelopment = allowLocalDevelopment
    }
}
