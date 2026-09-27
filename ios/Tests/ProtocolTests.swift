import XCTest
import CryptoKit
@testable import RiWorkCore

final class ProtocolTests: XCTestCase {
    private func fixture() throws -> JSONValue {
        #if SWIFT_PACKAGE
        let bundle = Bundle.module
        #else
        let bundle = Bundle(for: Self.self)
        #endif
        let url = try XCTUnwrap(bundle.url(forResource: "v1", withExtension: "json", subdirectory: "Fixtures") ?? bundle.url(forResource: "v1", withExtension: "json"))
        return try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: url))
    }
    private func pairing(_ fixture: JSONValue) throws -> Pairing {
        try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"\(fixture["desktop_id"].string!)","device_id":"\(fixture["device_id"].string!)","route_id":"\(fixture["route_id"].string!)","device_name":"Test","pairing_secret":"\(fixture["pairing_secret"].string!)","relay_token":"\(fixture["pairing_secret"].string!)"}
        """)
    }
    private func connectedCipher() throws -> (JSONValue, SessionCipher) {
        let f = try fixture()
        let handshake = try ClientHandshake(pairing: pairing(f), nonce: Base64URL.decode(f["client_nonce"].string!, bytes: 32))
        return (f, try handshake.accept(f["server_hello"]).cipher)
    }
    private func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }
    private func change(_ value: JSONValue, key: String, to changed: JSONValue) -> JSONValue { guard case .object(var fields) = value else { return value }; fields[key] = changed; return .object(fields) }

    func testPublishedRustSwiftVector() throws {
        let f = try fixture()
        let handshake = try ClientHandshake(pairing: pairing(f), nonce: Base64URL.decode(f["client_nonce"].string!, bytes: 32))
        XCTAssertEqual(handshake.hello, f["client_hello"])
        XCTAssertEqual(hex(handshake.identity), f["identity_hex"].string)
        let result = try handshake.accept(f["server_hello"])
        XCTAssertEqual(result.finish, f["client_finish"])
        var cipher = result.cipher
        XCTAssertEqual(cipher.c2d.withUnsafeBytes { hex(Data($0)) }, f["c2d_key_hex"].string)
        XCTAssertEqual(cipher.d2c.withUnsafeBytes { hex(Data($0)) }, f["d2c_key_hex"].string)
        XCTAssertEqual(Base64URL.encode(cipher.sessionID), f["session_id"].string)
        XCTAssertEqual(hex(cipher.aad(direction: "c2d", counter: 0)), f["frames"].array[0]["aad_hex"].string)
        let request = f["frames"].array[0]
        XCTAssertEqual(try cipher.sealJSON(Data(request["plaintext_utf8"].string!.utf8)), request["envelope"])
        XCTAssertEqual(cipher.sendCounter, 1)
        let ready = f["frames"].array[1]
        XCTAssertEqual(try cipher.open(ready["envelope"]), try JSONDecoder().decode(JSONValue.self, from: Data(ready["plaintext_utf8"].string!.utf8)))
        XCTAssertEqual(cipher.receiveCounter, 1)
    }
    func testTamperDoesNotAdvanceCounter() throws {
        let (f, original) = try connectedCipher()
        let frame = f["frames"].array[1]["envelope"]
        var bytes = try Base64URL.decode(frame["ciphertext"].string!)
        bytes[bytes.startIndex] ^= 1
        var cipher = original
        XCTAssertThrowsError(try cipher.open(change(frame, key: "ciphertext", to: .string(Base64URL.encode(bytes)))))
        XCTAssertEqual(cipher.receiveCounter, 0)
        XCTAssertEqual(try cipher.open(frame)["type"].string, "ready")
        XCTAssertThrowsError(try cipher.open(frame))
        XCTAssertEqual(cipher.receiveCounter, 1)
    }
    func testRejectsReplayGapDirectionSessionCounterEncodingAndVersion() throws {
        let (f, original) = try connectedCipher()
        let frame = f["frames"].array[1]["envelope"]
        for (key, value) in [("counter", JSONValue.string("1")), ("counter", .string("00")), ("counter", .number(0)), ("direction", .string("c2d")), ("session_id", .string("wrong")), ("v", .number(2)), ("type", .string("ready"))] {
            var cipher = original
            XCTAssertThrowsError(try cipher.open(change(frame, key: key, to: value)), key)
            XCTAssertEqual(cipher.receiveCounter, 0)
        }
    }
    func testUnauthenticatedOrOutOfOrderHelloIsRejected() throws {
        let f = try fixture()
        let h = try ClientHandshake(pairing: pairing(f), nonce: Base64URL.decode(f["client_nonce"].string!))
        XCTAssertThrowsError(try h.accept(f["client_finish"]))
        XCTAssertThrowsError(try h.accept(change(f["server_hello"], key: "mac", to: .string(f["client_hello"]["mac"].string!))))
        XCTAssertThrowsError(try h.accept(change(f["server_hello"], key: "desktop_nonce", to: f["client_nonce"])))
    }
    func testFreshReconnectChangesKeysAndRejectsOldFrames() throws {
        let (f, old) = try connectedCipher()
        let h = try ClientHandshake(pairing: pairing(f))
        XCTAssertNotEqual(Base64URL.encode(h.clientNonce), f["client_nonce"].string)
        let d = try Base64URL.decode(f["desktop_nonce"].string!)
        let transcript = Data("riwork/v1/session\0".utf8) + h.identity + h.clientNonce + d
        let proof = h.mac(Data("riwork/v1/server-hello\0".utf8) + transcript)
        var fresh = try h.accept(.object(["v": .number(1), "type": .string("server_hello"), "desktop_nonce": f["desktop_nonce"], "mac": .string(proof)])).cipher
        XCTAssertNotEqual(fresh.sessionID, old.sessionID)
        XCTAssertNotEqual(fresh.c2d.withUnsafeBytes { Data($0) }, old.c2d.withUnsafeBytes { Data($0) })
        XCTAssertEqual(fresh.sendCounter, 0)
        XCTAssertThrowsError(try fresh.open(f["frames"].array[1]["envelope"]))
    }
    func testPairingDeepLinkAndLocalDevelopmentPolicy() throws {
        let p = try pairing(fixture())
        let data = try JSONEncoder().encode(p)
        XCTAssertEqual(try Pairing.parse("riwork://pair?v=1&data=\(Base64URL.encode(data))"), p)
        XCTAssertThrowsError(try Pairing.parse("riwork://pair?v=2&data=\(Base64URL.encode(data))"))
        XCTAssertThrowsError(try Pairing.parse("riwork://pair?v=1&v=1&data=\(Base64URL.encode(data))"))
        let local = String(data: data, encoding: .utf8)!.replacingOccurrences(of: "wss:\\/\\/example.com", with: "ws:\\/\\/127.0.0.1:9876").replacingOccurrences(of: "wss://example.com", with: "ws://127.0.0.1:9876")
        XCTAssertThrowsError(try Pairing.parse(local))
        XCTAssertEqual(try Pairing.parse(local, allowLocalDevelopment: true).relayHost, "127.0.0.1")
        XCTAssertThrowsError(try Pairing.parse(local.replacingOccurrences(of: "127.0.0.1", with: "192.168.1.5"), allowLocalDevelopment: true))
        XCTAssertThrowsError(try Base64URL.decode("AA=="))
        XCTAssertThrowsError(try Base64URL.decode("AB"))
    }
    func testRequestsRequireExplicitIDsAndBoundedSingleLine() throws {
        let id = "44444444-4444-4444-8444-444444444444"
        try RequestValidation.validate(method: "shell.input", params: ["shell_id": .string(id), "line": .string("continue the tests")], id: id)
        XCTAssertThrowsError(try RequestValidation.validate(method: "shell.create", params: [:], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "shells.list", params: [:], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "projects.list", params: ["command": .string("x")], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "shell.output", params: ["shell_id": .string(String(id.prefix(8)))], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "shell.output", params: ["shell_id": .string(id), "lines": .number(2001)], id: id))
        try RequestValidation.validate(method: "shell.resize", params: ["shell_id": .string(id), "columns": .number(43), "rows": .number(17)], id: id)
        try RequestValidation.validate(method: "shell.resize.clear", params: ["shell_id": .string(id)], id: id)
        for value in [19.0, 301, 43.5] { XCTAssertThrowsError(try RequestValidation.validate(method: "shell.resize", params: ["shell_id": .string(id), "columns": .number(value), "rows": .number(17)], id: id)) }
        XCTAssertThrowsError(try RequestValidation.validate(method: "shell.resize", params: ["shell_id": .string(id), "columns": .number(43), "rows": .null], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "shell.resize.clear", params: ["shell_id": .string(id), "owner": .string(id)], id: id))
        for line in ["a\nb", "a\rb", "a\u{0}b", "a\tb", "a\u{2028}b", "a\u{2029}b", String(repeating: "é", count: 4097), " "] { XCTAssertThrowsError(try InputValidation.validate(line)) }
        try InputValidation.validate(String(repeating: "é", count: 4096))
    }
    func testReadableTerminalOutput() {
        XCTAssertEqual(TerminalText.readable("\u{1b}[32mhello\u{1b}[0m\r\nold\rnew\n\u{1b}]0;title\u{7}done"), "hello\nnew\ndone")
        XCTAssertEqual(TerminalText.readable("\u{e000}branch\n\n  \n"), "branch")
    }
    func testViewportFitsPhoneTabletKeyboardAndRejectsInvalidGeometry() {
        XCTAssertEqual(TerminalViewport.fit(width: 360, height: 400, cellWidth: 8, lineHeight: 16), TerminalViewport(columns: 45, rows: 25))
        XCTAssertEqual(TerminalViewport.fit(width: 800, height: 1000, cellWidth: 8, lineHeight: 16), TerminalViewport(columns: 100, rows: 62))
        XCTAssertEqual(TerminalViewport.fit(width: 360, height: 160, cellWidth: 8, lineHeight: 16)?.rows, 10)
        XCTAssertNil(TerminalViewport.fit(width: .infinity, height: 200, cellWidth: 8, lineHeight: 16))
        XCTAssertNil(TerminalViewport.fit(width: 400, height: 0, cellWidth: 8, lineHeight: 16))
        XCTAssertEqual(TerminalViewport.fit(width: 1e100, height: 1, cellWidth: 8, lineHeight: 16), TerminalViewport(columns: 300, rows: 8))
    }
}
