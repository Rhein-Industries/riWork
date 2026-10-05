import Foundation

/// An sRGB color. It is created only from the strict `#rrggbb` form, so a peer cannot smuggle in anything else.
public struct RGB: Sendable, Hashable {
    public let red: UInt8, green: UInt8, blue: UInt8
    public init(red: UInt8, green: UInt8, blue: UInt8) { self.red = red; self.green = green; self.blue = blue }
    /// `0xrrggbb`.
    public init(_ value: UInt32) { self.init(red: UInt8((value >> 16) & 255), green: UInt8((value >> 8) & 255), blue: UInt8(value & 255)) }
    /// Exactly `#` and six hex digits, in either case. No shorthand, no alpha, no whitespace.
    public init?(hex: String) {
        let bytes = Array(hex.utf8)
        guard bytes.count == 7, bytes[0] == UInt8(ascii: "#") else { return nil }
        var value: UInt32 = 0
        for byte in bytes[1...] {
            let digit: UInt32
            switch byte {
            case UInt8(ascii: "0")...UInt8(ascii: "9"): digit = UInt32(byte - UInt8(ascii: "0"))
            case UInt8(ascii: "a")...UInt8(ascii: "f"): digit = UInt32(byte - UInt8(ascii: "a")) + 10
            case UInt8(ascii: "A")...UInt8(ascii: "F"): digit = UInt32(byte - UInt8(ascii: "A")) + 10
            default: return nil
            }
            value = value << 4 | digit
        }
        self.init(value)
    }
    public var value: UInt32 { UInt32(red) << 16 | UInt32(green) << 8 | UInt32(blue) }
    /// Lowercase `#rrggbb`.
    public var hex: String { String(format: "#%06x", value) }
    /// WCAG 2.x relative luminance.
    public var relativeLuminance: Double {
        func channel(_ byte: UInt8) -> Double {
            let c = Double(byte) / 255
            return c <= 0.03928 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
    }
    /// WCAG contrast ratio, 1 (identical) to 21 (black on white).
    public func contrast(with other: RGB) -> Double {
        let a = relativeLuminance, b = other.relativeLuminance
        return (max(a, b) + 0.05) / (min(a, b) + 0.05)
    }
}

/// The desktop's UI palette (`src/theme.rs` `Palette`), in the desktop's own names.
public struct DesktopPalette: Sendable, Equatable {
    public let bg: RGB, panel: RGB, panelActive: RGB, divider: RGB, cyan: RGB, magenta: RGB, gold: RGB, text: RGB, muted: RGB
    public init(bg: RGB, panel: RGB, panelActive: RGB, divider: RGB, cyan: RGB, magenta: RGB, gold: RGB, text: RGB, muted: RGB) {
        self.bg = bg; self.panel = panel; self.panelActive = panelActive; self.divider = divider
        self.cyan = cyan; self.magenta = magenta; self.gold = gold; self.text = text; self.muted = muted
    }
}

/// Terminal colors: background, foreground and the sixteen ANSI colors.
public struct TerminalColors: Sendable, Equatable {
    public static let paletteCount = 16
    public let background: RGB, foreground: RGB
    public let palette: [RGB]
    public init(background: RGB, foreground: RGB, palette: [RGB]) { self.background = background; self.foreground = foreground; self.palette = palette }
}

public enum AppearanceError: Error, Equatable, LocalizedError, Sendable {
    /// `v` is missing or is not 1: a newer desktop than this app understands.
    case unsupportedVersion
    /// A field is missing or has the wrong shape. The text names the field, never the value.
    case invalid(String)
    public var errorDescription: String? {
        switch self {
        case .unsupportedVersion: "The desktop's appearance format is not supported by this app."
        case .invalid(let field): "The desktop sent an invalid appearance (\(field))."
        }
    }
}

/// What `appearance.get` returns: the desktop's current colors.
///
/// `{"v":1,"updated_at":<unix>,"dark":<bool>,"palette":{…9 × "#rrggbb"},"terminal":{"background","foreground","palette":[16 × "#rrggbb"]},"native":true,"mic":true}`
/// with `terminal`, `native` and `mic` optional. Parsing is strict about what it uses; unknown extra fields are ignored.
public struct DesktopAppearance: Sendable, Equatable, Codable {
    public static let version = 1
    public let updatedAt: UInt64
    public let dark: Bool
    public let palette: DesktopPalette
    public let terminal: TerminalColors?
    /// The desktop uses its Native skin: the interface is drawn the native way (system font, sentence case, glass). A desktop
    /// from before the flag never sends it, and one with another theme leaves it out; both mean the terminal look.
    public let native: Bool
    /// The desktop's dictation setting is on: the phone shows its mics. Sent only while it is on; a desktop from before the setting
    /// never sends it, and both mean off.
    public let mic: Bool
    public init(updatedAt: UInt64, dark: Bool, palette: DesktopPalette, terminal: TerminalColors? = nil, native: Bool = false, mic: Bool = false) {
        self.updatedAt = updatedAt; self.dark = dark; self.palette = palette; self.terminal = terminal; self.native = native; self.mic = mic
    }

