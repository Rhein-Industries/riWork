import XCTest
import CryptoKit
@testable import RiWorkCore

final class ProtocolTests: XCTestCase {
    private func fixture(_ name: String = "v1") throws -> JSONValue {
        #if SWIFT_PACKAGE
        let bundle = Bundle.module
        #else
        let bundle = Bundle(for: Self.self)
        #endif
        let url = try XCTUnwrap(bundle.url(forResource: name, withExtension: "json", subdirectory: "Fixtures") ?? bundle.url(forResource: name, withExtension: "json"))
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
    /// Vectors above counter 0 come from an independent RFC 8439 implementation that first reproduces the
    /// Rust fixture (`ios/scripts/gen-counter-vectors.py`). They pin big-endian nonce and AAD bytes.
    func testCounterVectorsAboveZeroMatchIndependentDerivation() throws {
        let f = try fixture(), vectors = try fixture("counter-vectors")["vectors"].array
        XCTAssertEqual(vectors.count, 16)
        let psk = SymmetricKey(data: try Base64URL.decode(f["pairing_secret"].string!, bytes: 32))
        let transcript = Data(hexBytes: f["transcript_hex"].string!)
        for vector in vectors {
            let counter = try XCTUnwrap(UInt64(vector["counter"].string!))
            XCTAssertGreaterThan(counter, 0)
            let direction = vector["direction"].string!
            var cipher = SessionCipher(psk: psk, transcript: transcript, sendCounter: counter, receiveCounter: counter)
            XCTAssertEqual(hex(Data(repeating: 0, count: 4) + SessionCipher.counterBytes(counter)), vector["nonce_hex"].string, "nonce \(counter)")
            XCTAssertEqual(hex(cipher.aad(direction: direction, counter: counter)), vector["aad_hex"].string, "aad \(direction) \(counter)")
            let envelope = vector["envelope"], plaintext = try JSONDecoder().decode(JSONValue.self, from: Data(vector["plaintext_utf8"].string!.utf8))
            if direction == "c2d" {
                XCTAssertEqual(try cipher.sealJSON(Data(vector["plaintext_utf8"].string!.utf8)), envelope, "seal \(counter)")
                XCTAssertEqual(cipher.sendCounter, counter + 1)
            } else {
                XCTAssertEqual(try cipher.open(envelope), plaintext, "open \(counter)")
                XCTAssertEqual(cipher.receiveCounter, counter + 1)
                // The same ciphertext is bound to its counter: neither a neighbouring nor a stale counter opens it.
                for wrong in [counter - 1, counter + 1] {
                    var other = SessionCipher(psk: psk, transcript: transcript, sendCounter: 0, receiveCounter: wrong)
                    XCTAssertThrowsError(try other.open(envelope), "counter \(wrong) must not accept \(counter)")
                    XCTAssertEqual(other.receiveCounter, wrong)
                }
            }
        }
    }
    func testSequentialFramesFollowFixtureThenVectors() throws {
        let f = try fixture(), vectors = try fixture("counter-vectors")["vectors"].array
        func vector(_ direction: String, _ counter: String) -> JSONValue { vectors.first { $0["direction"].string == direction && $0["counter"].string == counter }! }
        var cipher = try connectedCipher().1
        let request = Data(f["frames"].array[0]["plaintext_utf8"].string!.utf8)
        XCTAssertEqual(try cipher.sealJSON(request), f["frames"].array[0]["envelope"])
        for counter in ["1", "2"] { XCTAssertEqual(try cipher.sealJSON(request), vector("c2d", counter)["envelope"]) }
        XCTAssertEqual(cipher.sendCounter, 3)
        _ = try cipher.open(f["frames"].array[1]["envelope"])
        for counter in ["1", "2"] { XCTAssertEqual(try cipher.open(vector("d2c", counter)["envelope"])["type"].string, "response") }
        XCTAssertEqual(cipher.receiveCounter, 3)
        XCTAssertThrowsError(try cipher.open(vector("d2c", "2")["envelope"]), "replaying counter 2 must fail")
    }
    func testHostilePairingLinksAreRejected() throws {
        let data = Base64URL.encode(try JSONEncoder().encode(pairing(fixture())))
        XCTAssertNoThrow(try Pairing.parse("riwork://pair?v=1&data=\(data)"))
        XCTAssertNoThrow(try Pairing.parse("riwork://pair/?data=\(data)&v=1"))
        let hostile = [
            "riwork://evil@pair?v=1&data=\(data)",             // userinfo
            "riwork://user:secret@pair?v=1&data=\(data)",
            "riwork://pair:8080?v=1&data=\(data)",              // port
            "riwork://p%61ir?v=1&data=\(data)",                 // percent-encoded host
            "riwork://pair/%2e%2e?v=1&data=\(data)",            // percent-encoded path
            "riwork://pair/%2F?v=1&data=\(data)",
            "riwork://pair/extra?v=1&data=\(data)",
            "riwork://pair//?v=1&data=\(data)",
            "RIWORK://pair?v=1&data=\(data)",                   // scheme is case-sensitive in v1
            "Riwork://pair?v=1&data=\(data)",
            "riwork://pair?%76=1&data=\(data)",                 // percent-encoded query names
            "riwork://pair?v=1&%64ata=\(data)",
            "riwork://pair?v=1&data=\(data.prefix(20))%41\(data.dropFirst(21))",
            "riwork://pair?v=1&data=\(data)&x=1",
            "riwork://pair?v=1&v=1&data=\(data)",
            "riwork://pair?v=1&data=\(data)#fragment",
            "riwork:pair?v=1&data=\(data)",
            "riwork:///pair?v=1&data=\(data)",
            "https://pair?v=1&data=\(data)",
        ]
        for link in hostile { XCTAssertThrowsError(try Pairing.parse(link), link) }
        // An uppercase scheme is reported as a bad link, not as bad JSON.
        do { _ = try Pairing.parse("RIWORK://pair?v=1&data=\(data)"); XCTFail() }
        catch { XCTAssertTrue(error.localizedDescription.contains("riwork://pair"), error.localizedDescription) }
    }
    func testPairingDisplayFieldsAreBoundedPrintableText() throws {
        var p = try pairing(fixture())
        XCTAssertEqual(p.relayHost, "example.com")
        XCTAssertEqual(p.desktopShortID, "11111111")
        XCTAssertFalse(p.usesLocalDevelopmentRelay)
        let json = String(data: try JSONEncoder().encode(p), encoding: .utf8)!.replacingOccurrences(of: "\"device_name\":\"Test\"", with: "\"device_name\":\"\\u001b[2Jevil\\nname\(String(repeating: "x", count: 200))\"")
        p = try Pairing.parse(json)
        XCTAssertFalse(p.displayDeviceName.unicodeScalars.contains { $0.value < 32 })
        XCTAssertLessThanOrEqual(p.displayDeviceName.count, 80)
        XCTAssertTrue(p.displayDeviceName.hasPrefix("[2Jevilname"))
    }
    func testTerminalTextStopsOSCAtTheFirstTerminator() {
        XCTAssertEqual(TerminalText.readable("a\u{1b}]0;title\u{1b}\\visible\u{7}b"), "avisibleb", "ESC \\ ends the OSC; visible text after it must survive a later BEL")
        XCTAssertEqual(TerminalText.readable("\u{1b}]0;one\u{7}keep\u{1b}]0;two\u{7}"), "keep")
        XCTAssertEqual(TerminalText.readable("\u{1b}]8;;https://example.com\u{1b}\\link\u{1b}]8;;\u{1b}\\ text"), "link text")
        XCTAssertEqual(TerminalText.readable("\u{1b}]0;no terminator\nnext line"), "0;no terminator\nnext line", "an unterminated OSC must not eat following lines")
    }
    func testRelayCloseCodesReadAsSentences() {
        XCTAssertTrue(RemoteError.relayClosed(code: 1005, reason: nil).localizedDescription.contains("another connection"))
        XCTAssertTrue(RemoteError.relayClosed(code: 1008, reason: nil).localizedDescription.contains("authorized"))
        XCTAssertTrue(RemoteError.relayClosed(code: 1001, reason: nil).localizedDescription.contains("restarting"))
        XCTAssertEqual(RemoteError.relayClosed(code: 4999, reason: nil).localizedDescription, "The relay closed the connection (code 4999).")
        let shown = RemoteError.relayClosed(code: 1011, reason: "boom\u{1b}[31m\n\(String(repeating: "y", count: 500))").localizedDescription
        XCTAssertFalse(shown.unicodeScalars.contains { $0.value < 32 })
        XCTAssertLessThan(shown.count, 320)
    }
}

private extension Data {
    init(hexBytes hex: String) {
        self.init(stride(from: 0, to: hex.count, by: 2).map { UInt8(hex[hex.index(hex.startIndex, offsetBy: $0)...hex.index(hex.startIndex, offsetBy: $0 + 1)], radix: 16)! })
    }
}
