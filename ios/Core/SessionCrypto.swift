import Foundation
import CryptoKit
import Security

public struct ClientHandshake: Sendable {
    public let pairing: Pairing
    public let clientNonce: Data
    public let identity: Data
    private let psk: SymmetricKey
    public init(pairing: Pairing, nonce: Data? = nil) throws {
        self.pairing = pairing
        psk = SymmetricKey(data: try Base64URL.decode(pairing.pairing_secret, bytes: 32))
        identity = try pairing.identityBytes()
        if let nonce { guard nonce.count == 32 else { throw RemoteError.protocolViolation("Invalid nonce.") }; clientNonce = nonce }
        else {
            var random = [UInt8](repeating: 0, count: 32)
            guard SecRandomCopyBytes(kSecRandomDefault, random.count, &random) == errSecSuccess else { throw RemoteError.protocolViolation("Secure randomness unavailable.") }
            clientNonce = Data(random)
        }
    }
    public var hello: JSONValue {
        .object(["v": .number(1), "type": .string("client_hello"), "desktop_id": .string(pairing.desktop_id), "device_id": .string(pairing.device_id), "route_id": .string(pairing.route_id), "client_nonce": .string(Base64URL.encode(clientNonce)), "mac": .string(mac(Data("riwork/v1/client-hello\0".utf8) + identity + clientNonce))])
    }
    public func accept(_ serverHello: JSONValue) throws -> (finish: JSONValue, cipher: SessionCipher) {
        guard serverHello["v"] == .number(1), serverHello["type"].string == "server_hello", let nonce = serverHello["desktop_nonce"].string, let proof = serverHello["mac"].string else { throw RemoteError.protocolViolation("Expected authenticated server hello.") }
        let desktopNonce = try Base64URL.decode(nonce, bytes: 32)
        let transcript = Data("riwork/v1/session\0".utf8) + identity + clientNonce + desktopNonce
        let proofBytes = try Base64URL.decode(proof, bytes: 32)
        guard HMAC<SHA256>.isValidAuthenticationCode(proofBytes, authenticating: Data("riwork/v1/server-hello\0".utf8) + transcript, using: psk) else { throw RemoteError.protocolViolation("Desktop authentication failed. Pair again with the correct desktop.") }
        let finish = JSONValue.object(["v": .number(1), "type": .string("client_finish"), "mac": .string(mac(Data("riwork/v1/client-finish\0".utf8) + transcript))])
        return (finish, SessionCipher(psk: psk, transcript: transcript))
    }
    public func mac(_ message: Data) -> String { Base64URL.encode(Data(HMAC<SHA256>.authenticationCode(for: message, using: psk))) }
}

