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
