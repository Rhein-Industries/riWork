import XCTest
@testable import RiWorkCore

private let paletteJSON = """
{"bg":"#090d14","panel":"#101720","panel_active":"#14212a","divider":"#253c45","cyan":"#55e6dc","magenta":"#ce78ef","gold":"#f4bf75","text":"#d3e1e6","muted":"#708993"}
"""
private let ansi = ["#000000", "#cc0000", "#00cc00", "#cccc00", "#0000cc", "#cc00cc", "#00cccc", "#cccccc",
                    "#555555", "#ff5555", "#55ff55", "#ffff55", "#5555ff", "#ff55ff", "#55ffff", "#ffffff"]
private func terminalJSON(palette: [String] = ansi, background: String = "#0b0f16", foreground: String = "#c8d3d8") -> String {
    "{\"background\":\"\(background)\",\"foreground\":\"\(foreground)\",\"palette\":[\(palette.map { "\"\($0)\"" }.joined(separator: ","))]}"
}
private func appearanceJSON(v: String = "1", updatedAt: String = "1790000000", dark: String = "true", palette: String = paletteJSON, terminal: String? = nil) -> String {
    "{\"v\":\(v),\"updated_at\":\(updatedAt),\"dark\":\(dark),\"palette\":\(palette)" + (terminal.map { ",\"terminal\":\($0)" } ?? "") + "}"
}
/// The document with `"native":<value>` added at the end.
private func withNative(_ document: String, _ value: String) -> String { String(document.dropLast()) + ",\"native\":\(value)}" }
private func parse(_ text: String) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)) }

final class AppearanceTests: XCTestCase {
    private func appearance(_ text: String) throws -> DesktopAppearance { try DesktopAppearance(json: try parse(text)) }
    private func assertInvalid(_ text: String, field: String? = nil, _ message: String = "", file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertThrowsError(try appearance(text), message, file: file, line: line) { error in
            guard let error = error as? AppearanceError, error != .unsupportedVersion else { return XCTFail("expected .invalid, got \(error). \(message)", file: file, line: line) }
            if let field { XCTAssertEqual(error, .invalid(field), message, file: file, line: line) }
        }
    }