public struct SessionCipher: Sendable {
    public let version: Int
    public let sessionID: Data
    public let c2d: SymmetricKey
    public let d2c: SymmetricKey
    public private(set) var sendCounter: UInt64
    public private(set) var receiveCounter: UInt64
    public init(psk: SymmetricKey, transcript: Data) { self.init(psk: psk, transcript: transcript, sendCounter: 0, receiveCounter: 0) }
    /// Tests start mid-stream to reach large counters without sealing millions of frames.
    init(psk: SymmetricKey, transcript: Data, sendCounter: UInt64, receiveCounter: UInt64) {
        let salt = Data(SHA256.hash(data: transcript))
        let c2d = HKDF<SHA256>.deriveKey(inputKeyMaterial: psk, salt: salt, info: Data("riwork/v1/c2d".utf8), outputByteCount: 32)
        let d2c = HKDF<SHA256>.deriveKey(inputKeyMaterial: psk, salt: salt, info: Data("riwork/v1/d2c".utf8), outputByteCount: 32)
        self.init(version: 1, sessionID: salt.prefix(16), c2d: c2d, d2c: d2c, sendCounter: sendCounter, receiveCounter: receiveCounter)
    }
    public init(version: Int, sessionID: Data, c2d: SymmetricKey, d2c: SymmetricKey, sendCounter: UInt64 = 0, receiveCounter: UInt64 = 0) {
        self.version = version
        self.sessionID = sessionID
        self.c2d = c2d
        self.d2c = d2c
        self.sendCounter = sendCounter
        self.receiveCounter = receiveCounter
    }
    public static func counterBytes(_ counter: UInt64) -> Data { var big = counter.bigEndian; return withUnsafeBytes(of: &big) { Data($0) } }
    public func aad(direction: String, counter: UInt64) -> Data {
        let label = version == 2 ? "riwork/v2/frame\0" : "riwork/v1/frame\0"
        return Data(label.utf8) + sessionID + Data([direction == "c2d" ? 0 : 1]) + Self.counterBytes(counter)
    }
    public mutating func seal(_ payload: JSONValue) throws -> JSONValue {
        try sealJSON(JSONEncoder().encode(payload))
    }
    public mutating func sealJSON(_ data: Data) throws -> JSONValue {
        guard sendCounter < UInt64.max else { throw RemoteError.protocolViolation("Counter exhausted; reconnect.") }
        guard data.count <= 131072 else { throw RemoteError.protocolViolation("Request too large.") }
        guard try JSONDecoder().decode(JSONValue.self, from: data)["v"] == .number(1) else { throw RemoteError.protocolViolation("Unsupported request version.") }
        let nonce = try ChaChaPoly.Nonce(data: Data(repeating: 0, count: 4) + Self.counterBytes(sendCounter))
        let box = try ChaChaPoly.seal(data, using: c2d, nonce: nonce, authenticating: aad(direction: "c2d", counter: sendCounter))
        let envelope = JSONValue.object(["v": .number(Double(version)), "type": .string("encrypted"), "session_id": .string(Base64URL.encode(sessionID)), "direction": .string("c2d"), "counter": .string(String(sendCounter)), "ciphertext": .string(Base64URL.encode(box.ciphertext + box.tag))])
        sendCounter += 1
        return envelope
    }
    public mutating func open(_ frame: JSONValue) throws -> JSONValue { try openFrame(frame).payload }
    /// What a received frame held: the payload, and how big it was in its forms.
    public struct OpenedFrame: Sendable {
        public let payload: JSONValue
        /// The encrypted payload with its tag.
        public let sealedBytes: Int
        /// The JSON text, whether or not it was compressed.
        public let jsonBytes: Int
        public let compressed: Bool
    }
    public mutating func openFrame(_ frame: JSONValue) throws -> OpenedFrame {
        guard receiveCounter < UInt64.max, frame["v"] == .number(Double(version)), frame["type"].string == "encrypted", frame["session_id"].string == Base64URL.encode(sessionID), frame["direction"].string == "d2c", frame["counter"].string == String(receiveCounter), let encoded = frame["ciphertext"].string else { throw RemoteError.protocolViolation("Stale, replayed or out-of-order frame.") }
        let bytes = try Base64URL.decode(encoded)
        guard bytes.count >= 16, bytes.count <= 131088 else { throw RemoteError.protocolViolation("Invalid frame size.") }
        let nonce = try ChaChaPoly.Nonce(data: Data(repeating: 0, count: 4) + Self.counterBytes(receiveCounter))
        let box = try ChaChaPoly.SealedBox(nonce: nonce, ciphertext: bytes.dropLast(16), tag: bytes.suffix(16))
        let plaintext: Data
        do { plaintext = try ChaChaPoly.open(box, using: d2c, authenticating: aad(direction: "d2c", counter: receiveCounter)) }
        catch { throw RemoteError.protocolViolation("Encrypted frame authentication failed.") }
        // A plain JSON text, or the desktop's deflated form of one (`LinkFrame`).
        let (json, compressed) = try LinkFrame.decode(plaintext)
        let payload = try JSONDecoder().decode(JSONValue.self, from: json)
        guard payload["v"] == .number(1) else { throw RemoteError.protocolViolation("Unsupported payload version.") }
        receiveCounter += 1
        return OpenedFrame(payload: payload, sealedBytes: bytes.count, jsonBytes: json.count, compressed: compressed)
    }
}

