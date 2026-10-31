import Foundation

/// The same fixed color schemes used by the desktop. Native appearance and
/// following a desktop are device policies, so they are not catalog entries.
public struct PhonePalette: Identifiable, Sendable, Equatable {
    public let id: String
    public let title: String
    public let detail: String
    public let dark: Bool
    public let palette: DesktopPalette
    public let terminal: TerminalColors
    /// Errors and failures: the palette's `danger`; a catalog from before it gives its `error`, else the terminal's red.
    public let error: RGB
    /// Agents at work: the palette's `working`; a catalog from before it gives the accent (`cyan`).
    public let working: RGB
}

public enum PhonePaletteCatalog {
    public enum Invalid: Error, LocalizedError, Sendable {
        case version, palette(String)
        public var errorDescription: String? {
            switch self {
            case .version: "This color palette catalog is not supported."
            case .palette(let id): "The color palette catalog contains an invalid palette (\(id))."
            }
        }
    }

    /// The app supplies the bundled canonical JSON; Core never chooses a bundle
    /// or imports UIKit. Invalid resources cannot install a partial palette.
    public static func decode(data: Data) throws -> [PhonePalette] {
        let catalog = try JSONDecoder().decode(Catalog.self, from: data)
        guard catalog.schema == 1 else { throw Invalid.version }
        var seen = Set<String>()
        return try catalog.palettes.map { entry in
            guard !entry.id.isEmpty, entry.id != "native", entry.id != "desktop", entry.id != "ghostty",
                  !entry.label.isEmpty, seen.insert(entry.id).inserted,
                  entry.terminal.palette.count == TerminalColors.paletteCount else { throw Invalid.palette(entry.id) }
            func rgb(_ packed: UInt32) throws -> RGB {
                guard packed <= 0xffffff else { throw Invalid.palette(entry.id) }
                return RGB(packed)
            }
            let p = entry.palette, t = entry.terminal
            return try PhonePalette(id: entry.id, title: entry.label, detail: entry.description, dark: entry.dark,
                palette: DesktopPalette(bg: rgb(p.bg), panel: rgb(p.panel), panelActive: rgb(p.panelActive),
                    divider: rgb(p.divider), cyan: rgb(p.cyan), magenta: rgb(p.magenta), gold: rgb(p.gold), text: rgb(p.text), muted: rgb(p.muted)),
                terminal: TerminalColors(background: rgb(t.background), foreground: rgb(t.foreground), palette: t.palette.map(rgb)),
                error: rgb(p.danger ?? p.error ?? t.palette[1]), working: rgb(p.working ?? p.cyan))
        }
    }

    private struct Catalog: Decodable {
        let schema: Int
        let palettes: [Entry]
    }
    private struct Entry: Decodable {
        let id, label, description: String
        let dark: Bool
        let palette: Palette
        let terminal: Terminal
    }
    private struct Palette: Decodable {
        let bg, panel, panelActive, divider, cyan, magenta, gold, text, muted: UInt32
        /// The desktop's names for the state colors (`src/theme.rs` `Palette`); `error` is the name before `danger`.
        let danger, error, working: UInt32?
    }
    private struct Terminal: Decodable {
        let background, foreground: UInt32
        let palette: [UInt32]
    }
}
