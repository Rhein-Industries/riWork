// Native CryptoKit interoperability check. No iOS project dependencies.
// swift remote/fixtures/verify.swift remote/fixtures/v1.json
import Foundation
import CryptoKit

func b64(_ data: Data) -> String {
    data.base64EncodedString().replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
}
func decode(_ value: String) -> Data {
    let s = value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
    return Data(base64Encoded: s + String(repeating: "=", count: (4 - s.count % 4) % 4))!
}
func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }
func bytes(_ id: String) -> Data { var u = UUID(uuidString: id)!.uuid; return withUnsafeBytes(of: &u) { Data($0) } }
func ascii(_ s: String) -> Data { Data(s.utf8) }
func check(_ ok: @autoclosure () -> Bool, _ label: String) { precondition(ok(), label) }

let fixture = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))) as! [String: Any]
if (fixture["v"] as? Int) == 2 {
    func str(_ key: String) -> String { fixture[key] as! String }
    func obj(_ key: String) -> [String: Any] { fixture[key] as! [String: Any] }
    func mac(_ key: SymmetricKey, _ message: Data) -> String { b64(Data(HMAC<SHA256>.authenticationCode(for: message, using: key))) }
    func be(_ value: UInt64) -> Data { var big = value.bigEndian; return withUnsafeBytes(of: &big) { Data($0) } }
    func be16(_ value: Int) -> Data { var big = UInt16(value).bigEndian; return withUnsafeBytes(of: &big) { Data($0) } }
    let inviteSecret = decode(str("invite_secret"))
    let pairC = decode(str("pair_client_nonce")), pairD = decode(str("pair_desktop_nonce"))
    let identity = bytes(str("desktop_id")) + bytes(str("device_id")) + bytes(str("route_id"))
    let url = ascii(str("relay_url"))
    let prefix = identity + bytes(str("invite_id")) + be(UInt64(fixture["expires_at"] as! Int)) + be16(url.count) + url
    let transcript = ascii("riwork/v2/pair\0") + prefix + pairC + pairD
    let root = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: inviteSecret), salt: Data(SHA256.hash(data: transcript)), info: ascii("riwork/v2/root"), outputByteCount: 32)
    check(root.withUnsafeBytes { hex(Data($0)) } == str("root_key_hex"), "root")
    check(mac(SymmetricKey(data: inviteSecret), ascii("riwork/v2/pair-hello\0") + prefix + pairC) == obj("pair_hello")["mac"] as! String, "pair hello")
    check(mac(root, ascii("riwork/v2/pair-accept\0") + transcript) == obj("pair_accept")["mac"] as! String, "pair accept")
    check(mac(root, ascii("riwork/v2/pair-finish\0") + transcript) == obj("pair_finish")["mac"] as! String, "pair finish")
    let clientPrivate = Data(hexBytes: str("client_private_hex")), desktopPrivate = Data(hexBytes: str("desktop_private_hex"))
    let clientPublic = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: clientPrivate).publicKey.rawRepresentation
    let desktopPublic = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: desktopPrivate).publicKey.rawRepresentation
    check(hex(clientPublic) == str("client_public_hex"), "client public")
    check(hex(desktopPublic) == str("desktop_public_hex"), "desktop public")
    let dh = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: clientPrivate).sharedSecretFromKeyAgreement(with: try Curve25519.KeyAgreement.PublicKey(rawRepresentation: desktopPublic)).withUnsafeBytes { Data($0) }
    check(hex(dh) == str("dh_hex"), "diffie-hellman")
    let session = ascii("riwork/v2/session\0") + identity + clientPublic + desktopPublic
    let salt = Data(SHA256.hash(data: session))
    check(b64(salt.prefix(16)) == str("session_id"), "session id")
    let material = SymmetricKey(data: root.withUnsafeBytes { Data($0) } + dh)
    var keys: [String: SymmetricKey] = [:]
    for name in ["c2d", "d2c", "hs"] {
        let key = HKDF<SHA256>.deriveKey(inputKeyMaterial: material, salt: salt, info: ascii("riwork/v2/" + name), outputByteCount: 32)
        if name != "hs" { check(key.withUnsafeBytes { hex(Data($0)) } == str(name + "_key_hex"), name) }
        keys[name] = key
    }
    check(mac(root, ascii("riwork/v2/client-hello\0") + identity + clientPublic) == obj("client_hello")["mac"] as! String, "client hello")
    check(mac(keys["hs"]!, ascii("riwork/v2/server-hello\0") + session) == obj("server_hello")["mac"] as! String, "server hello")
    check(mac(keys["hs"]!, ascii("riwork/v2/client-finish\0") + session) == obj("client_finish")["mac"] as! String, "client finish")
    for frame in fixture["frames"] as! [[String: Any]] {
        let envelope = frame["envelope"] as! [String: Any], direction = envelope["direction"] as! String
        let plain = ascii(frame["plaintext_utf8"] as! String)
        let aad = ascii("riwork/v2/frame\0") + salt.prefix(16) + Data([direction == "c2d" ? 0 : 1]) + Data(repeating: 0, count: 8)
        check(hex(aad) == frame["aad_hex"] as! String, "aad")
        let box = try ChaChaPoly.seal(plain, using: keys[direction]!, nonce: try ChaChaPoly.Nonce(data: Data(repeating: 0, count: 12)), authenticating: aad)
        check(b64(box.ciphertext + box.tag) == envelope["ciphertext"] as! String, direction)
        check(envelope["v"] as! Int == 2, "envelope version")
    }
    print("PASS: CryptoKit v2 invite, X25519 session, HKDF and ChaChaPoly fixtures")
    exit(0)
}
extension Data { init(hexBytes hex: String) { self.init(stride(from: 0, to: hex.count, by: 2).map { UInt8(hex[hex.index(hex.startIndex, offsetBy: $0)...hex.index(hex.startIndex, offsetBy: $0 + 1)], radix: 16)! }) } }
func str(_ key: String) -> String { fixture[key] as! String }
let secret = SymmetricKey(data: decode(str("pairing_secret")))
let c = decode(str("client_nonce")), d = decode(str("desktop_nonce"))
let identity = bytes(str("desktop_id")) + bytes(str("device_id")) + bytes(str("route_id"))
let transcript = ascii("riwork/v1/session\0") + identity + c + d
let salt = Data(SHA256.hash(data: transcript)), sessionID = salt.prefix(16)
check(hex(identity) == str("identity_hex"), "UUID bytes")
check(hex(transcript) == str("transcript_hex"), "transcript")
check(hex(salt) == str("salt_hex"), "salt")
check(b64(sessionID) == str("session_id"), "session ID")
for (field, label, body) in [("client_hello", "riwork/v1/client-hello\0", identity + c),
                            ("server_hello", "riwork/v1/server-hello\0", transcript),
                            ("client_finish", "riwork/v1/client-finish\0", transcript)] {
    let message = fixture[field] as! [String: Any]
    let proof = Data(HMAC<SHA256>.authenticationCode(for: ascii(label) + body, using: secret))
    check(b64(proof) == message["mac"] as! String, field)
}
var keys: [String: SymmetricKey] = [:]
for direction in ["c2d", "d2c"] {
    let key = HKDF<SHA256>.deriveKey(inputKeyMaterial: secret, salt: salt,
                                   info: ascii("riwork/v1/" + direction), outputByteCount: 32)
    check(key.withUnsafeBytes { hex(Data($0)) } == str(direction + "_key_hex"), "HKDF " + direction)
    keys[direction] = key
}
for frame in fixture["frames"] as! [[String: Any]] {
    let envelope = frame["envelope"] as! [String: Any], direction = envelope["direction"] as! String
    let plain = ascii(frame["plaintext_utf8"] as! String), nonce = Data(repeating: 0, count: 12)
    let aad = ascii("riwork/v1/frame\0") + sessionID + Data([direction == "c2d" ? 0 : 1]) + Data(repeating: 0, count: 8)
    check(hex(nonce) == frame["nonce_hex"] as! String, "nonce")
    check(hex(aad) == frame["aad_hex"] as! String, "AAD")
    let box = try ChaChaPoly.seal(plain, using: keys[direction]!, nonce: ChaChaPoly.Nonce(data: nonce), authenticating: aad)
    check(b64(box.ciphertext + box.tag) == envelope["ciphertext"] as! String, "ciphertext/tag " + direction)
    let opened = try ChaChaPoly.open(box, using: keys[direction]!, authenticating: aad)
    check(opened == plain, "decryption")
}
print("PASS: native CryptoKit UUID bytes, handshake MACs, HKDF keys, nonce/AAD and ChaChaPoly fixtures")