enum V2MAC {
    static func code(_ key: SymmetricKey, _ message: Data) -> String {
        Base64URL.encode(Data(HMAC<SHA256>.authenticationCode(for: message, using: key)))
    }
    static func verify(_ key: SymmetricKey, _ message: Data, _ proof: String) throws {
        let bytes = try Base64URL.decode(proof, bytes: 32)
        guard HMAC<SHA256>.isValidAuthenticationCode(bytes, authenticating: message, using: key) else {
            throw RemoteError.protocolViolation("Desktop authentication failed. Pair again with the correct desktop.")
        }
    }
}

func v2Random32() throws -> Data {
    var random = [UInt8](repeating: 0, count: 32)
    guard SecRandomCopyBytes(kSecRandomDefault, random.count, &random) == errSecSuccess else {
        throw RemoteError.protocolViolation("Secure randomness unavailable.")
    }
    return Data(random)
}

func v2PairPrefix(identity: Data, inviteID: String, expiresAt: UInt64, relayURL: String) throws -> Data {
    guard let uuid = UUID(uuidString: inviteID) else { throw RemoteError.protocolViolation("Invalid invite.") }
    var invite = uuid.uuid
    var body = identity
    body.append(withUnsafeBytes(of: &invite) { Data($0) })
    var expires = expiresAt.bigEndian
    body.append(withUnsafeBytes(of: &expires) { Data($0) })
    let url = Data(relayURL.utf8)
    guard url.count <= 2048 else { throw RemoteError.protocolViolation("Relay URL too long.") }
    var length = UInt16(url.count).bigEndian
    body.append(withUnsafeBytes(of: &length) { Data($0) })
    body.append(url)
    return body
}

func v2Root(inviteSecret: Data, transcript: Data) -> SymmetricKey {
    let salt = Data(SHA256.hash(data: transcript))
    return HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: inviteSecret), salt: salt, info: Data("riwork/v2/root".utf8), outputByteCount: 32)
}

func v2SharedSecret(privateScalar: Data, peerPublic: Data) throws -> Data {
    let secret = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: privateScalar)
    let peer = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: peerPublic)
    let shared = try secret.sharedSecretFromKeyAgreement(with: peer)
    let bytes = shared.withUnsafeBytes { Data($0) }
    guard bytes.count == 32, bytes != Data(repeating: 0, count: 32) else {
        throw RemoteError.protocolViolation("Degenerate Diffie-Hellman output.")
    }
    return bytes
}

func v2SessionKeys(root: SymmetricKey, dh: Data, transcript: Data) -> (cipher: SessionCipher, handshake: SymmetricKey) {
    let salt = Data(SHA256.hash(data: transcript))
    let rootBytes = root.withUnsafeBytes { Data($0) }
    let material = SymmetricKey(data: rootBytes + dh)
    func expand(_ info: String) -> SymmetricKey {
        HKDF<SHA256>.deriveKey(inputKeyMaterial: material, salt: salt, info: Data(info.utf8), outputByteCount: 32)
    }
    let cipher = SessionCipher(version: 2, sessionID: salt.prefix(16), c2d: expand("riwork/v2/c2d"), d2c: expand("riwork/v2/d2c"))
    return (cipher, expand("riwork/v2/hs"))
}

/// Proves possession of a single-use v2 invite and replaces it with the root key.
public struct V2Invite: Sendable {
    public let pairing: Pairing
    public let clientNonce: Data
    private let secret: SymmetricKey
    private let identity: Data
    private let expiresAt: UInt64
    private let prefix: Data

