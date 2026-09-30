import Foundation

/// A terminal color as the shell asked for it. Palette entries stay symbolic, so a new synced palette re-colors an
/// already parsed screen without parsing it again.
public enum TerminalColor: Sendable, Hashable {
    /// The terminal's own foreground or background (SGR 39 / 49, or nothing set).
    case `default`
    /// 0-7 the ANSI colors, 8-15 their bright forms, 16-231 the 6x6x6 cube, 232-255 the gray ramp.
    case indexed(UInt8)
    case rgb(RGB)
}

/// Text attributes that change how a cell looks but never how wide it is.
public struct TextAttributes: OptionSet, Sendable, Hashable {
    public let rawValue: UInt8
    public init(rawValue: UInt8) { self.rawValue = rawValue }
    public static let bold = TextAttributes(rawValue: 1 << 0)
    public static let dim = TextAttributes(rawValue: 1 << 1)
    public static let italic = TextAttributes(rawValue: 1 << 2)
    public static let underline = TextAttributes(rawValue: 1 << 3)
    public static let inverse = TextAttributes(rawValue: 1 << 4)
    public static let strikethrough = TextAttributes(rawValue: 1 << 5)
    /// SGR 8: the text is there but not shown.
    public static let hidden = TextAttributes(rawValue: 1 << 6)
}

/// Everything SGR can say about a cell.
public struct CellStyle: Sendable, Hashable {
    public var foreground: TerminalColor = .default
    public var background: TerminalColor = .default
    public var attributes: TextAttributes = []
    public init(foreground: TerminalColor = .default, background: TerminalColor = .default, attributes: TextAttributes = []) {
        self.foreground = foreground; self.background = background; self.attributes = attributes
    }
    public static let plain = CellStyle()
    public var isPlain: Bool { self == .plain }
    /// Whether an empty cell with this style can be seen (a filled background, an inverted block, a line through it).
    public var paintsBlank: Bool {
        background != .default || attributes.contains(.inverse) || attributes.contains(.underline) || attributes.contains(.strikethrough)
    }
}

/// Reads the parameters of one SGR sequence (`ESC [ … m`) into a style. Malformed or unknown parameters are skipped;
/// nothing here can fail or trap.
public enum SGR {
    /// Parameters are separated by `;`; a parameter may carry `:` sub-values (`38:2::255:0:0`, `4:3`). An empty value is `-1`.
    public typealias Parameter = [Int]
    public static let maximumParameters = 64

    /// The parameters of `bytes` (the text between `[` and `m`), or nil when it holds anything but digits, `;` and `:`.
    public static func parameters(_ bytes: ArraySlice<UInt8>) -> [Parameter]? {
        var result: [Parameter] = []
        var current: Parameter = []
        var value = -1
        var digits = 0
        func closeValue() {
            current.append(digits > 6 ? -1 : value)
            value = -1; digits = 0
        }
        for byte in bytes {
            switch byte {
            case 0x30...0x39:
                digits += 1
                if digits <= 6 { value = max(value, 0) * 10 + Int(byte - 0x30) }
            case 0x3A: closeValue()
            case 0x3B:
                closeValue()
                if result.count < maximumParameters { result.append(current) }
                current = []
            default: return nil
            }
        }
        closeValue()
        if result.count < maximumParameters { result.append(current) }
        return result
    }

    /// Applies the parameters to `style`. An empty list is a reset (`ESC [ m`).
    public static func apply(_ parameters: [Parameter], to style: inout CellStyle) {
        if parameters.isEmpty { style = .plain; return }
        var i = 0
        while i < parameters.count {
            let group = parameters[i]
            let code = group.first.map { $0 < 0 ? 0 : $0 } ?? 0
            i += 1
            switch code {
            case 0: style = .plain
            case 1: style.attributes.insert(.bold)
            case 2: style.attributes.insert(.dim)
            case 3: style.attributes.insert(.italic)
            case 4:
                // `4:0` switches underline off, every other style (`4:1` … `4:5`) is some underline.
                if group.count > 1, group[1] == 0 { style.attributes.remove(.underline) } else { style.attributes.insert(.underline) }
            case 7: style.attributes.insert(.inverse)
            case 8: style.attributes.insert(.hidden)
            case 9: style.attributes.insert(.strikethrough)
            case 21: style.attributes.insert(.underline)
            case 22: style.attributes.subtract([.bold, .dim])
            case 23: style.attributes.remove(.italic)
            case 24: style.attributes.remove(.underline)
            case 27: style.attributes.remove(.inverse)
            case 28: style.attributes.remove(.hidden)
            case 29: style.attributes.remove(.strikethrough)
            case 30...37: style.foreground = .indexed(UInt8(code - 30))
            case 39: style.foreground = .default
            case 40...47: style.background = .indexed(UInt8(code - 40))
            case 49: style.background = .default
            case 90...97: style.foreground = .indexed(UInt8(code - 90 + 8))
            case 100...107: style.background = .indexed(UInt8(code - 100 + 8))
            case 38, 48, 58:
                let color: TerminalColor?
                if group.count > 1 {
                    // Colon form: everything is in this parameter.
                    color = extended(Array(group.dropFirst()))
                } else {
                    // Semicolon form: the mode and its values are the parameters that follow.
                    let mode = i < parameters.count ? parameters[i].first ?? -1 : -1
                    let needed = mode == 5 ? 1 : (mode == 2 ? 3 : 0)
                    let available = min(needed, max(0, parameters.count - i - 1))
                    var values = [mode]
                    for offset in 0..<available { values.append(parameters[i + 1 + offset].first ?? -1) }
                    color = available == needed ? extended(values) : nil
                    // A spec that is cut short swallows what is left; an unknown mode only itself.
                    i = min(parameters.count, i + 1 + (mode == 5 || mode == 2 ? available : 0))
                }
                // 58 is the underline color, which this terminal does not draw.
                if let color { if code == 38 { style.foreground = color } else if code == 48 { style.background = color } }
            default: break   // blink, overline, fonts and anything unknown do not change how a cell looks here
            }
        }
    }