    func testValidAppearanceWithAndWithoutTerminal() throws {
        let plain = try appearance(appearanceJSON())
        XCTAssertEqual(plain.updatedAt, 1_790_000_000)
        XCTAssertTrue(plain.dark)
        XCTAssertEqual(plain.palette.bg, RGB(0x090d14))
        XCTAssertEqual(plain.palette.panelActive, RGB(0x14212a))
        XCTAssertEqual(plain.palette.cyan.hex, "#55e6dc")
        XCTAssertEqual(plain.palette.muted.hex, "#708993")
        XCTAssertNil(plain.terminal)
        let full = try appearance(appearanceJSON(dark: "false", terminal: terminalJSON()))
        XCTAssertFalse(full.dark)
        XCTAssertEqual(full.terminal?.background.hex, "#0b0f16")
        XCTAssertEqual(full.terminal?.foreground.hex, "#c8d3d8")
        XCTAssertEqual(full.terminal?.palette.count, 16)
        XCTAssertEqual(full.terminal?.palette[15].hex, "#ffffff")
    }
    func testNullTerminalIsAbsentAndUnknownFieldsAreIgnored() throws {
        let value = try parse(appearanceJSON(terminal: "null").replacingOccurrences(of: "\"v\":1", with: "\"v\":1,\"future\":{\"x\":1}"))
        let parsed = try DesktopAppearance(json: value)
        XCTAssertNil(parsed.terminal)
    }
    func testUppercaseHexIsAcceptedAndNormalized() throws {
        let parsed = try appearance(appearanceJSON(palette: paletteJSON.replacingOccurrences(of: "#55e6dc", with: "#55E6DC")))
        XCTAssertEqual(parsed.palette.cyan.hex, "#55e6dc")
    }
    func testMissingUpdatedAtIsToleratedButAWrongOneIsNot() throws {
        XCTAssertEqual(try appearance(appearanceJSON().replacingOccurrences(of: "\"updated_at\":1790000000,", with: "")).updatedAt, 0)
        for bad in ["-1", "1.5", "\"1790000000\"", "true", "1e300"] { assertInvalid(appearanceJSON(updatedAt: bad), field: "updated_at", bad) }
    }
    func testUnknownOrMissingVersionIsRejectedAsUnsupported() throws {
        for v in ["2", "0", "1.5", "\"1\"", "null", "true", "-1"] {
            XCTAssertThrowsError(try appearance(appearanceJSON(v: v)), "v=\(v)") { XCTAssertEqual($0 as? AppearanceError, .unsupportedVersion, "v=\(v)") }
        }
        XCTAssertThrowsError(try appearance(appearanceJSON().replacingOccurrences(of: "\"v\":1,", with: ""))) { XCTAssertEqual($0 as? AppearanceError, .unsupportedVersion) }
    }
    func testHexIsStrict() throws {
        for bad in ["#fff", "ffffff", "#gggggg", "#ffffffff", " #ffffff", "#ffffff ", "#ffff ff", "#ffffé", "rgb(0,0,0)", "", "#", "0xffffff", "#ffffff\\n", "＃ffffff"] {
            assertInvalid(appearanceJSON(palette: paletteJSON.replacingOccurrences(of: "#090d14", with: bad)), field: "palette.bg", "bg=\(bad.debugDescription)")
        }
        for (key, other) in [("bg", "#090d14"), ("panel", "#101720"), ("panel_active", "#14212a"), ("divider", "#253c45"), ("cyan", "#55e6dc"),
                             ("magenta", "#ce78ef"), ("gold", "#f4bf75"), ("text", "#d3e1e6"), ("muted", "#708993")] {
            assertInvalid(appearanceJSON(palette: paletteJSON.replacingOccurrences(of: other, with: "#12345")), field: "palette.\(key)", key)
            // A number where a string belongs.
            assertInvalid(appearanceJSON(palette: paletteJSON.replacingOccurrences(of: "\"\(other)\"", with: "1193046")), field: "palette.\(key)", key)
        }
        XCTAssertNil(RGB(hex: "#12345g"))
        XCTAssertEqual(RGB(hex: "#0A1b2C")?.value, 0x0a1b2c)
    }
    func testEveryPaletteEntryIsRequired() throws {
        for key in ["bg", "panel", "panel_active", "divider", "cyan", "magenta", "gold", "text", "muted"] {
            guard case .object(var fields) = try parse(paletteJSON) else { return XCTFail() }
            fields[key] = nil
            guard case .object(var whole) = try parse(appearanceJSON()) else { return XCTFail() }
            whole["palette"] = .object(fields)
            XCTAssertThrowsError(try DesktopAppearance(json: .object(whole)), key) { XCTAssertEqual($0 as? AppearanceError, .invalid("palette.\(key)")) }
        }
        assertInvalid(appearanceJSON(palette: "[]"), field: "palette")
        assertInvalid(appearanceJSON(palette: "{}"), "empty palette")
        assertInvalid("{\"v\":1,\"dark\":true}", field: "palette")
    }
    func testDarkMustBeABoolean() {
        for bad in ["\"true\"", "1", "null"] { assertInvalid(appearanceJSON(dark: bad), field: "dark", bad) }
    }
    func testTerminalPaletteHasExactlySixteenColors() throws {
        for count in [0, 1, 8, 15, 17, 32] {
            let list = (0..<count).map { _ in "#123456" }
            assertInvalid(appearanceJSON(terminal: terminalJSON(palette: list)), field: "terminal.palette", "\(count) colors")
        }
        assertInvalid(appearanceJSON(terminal: terminalJSON(palette: Array(ansi.dropLast()) + ["#12345"])), field: "terminal.palette[15]")
        assertInvalid(appearanceJSON(terminal: terminalJSON(background: "blue")), field: "terminal.background")
        assertInvalid(appearanceJSON(terminal: terminalJSON(foreground: "#12")), field: "terminal.foreground")
        assertInvalid(appearanceJSON(terminal: "{\"background\":\"#000000\",\"foreground\":\"#ffffff\"}"), field: "terminal.palette")
        assertInvalid(appearanceJSON(terminal: "{\"background\":\"#000000\",\"foreground\":\"#ffffff\",\"palette\":\"#000000\"}"), field: "terminal.palette")
        assertInvalid(appearanceJSON(terminal: "\"none\""), field: "terminal")
        assertInvalid(appearanceJSON(terminal: "[]"), field: "terminal")
    }
    func testNonObjectResultIsRejected() {
        for text in ["[]", "\"x\"", "null", "3"] { XCTAssertThrowsError(try appearance(text), text) }
    }
    func testWireRoundTripAndCodable() throws {
        let original = try appearance(appearanceJSON(dark: "false", terminal: terminalJSON()))
        XCTAssertEqual(try DesktopAppearance(json: original.json), original)
        let data = try JSONEncoder().encode(original)
        XCTAssertEqual(try JSONDecoder().decode(DesktopAppearance.self, from: data), original)
        XCTAssertThrowsError(try JSONDecoder().decode(DesktopAppearance.self, from: Data("{\"v\":1}".utf8)), "stored garbage is rejected by the same parser")
    }
    func testSameLookIgnoresPublicationTime() throws {
        let a = try appearance(appearanceJSON(updatedAt: "100")), b = try appearance(appearanceJSON(updatedAt: "200"))
        XCTAssertNotEqual(a, b)
        XCTAssertTrue(a.sameLook(as: b))
        XCTAssertFalse(a.sameLook(as: try appearance(appearanceJSON(dark: "false"))))
        XCTAssertFalse(a.sameLook(as: try appearance(appearanceJSON(palette: paletteJSON.replacingOccurrences(of: "#55e6dc", with: "#55e6dd")))))
        XCTAssertFalse(a.sameLook(as: try appearance(appearanceJSON(terminal: terminalJSON()))))
    }
    func testTheNativeFlagIsOptionalAndOffWhenMissing() throws {
        // A desktop from before the flag, or one with another theme, sends none: the terminal look.
        XCTAssertFalse(try appearance(appearanceJSON()).native)
        let native = try appearance(withNative(appearanceJSON(terminal: terminalJSON()), "true"))
        XCTAssertTrue(native.native)
        XCTAssertFalse(try appearance(withNative(appearanceJSON(), "false")).native)
        XCTAssertFalse(try appearance(withNative(appearanceJSON(), "null")).native)
        for bad in ["\"true\"", "1", "{}", "[]"] { assertInvalid(withNative(appearanceJSON(), bad), field: "native", bad) }
    }
    func testTheNativeFlagIsKeptThroughStorageAndWrittenOnlyWhenSet() throws {
        let native = try appearance(withNative(appearanceJSON(), "true"))
        XCTAssertEqual(native.json["native"], .bool(true))
        XCTAssertEqual(try DesktopAppearance(json: native.json), native)
        XCTAssertEqual(try JSONDecoder().decode(DesktopAppearance.self, from: try JSONEncoder().encode(native)), native)
        // Off, the stored shape is exactly what it was before the flag existed.
        XCTAssertEqual(try appearance(appearanceJSON()).json["native"], .null)
    }
    func testSwitchingNativeIsANewLookEvenWithTheSameColors() throws {
        let plain = try appearance(appearanceJSON(updatedAt: "100")), native = try appearance(withNative(appearanceJSON(updatedAt: "100"), "true"))
        XCTAssertFalse(plain.sameLook(as: native))
        XCTAssertFalse(native.sameLook(as: plain))
        XCTAssertTrue(native.sameLook(as: try appearance(withNative(appearanceJSON(updatedAt: "200"), "true"))))
    }
    /// The document with `"mic":<value>` added at the end.
    private func withMic(_ document: String, _ value: String) -> String { String(document.dropLast()) + ",\"mic\":\(value)}" }
    func testTheMicSettingIsOptionalAndOffWhenMissing() throws {
        // A desktop from before the setting sends none, and one with the setting off leaves it out: off.
        XCTAssertFalse(try appearance(appearanceJSON()).mic)
        XCTAssertTrue(try appearance(withMic(appearanceJSON(terminal: terminalJSON()), "true")).mic)
        XCTAssertFalse(try appearance(withMic(appearanceJSON(), "false")).mic)
        XCTAssertFalse(try appearance(withMic(appearanceJSON(), "null")).mic)
        let both = try appearance(withMic(withNative(appearanceJSON(), "true"), "true"))
        XCTAssertTrue(both.native && both.mic, "independent of the skin")
        XCTAssertFalse(try appearance(withNative(appearanceJSON(), "true")).mic)
        for bad in ["\"true\"", "1", "0", "{}", "[]"] { assertInvalid(withMic(appearanceJSON(), bad), field: "mic", bad) }
    }
    func testTheMicSettingIsKeptThroughStorageAndWrittenOnlyWhenOn() throws {
        let on = try appearance(withMic(appearanceJSON(), "true"))
        XCTAssertEqual(on.json["mic"], .bool(true))
        XCTAssertEqual(try DesktopAppearance(json: on.json), on)
        XCTAssertEqual(try JSONDecoder().decode(DesktopAppearance.self, from: try JSONEncoder().encode(on)), on)
        XCTAssertEqual(try appearance(appearanceJSON()).json["mic"], .null, "off, the stored shape is what it was before the setting")
    }
    func testTurningTheMicSettingOnOrOffAloneIsAChange() throws {
        let off = try appearance(appearanceJSON(updatedAt: "100")), on = try appearance(withMic(appearanceJSON(updatedAt: "100"), "true"))
        XCTAssertFalse(off.sameLook(as: on))
        XCTAssertFalse(on.sameLook(as: off))
        XCTAssertTrue(on.sameLook(as: try appearance(withMic(appearanceJSON(updatedAt: "200"), "true"))))
    }
    func testContrastRatioMatchesWCAG() {
        XCTAssertEqual(RGB(0x000000).contrast(with: RGB(0xffffff)), 21, accuracy: 0.001)
        XCTAssertEqual(RGB(0x123456).contrast(with: RGB(0x123456)), 1, accuracy: 0.001)
        XCTAssertEqual(RGB(0x777777).contrast(with: RGB(0xffffff)), 4.478, accuracy: 0.01)
        XCTAssertEqual(RGB(0xffffff).contrast(with: RGB(0x777777)), RGB(0x777777).contrast(with: RGB(0xffffff)), "symmetric")
    }
}