    public init(json: JSONValue) throws {
        guard case .object = json else { throw AppearanceError.invalid("result") }
        guard case .number(let version) = json["v"], version == Double(Self.version) else { throw AppearanceError.unsupportedVersion }
        switch json["updated_at"] {
        case .null: updatedAt = 0
        case .number(let seconds) where seconds.isFinite && seconds >= 0 && seconds < 1e15 && seconds.rounded() == seconds: updatedAt = UInt64(seconds)
        default: throw AppearanceError.invalid("updated_at")
        }
        guard case .bool(let dark) = json["dark"] else { throw AppearanceError.invalid("dark") }
        self.dark = dark
        func color(_ value: JSONValue, _ name: String) throws -> RGB {
            guard let hex = value.string, let color = RGB(hex: hex) else { throw AppearanceError.invalid(name) }
            return color
        }
        let p = json["palette"]
        guard case .object = p else { throw AppearanceError.invalid("palette") }
        palette = DesktopPalette(
            bg: try color(p["bg"], "palette.bg"), panel: try color(p["panel"], "palette.panel"),
            panelActive: try color(p["panel_active"], "palette.panel_active"), divider: try color(p["divider"], "palette.divider"),
            cyan: try color(p["cyan"], "palette.cyan"), magenta: try color(p["magenta"], "palette.magenta"),
            gold: try color(p["gold"], "palette.gold"), text: try color(p["text"], "palette.text"), muted: try color(p["muted"], "palette.muted"))
        let t = json["terminal"]
        switch t {
        case .null: terminal = nil
        case .object:
            guard case .array(let list) = t["palette"], list.count == TerminalColors.paletteCount else { throw AppearanceError.invalid("terminal.palette") }
            terminal = TerminalColors(
                background: try color(t["background"], "terminal.background"),
                foreground: try color(t["foreground"], "terminal.foreground"),
                palette: try list.enumerated().map { try color($0.element, "terminal.palette[\($0.offset)]") })
        default: throw AppearanceError.invalid("terminal")
        }
        switch json["native"] {
        case .null: native = false
        case .bool(let value): native = value
        default: throw AppearanceError.invalid("native")
        }
        switch json["mic"] {
        case .null: mic = false
        case .bool(let value): mic = value
        default: throw AppearanceError.invalid("mic")
        }
    }

    /// The wire shape, used for persistence so what is stored is validated by the same parser that reads the network.
    public var json: JSONValue {
        var fields: [String: JSONValue] = [
            "v": .number(Double(Self.version)), "updated_at": .number(Double(updatedAt)), "dark": .bool(dark),
            "palette": .object([
                "bg": .string(palette.bg.hex), "panel": .string(palette.panel.hex), "panel_active": .string(palette.panelActive.hex),
                "divider": .string(palette.divider.hex), "cyan": .string(palette.cyan.hex), "magenta": .string(palette.magenta.hex),
                "gold": .string(palette.gold.hex), "text": .string(palette.text.hex), "muted": .string(palette.muted.hex)])
        ]
        if let terminal {
            fields["terminal"] = .object(["background": .string(terminal.background.hex), "foreground": .string(terminal.foreground.hex),
                                          "palette": .array(terminal.palette.map { .string($0.hex) })])
        }
        if native { fields["native"] = .bool(true) }
        if mic { fields["mic"] = .bool(true) }
        return .object(fields)
    }
    public init(from decoder: any Decoder) throws { try self.init(json: try JSONValue(from: decoder)) }
    public func encode(to encoder: any Encoder) throws { try json.encode(to: encoder) }

    /// The same colors, skin and mic setting, whatever the publication time. A refresh that only bumps `updated_at` changes nothing
    /// on screen; turning the mic setting on or off alone is a change.
    public func sameLook(as other: DesktopAppearance) -> Bool {
        dark == other.dark && palette == other.palette && terminal == other.terminal && native == other.native && mic == other.mic
    }
}

extension RemoteTransport {
    /// `appearance.get`: the desktop's current colors. `not_found` means the desktop has not published any yet; an older
    /// desktop answers `invalid_request` "unsupported RPC method" (see `RemoteError.isUnsupportedMethod`).
    public func appearance(id: String = UUID().uuidString.lowercased()) async throws -> DesktopAppearance {
        try DesktopAppearance(json: try await request(method: "appearance.get", params: [:], id: id))
    }
}