    /// `5;n` or `2;r;g;b` (with the optional colorspace slot `2;;r;g;b` of the colon form). Nil when out of range or unknown.
    static func extended(_ values: [Int]) -> TerminalColor? {
        guard let mode = values.first else { return nil }
        func byte(_ value: Int) -> UInt8? { (0...255).contains(value) ? UInt8(value) : nil }
        switch mode {
        case 5:
            guard values.count >= 2, let n = byte(values[1]) else { return nil }
            return .indexed(n)
        case 2:
            // `38:2:r:g:b` and `38:2:cs:r:g:b`: the last three are the color.
            guard values.count >= 4 else { return nil }
            let rgb = values.suffix(3)
            guard values.count <= 5, let r = byte(rgb[rgb.startIndex]), let g = byte(rgb[rgb.startIndex + 1]), let b = byte(rgb[rgb.startIndex + 2]) else { return nil }
            return .rgb(RGB(red: r, green: g, blue: b))
        default: return nil
        }
    }
}

/// The colors a screen is drawn with: the terminal's foreground and background and its 16 ANSI colors.
public struct TerminalRenderColors: Sendable, Equatable {
    public let foreground: RGB
    public let background: RGB
    public let ansi: [RGB]
    /// Draw bold text in the bright variant of colors 0-7, as Ghostty does with `bold-is-bright`. Off by default, like Ghostty.
    public let boldIsBright: Bool
    /// How much of the background a dim (faint) color keeps of the foreground, like Ghostty's `faint-opacity`.
    public static let faintOpacity = 0.5

    public init(foreground: RGB, background: RGB, ansi: [RGB], boldIsBright: Bool = false) {
        self.foreground = foreground; self.background = background
        self.ansi = ansi.count == 16 ? ansi : Self.fallbackAnsi(dark: true)
        self.boldIsBright = boldIsBright
    }

    /// xterm's 6x6x6 color cube steps.
    static let cubeLevels: [UInt8] = [0, 95, 135, 175, 215, 255]

    /// The color of palette entry `index`: 0-15 from the terminal's palette, 16-255 the fixed xterm cube and gray ramp.
    public func palette(_ index: UInt8) -> RGB {
        switch index {
        case 0..<16: return ansi[Int(index)]
        case 16..<232:
            let n = Int(index) - 16
            return RGB(red: Self.cubeLevels[n / 36], green: Self.cubeLevels[(n / 6) % 6], blue: Self.cubeLevels[n % 6])
        default:
            let gray = UInt8(8 + 10 * (Int(index) - 232))
            return RGB(red: gray, green: gray, blue: gray)
        }
    }

    /// The final colors of a cell. `background` is nil when the cell keeps the terminal's own background (nothing to paint).
    public func resolve(_ style: CellStyle) -> (foreground: RGB, background: RGB?) {
        var fg: RGB
        switch style.foreground {
        case .default: fg = foreground
        case .indexed(let index):
            // Bold picks the bright twin of the eight base colors when asked to.
            fg = palette(boldIsBright && style.attributes.contains(.bold) && index < 8 ? index + 8 : index)
        case .rgb(let color): fg = color
        }
        var bg: RGB?
        switch style.background {
        case .default: bg = nil
        case .indexed(let index): bg = palette(index)
        case .rgb(let color): bg = color
        }
        if style.attributes.contains(.inverse) {
            // The text takes the cell's background (or the terminal's), the cell takes the text color.
            let text = bg ?? background
            bg = fg
            fg = text
        }
        let under = bg ?? background
        if style.attributes.contains(.dim) { fg = fg.blended(over: under, opacity: Self.faintOpacity) }
        if style.attributes.contains(.hidden) { fg = under }
        return (fg, bg)
    }

    // MARK: Built-in palettes, for a desktop that has not published terminal colors

    static let darkAnsi: [RGB] = [
        0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6,
        0x666666, 0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea
    ].map { RGB($0) }
    /// Gruvbox Light's darker variants, so every color reads on the light background.
    static let lightAnsi: [RGB] = [
        0x3c3836, 0x9d0006, 0x79740e, 0xb57614, 0x076678, 0x8f3f71, 0x427b58, 0x7c6f64,
        0x928374, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0x504945
    ].map { RGB($0) }
    public static func fallbackAnsi(dark: Bool) -> [RGB] { dark ? darkAnsi : lightAnsi }
}

extension RGB {
    /// This color at `opacity` over `other`.
    public func blended(over other: RGB, opacity: Double) -> RGB {
        func mix(_ a: UInt8, _ b: UInt8) -> UInt8 { UInt8(max(0, min(255, (Double(a) * opacity + Double(b) * (1 - opacity)).rounded()))) }
        return RGB(red: mix(red, other.red), green: mix(green, other.green), blue: mix(blue, other.blue))
    }
}