    public init(pairing: Pairing, nonce: Data? = nil) throws {
        guard pairing.v == 2, pairing.invite_state == "pending", let encoded = pairing.invite_secret, let inviteID = pairing.invite_id, let expiresAt = pairing.expires_at else {
            throw RemoteError.protocolViolation("This pairing is not an unused invite.")
        }
        self.pairing = pairing
        secret = SymmetricKey(data: try Base64URL.decode(encoded, bytes: 32))
        identity = try pairing.identityBytes()
        self.expiresAt = expiresAt
        prefix = try v2PairPrefix(identity: identity, inviteID: inviteID, expiresAt: expiresAt, relayURL: pairing.relay_url)
        if let nonce {
            guard nonce.count == 32 else { throw RemoteError.protocolViolation("Invalid nonce.") }
            clientNonce = nonce
        } else {
            clientNonce = try v2Random32()
        }
    }
    public var hello: JSONValue {
        let body = Data("riwork/v2/pair-hello\0".utf8) + prefix + clientNonce
        return .object([
            "v": .number(2), "type": .string("pair_hello"),
            "invite_id": .string(pairing.invite_id ?? ""),
            "desktop_id": .string(pairing.desktop_id), "device_id": .string(pairing.device_id), "route_id": .string(pairing.route_id),
            "client_nonce": .string(Base64URL.encode(clientNonce)),
            "mac": .string(V2MAC.code(secret, body)),
        ])
    }
    public func accept(_ message: JSONValue) throws -> (finish: JSONValue, established: Pairing) {
        guard message["v"] == .number(2), message["type"].string == "pair_accept", let nonce = message["desktop_nonce"].string, let proof = message["mac"].string else {
            throw RemoteError.protocolViolation("Expected authenticated pair accept.")
        }
        let desktopNonce = try Base64URL.decode(nonce, bytes: 32)
        let transcript = Data("riwork/v2/pair\0".utf8) + prefix + clientNonce + desktopNonce
        let root = v2Root(inviteSecret: secret.withUnsafeBytes { Data($0) }, transcript: transcript)
        try V2MAC.verify(root, Data("riwork/v2/pair-accept\0".utf8) + transcript, proof)
        let finish = JSONValue.object(["v": .number(2), "type": .string("pair_finish"), "mac": .string(V2MAC.code(root, Data("riwork/v2/pair-finish\0".utf8) + transcript))])
        return (finish, pairing.established(rootKey: root.withUnsafeBytes { Base64URL.encode(Data($0)) }))
    }
}

/// One X25519 handshake under an established v2 root key. Each call uses a fresh scalar.
public struct V2SessionHandshake: Sendable {
    public let pairing: Pairing
    private let root: SymmetricKey
    private let identity: Data
    private let clientPrivate: Data
    private let clientPublic: Data

    public init(pairing: Pairing, privateKey: Data? = nil) throws {
        guard pairing.v == 2, let encoded = pairing.root_key else { throw RemoteError.protocolViolation("Pairing is not established.") }
        self.pairing = pairing
        root = SymmetricKey(data: try Base64URL.decode(encoded, bytes: 32))
        identity = try pairing.identityBytes()
        let scalar = try privateKey ?? v2Random32()
        guard scalar.count == 32 else { throw RemoteError.protocolViolation("Invalid ephemeral key.") }
        let key = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: scalar)
        clientPrivate = scalar
        clientPublic = key.publicKey.rawRepresentation
    }
    public var hello: JSONValue {
        let body = Data("riwork/v2/client-hello\0".utf8) + identity + clientPublic
        return .object([
            "v": .number(2), "type": .string("client_hello"),
            "desktop_id": .string(pairing.desktop_id), "device_id": .string(pairing.device_id), "route_id": .string(pairing.route_id),
            "client_eph": .string(Base64URL.encode(clientPublic)),
            "mac": .string(V2MAC.code(root, body)),
        ])
    }
    public func accept(_ serverHello: JSONValue) throws -> (finish: JSONValue, cipher: SessionCipher) {
        guard serverHello["v"] == .number(2), serverHello["type"].string == "server_hello", let encoded = serverHello["desktop_eph"].string, let proof = serverHello["mac"].string else {
            throw RemoteError.protocolViolation("Expected authenticated server hello.")
        }
        let desktopPublic = try Base64URL.decode(encoded, bytes: 32)
        let dh = try v2SharedSecret(privateScalar: clientPrivate, peerPublic: desktopPublic)
        let transcript = Data("riwork/v2/session\0".utf8) + identity + clientPublic + desktopPublic
        let keys = v2SessionKeys(root: root, dh: dh, transcript: transcript)
        try V2MAC.verify(keys.handshake, Data("riwork/v2/server-hello\0".utf8) + transcript, proof)
        let finish = JSONValue.object(["v": .number(2), "type": .string("client_finish"), "mac": .string(V2MAC.code(keys.handshake, Data("riwork/v2/client-finish\0".utf8) + transcript))])
        return (finish, keys.cipher)
    }
}
