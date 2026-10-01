import XCTest
import CryptoKit
@testable import RiWorkCore

/// The link extension's frames: a reply's plaintext is JSON text, or `0x01 || length || raw deflate` of it. The vectors in
/// `Fixtures/link.json` were made by Python's zlib and by the desktop's own flate2 (`remote/fixtures/generate_link.py`); Rust reads the
/// same file.
final class LinkTests: XCTestCase {
    private func fixture() throws -> JSONValue {
        #if SWIFT_PACKAGE
        let bundle = Bundle.module
        #else
        let bundle = Bundle(for: Self.self)
        #endif
        let url = try XCTUnwrap(bundle.url(forResource: "link", withExtension: "json", subdirectory: "Fixtures") ?? bundle.url(forResource: "link", withExtension: "json"))
        return try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: url))
    }
    private func bytes(_ hex: String) -> Data {
        var data = Data(); var index = hex.startIndex
        while index < hex.endIndex { let next = hex.index(index, offsetBy: 2); data.append(UInt8(hex[index ..< next], radix: 16)!); index = next }
        return data
    }

    func testEveryPublishedFrameDecodesToTheJSONItWasMadeFrom() throws {
        let vectors = try fixture()["vectors"].array
        XCTAssertGreaterThanOrEqual(vectors.count, 5)
        var compressed = 0
        for vector in vectors {
            let name = vector["name"].string ?? "?"
            let (json, wasCompressed) = try LinkFrame.decode(bytes(vector["frame_hex"].string!))
            XCTAssertEqual(String(data: json, encoding: .utf8), vector["json"].string, name)
            guard case .number = try JSONDecoder().decode(JSONValue.self, from: json)["server_ms"] else { XCTFail("\(name) has no server_ms"); continue }
            if wasCompressed { compressed += 1 }
        }
        XCTAssertEqual(compressed, vectors.count - 1, "all but the plain one are deflated, including the one the connector's flate2 wrote")
    }
    func testFramesThatLieOrAreBrokenAreRefused() throws {
        let refused = try fixture()["refused"].array
        XCTAssertGreaterThanOrEqual(refused.count, 6)
        for vector in refused {
            XCTAssertThrowsError(try LinkFrame.decode(bytes(vector["frame_hex"].string!)), vector["name"].string ?? "?")
        }
        XCTAssertThrowsError(try LinkFrame.decode(Data()))
    }
    func testAPlainTextIsLeftAloneWhateverItStartsWith() throws {
        let plain = Data(#"{"v":1,"type":"response","id":"x","ok":true,"result":{}}"#.utf8)
        let (json, compressed) = try LinkFrame.decode(plain)
        XCTAssertEqual(json, plain); XCTAssertFalse(compressed)
        // White space before the brace is JSON too; any other marker is a format this phone does not know.
        let spaced = Data("\n  ".utf8) + plain
        XCTAssertEqual(try LinkFrame.decode(spaced).json, spaced)
        for marker: UInt8 in [0x00, 0x02, 0x7A, 0xFF] { XCTAssertThrowsError(try LinkFrame.decode(Data([marker]) + plain), "marker \(marker)") }
    }
    func testThePhonesOwnEncoderAndDecoderAgreeAndKeepTheMarker() throws {
        let text = String(repeating: "\u{1B}[38;5;71m│ \u{1B}[0mfn handler(ctx: &mut Context) -> Result<(), Error> {}\n", count: 200)
        let json = Data(#"{"v":1,"type":"response","id":"x","ok":true,"result":{"output":"\#(text.replacingOccurrences(of: "\u{1B}", with: "\\u001b").replacingOccurrences(of: "\n", with: "\\n"))"},"server_ms":4}"#.utf8)
        let frame = try XCTUnwrap(LinkFrame.compressedFrame(for: json))
        XCTAssertEqual(frame.first, LinkFrame.deflateMarker)
        XCTAssertLessThan(frame.count * 10, json.count, "styled text shrinks by an order of magnitude")
        let (back, compressed) = try LinkFrame.decode(frame)
        XCTAssertEqual(back, json); XCTAssertTrue(compressed)
        // A body that does not shrink is not framed.
        XCTAssertNil(LinkFrame.compressedFrame(for: Data(#"{"v":1}"#.utf8)))
    }
    func testASizeAboveTheLimitIsRefusedBeforeAnythingIsInflated() {
        var frame = Data([LinkFrame.deflateMarker])
        withUnsafeBytes(of: UInt32(LinkFrame.maximumInflatedBytes + 1).bigEndian) { frame.append(contentsOf: $0) }
        // A stream that is valid and tiny, so that only the declared size can be what is refused.
        frame.append(Data([0x4B, 0x4C, 0x04, 0x00]))   // raw deflate of "aa"
        XCTAssertNoThrow(try LinkFrame.inflate(Data([0x4B, 0x4C, 0x04, 0x00]), expecting: 2))
        XCTAssertThrowsError(try LinkFrame.decode(frame)) { error in
            XCTAssertTrue("\(error)".contains("unusable size"), "\(error)")
        }
    }

    // MARK: Opening sealed frames

    private func sealed(_ plaintext: Data, counter: UInt64, cipher: SessionCipher) throws -> JSONValue {
        let nonce = try ChaChaPoly.Nonce(data: Data(repeating: 0, count: 4) + SessionCipher.counterBytes(counter))
        let box = try ChaChaPoly.seal(plaintext, using: cipher.d2c, nonce: nonce, authenticating: cipher.aad(direction: "d2c", counter: counter))
        return .object(["v": .number(1), "type": .string("encrypted"), "session_id": .string(Base64URL.encode(cipher.sessionID)), "direction": .string("d2c"),
                        "counter": .string(String(counter)), "ciphertext": .string(Base64URL.encode(box.ciphertext + box.tag))])
    }
    func testASealedCompressedReplyOpensToTheSameValueAsAPlainOneAndReportsBothSizes() throws {
        let key = SymmetricKey(data: Data(repeating: 9, count: 32))
        var opener = SessionCipher(version: 1, sessionID: Data(repeating: 1, count: 16), c2d: key, d2c: key)
        let line = #"\u001b[38;5;71mline of styled output\u001b[0m\n"#
        let json = Data(#"{"v":1,"type":"response","id":"x","ok":true,"result":{"output":"\#(String(repeating: line, count: 300))"},"server_ms":12}"#.utf8)
        let frame = try XCTUnwrap(LinkFrame.compressedFrame(for: json))
        let packed = try opener.openFrame(sealed(frame, counter: 0, cipher: opener))
        XCTAssertTrue(packed.compressed)
        XCTAssertEqual(packed.jsonBytes, json.count)
        XCTAssertEqual(packed.sealedBytes, frame.count + 16)
        XCTAssertLessThan(packed.sealedBytes * 8, packed.jsonBytes)
        XCTAssertEqual(packed.payload["server_ms"], .number(12))
        let plain = try opener.openFrame(sealed(json, counter: 1, cipher: opener))
        XCTAssertFalse(plain.compressed)
        XCTAssertEqual(plain.payload, packed.payload, "the same reply either way")
        XCTAssertEqual(plain.sealedBytes, json.count + 16)
        XCTAssertEqual(opener.receiveCounter, 2)
    }
    func testACompressedFrameThatLiesDoesNotAdvanceTheCounter() throws {
        let key = SymmetricKey(data: Data(repeating: 9, count: 32))
        var opener = SessionCipher(version: 1, sessionID: Data(repeating: 1, count: 16), c2d: key, d2c: key)
        var frame = try XCTUnwrap(LinkFrame.compressedFrame(for: Data(#"{"v":1,"type":"response","id":"x","ok":true,"result":{"o":"\#(String(repeating: "abc", count: 400))"}}"#.utf8)))
        frame[4] = frame[4] &+ 1   // declares one byte more than there is
        XCTAssertThrowsError(try opener.openFrame(sealed(frame, counter: 0, cipher: opener)))
        XCTAssertEqual(opener.receiveCounter, 0, "a frame that is refused does not use up its counter")
    }

    // MARK: What a desktop announces

    func testReadyFeaturesAreReadAndAnOlderReadyOffersNothing() {
        let none = DesktopFeatures(ready: .object(["v": .number(1), "type": .string("ready")]))
        XCTAssertEqual(none, DesktopFeatures())
        XCTAssertFalse(none.deflate)
        XCTAssertEqual(none.historyMaximumLines, 1000, "pages were 1000 lines at most before 2026-10-01")
        let ready = JSONValue.object(["features": .object(["deflate": .object(["min_bytes": .number(2048), "max_inflated": .number(2_097_152)]), "history_max_lines": .number(5000)])])
        let features = DesktopFeatures(ready: ready)
        XCTAssertTrue(features.deflate)
        XCTAssertEqual(features.minimumCompressBytes, 2048)
        XCTAssertEqual(features.maximumInflatedBytes, 2_097_152)
        XCTAssertEqual(features.historyMaximumLines, 5000)
    }
    func testNonsenseFeaturesAreIgnoredAndLimitsAreClamped() {
        let silly = DesktopFeatures(ready: .object(["features": .object(["deflate": .string("yes"), "history_max_lines": .number(-3)])]))
        XCTAssertFalse(silly.deflate)
        XCTAssertEqual(silly.historyMaximumLines, 1000)
        let greedy = DesktopFeatures(ready: .object(["features": .object(["deflate": .object(["max_inflated": .number(900_000_000)]), "history_max_lines": .number(90_000)])]))
        XCTAssertEqual(greedy.maximumInflatedBytes, LinkFrame.maximumInflatedBytes, "never more than the phone is willing to inflate")
        XCTAssertEqual(greedy.historyMaximumLines, HistoryLimits.maximumPageLines)
        let timid = DesktopFeatures(ready: .object(["features": .object(["history_max_lines": .number(200)])]))
        XCTAssertEqual(timid.historyMaximumLines, 1000, "the old limit is the floor")
    }
    func testLinkConfigureIsValidatedBeforeAnythingIsSealed() throws {
        let id = UUID().uuidString.lowercased()
        try RequestValidation.validate(method: "link.configure", params: ["compression": .string("deflate")], id: id)
        try RequestValidation.validate(method: "link.configure", params: ["compression": .string("none")], id: id)
        try RequestValidation.validate(method: "link.configure", params: [:], id: id)
        for bad: [String: JSONValue] in [["compression": .string("zstd")], ["compression": .bool(true)], ["level": .number(9)]] {
            XCTAssertThrowsError(try RequestValidation.validate(method: "link.configure", params: bad, id: id), "\(bad)")
        }
    }
    func testHistoryPagesOfUpToFiveThousandLinesAreValidButALiveReadStillStopsAtTwoThousand() throws {
        let id = UUID().uuidString.lowercased(), shell = "44444444-4444-4444-8444-444444444444"
        for lines in [1.0, 1000, 2216, 5000] {
            try RequestValidation.validate(method: "shell.history", params: ["shell_id": .string(shell), "end": .number(0), "lines": .number(lines)], id: id)
        }
        XCTAssertThrowsError(try RequestValidation.validate(method: "shell.history", params: ["shell_id": .string(shell), "end": .number(0), "lines": .number(5001)], id: id))
        try RequestValidation.validate(method: "shell.output", params: ["shell_id": .string(shell), "lines": .number(2000)], id: id)
        XCTAssertThrowsError(try RequestValidation.validate(method: "shell.output", params: ["shell_id": .string(shell), "lines": .number(2001)], id: id))
    }
}
