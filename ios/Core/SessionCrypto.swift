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
        var bytes = Data()
        for id in [pairing.desktop_id, pairing.device_id, pairing.route_id] {
            guard let uuid = UUID(uuidString: id) else { throw RemoteError.protocolViolation("Invalid identity.") }
            var raw = uuid.uuid
            bytes.append(withUnsafeBytes(of: &raw) { Data($0) })
        }
        identity = bytes
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
    public let sessionID: Data
    public let c2d: SymmetricKey
    public let d2c: SymmetricKey
    public private(set) var sendCounter: UInt64
    public private(set) var receiveCounter: UInt64
    public init(psk: SymmetricKey, transcript: Data) { self.init(psk: psk, transcript: transcript, sendCounter: 0, receiveCounter: 0) }
    /// Tests start mid-stream to reach large counters without sealing millions of frames.
    init(psk: SymmetricKey, transcript: Data, sendCounter: UInt64, receiveCounter: UInt64) {
        self.sendCounter = sendCounter; self.receiveCounter = receiveCounter
        let salt = Data(SHA256.hash(data: transcript))
        sessionID = salt.prefix(16)
        c2d = HKDF<SHA256>.deriveKey(inputKeyMaterial: psk, salt: salt, info: Data("riwork/v1/c2d".utf8), outputByteCount: 32)
        d2c = HKDF<SHA256>.deriveKey(inputKeyMaterial: psk, salt: salt, info: Data("riwork/v1/d2c".utf8), outputByteCount: 32)
    }
    public static func counterBytes(_ counter: UInt64) -> Data { var big = counter.bigEndian; return withUnsafeBytes(of: &big) { Data($0) } }
    public func aad(direction: String, counter: UInt64) -> Data { Data("riwork/v1/frame\0".utf8) + sessionID + Data([direction == "c2d" ? 0 : 1]) + Self.counterBytes(counter) }
    public mutating func seal(_ payload: JSONValue) throws -> JSONValue {
        try sealJSON(JSONEncoder().encode(payload))
    }
    public mutating func sealJSON(_ data: Data) throws -> JSONValue {
        guard sendCounter < UInt64.max else { throw RemoteError.protocolViolation("Counter exhausted; reconnect.") }
        guard data.count <= 131072 else { throw RemoteError.protocolViolation("Request too large.") }
        guard try JSONDecoder().decode(JSONValue.self, from: data)["v"] == .number(1) else { throw RemoteError.protocolViolation("Unsupported request version.") }
        let nonce = try ChaChaPoly.Nonce(data: Data(repeating: 0, count: 4) + Self.counterBytes(sendCounter))
        let box = try ChaChaPoly.seal(data, using: c2d, nonce: nonce, authenticating: aad(direction: "c2d", counter: sendCounter))
        let envelope = JSONValue.object(["v": .number(1), "type": .string("encrypted"), "session_id": .string(Base64URL.encode(sessionID)), "direction": .string("c2d"), "counter": .string(String(sendCounter)), "ciphertext": .string(Base64URL.encode(box.ciphertext + box.tag))])
        sendCounter += 1
        return envelope
    }
    public mutating func open(_ frame: JSONValue) throws -> JSONValue {
        guard receiveCounter < UInt64.max, frame["v"] == .number(1), frame["type"].string == "encrypted", frame["session_id"].string == Base64URL.encode(sessionID), frame["direction"].string == "d2c", frame["counter"].string == String(receiveCounter), let encoded = frame["ciphertext"].string else { throw RemoteError.protocolViolation("Stale, replayed or out-of-order frame.") }
        let bytes = try Base64URL.decode(encoded)
        guard bytes.count >= 16, bytes.count <= 131088 else { throw RemoteError.protocolViolation("Invalid frame size.") }
        let nonce = try ChaChaPoly.Nonce(data: Data(repeating: 0, count: 4) + Self.counterBytes(receiveCounter))
        let box = try ChaChaPoly.SealedBox(nonce: nonce, ciphertext: bytes.dropLast(16), tag: bytes.suffix(16))
        let plaintext: Data
        do { plaintext = try ChaChaPoly.open(box, using: d2c, authenticating: aad(direction: "d2c", counter: receiveCounter)) }
        catch { throw RemoteError.protocolViolation("Encrypted frame authentication failed.") }
        let payload = try JSONDecoder().decode(JSONValue.self, from: plaintext)
        guard payload["v"] == .number(1) else { throw RemoteError.protocolViolation("Unsupported payload version.") }
        receiveCounter += 1
        return payload
    }
}
