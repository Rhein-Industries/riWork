import Foundation

/// A safe area in points, as the system reports it. Left and right are physical (a display corner does not mirror in
/// right-to-left languages).
public struct KeyBarInsets: Sendable, Equatable {
    public var left: Double, bottom: Double, right: Double
    public init(left: Double = 0, bottom: Double = 0, right: Double = 0) { self.left = left; self.bottom = bottom; self.right = right }
    public static let zero = KeyBarInsets()
}

/// The padding at the two ends of the key row. The bar itself never moves or grows: it stays where iOS puts it.
public struct KeyBarPadding: Sendable, Equatable {
    public var left: Double, right: Double
    public init(left: Double = 0, right: Double = 0) { self.left = left; self.right = right }
    public static let zero = KeyBarPadding()
}

/// Where the key bar (the keyboard's input accessory view) sits.
public enum KeyBarPosition: Sendable, Equatable {
    /// On top of the software keyboard. The keyboard already lines up with the display, so only the side safe area applies.
    case aboveKeyboard
    /// Alone at the bottom edge of the screen: a hardware keyboard is attached, so iOS hides the software keyboard and shows only
    /// the accessory view. The rounded bottom corners now sit right beside the first and last keys.
    case screenBottom
}

/// How far the key row's ends stay from the screen edges. Pure, so it is tested without a device.
///
/// The system reports one safe area but no corner radius (that value exists only as private API). At the bottom edge the corners
/// are cleared by a margin derived from the safe area instead: iPhones size the home-indicator inset to about 62% of the display's
/// corner radius, so the radius is estimated from it and half of the radius (20-28 pt on today's iPhones) is kept clear beside
/// the first and last key, on top of any side safe area. This is for the iPhone in portrait; nothing here is iPad specific.
public enum KeyBarGeometry {
    /// The bar's height: a 1 pt rule and the keys. It does not change with the keyboard.
    public static let height = 44.0
    /// The bar's height at an interface scale (see `InterfaceScale`): it grows with a larger interface, never below a 44-point target.
    public static func height(scale: Double) -> Double { BottomBarGeometry.target(scale: scale) }
    public static let clearance: ClosedRange<Double> = 20...28
    private static let insetToRadius = 1.0 / 0.62

    /// A bar whose bottom edge is on the screen's bottom edge (within `tolerance` points) is alone at the bottom.
    public static func position(barMaxY: Double, screenHeight: Double, tolerance: Double = 1) -> KeyBarPosition {
        screenHeight > 0 && barMaxY >= screenHeight - tolerance ? .screenBottom : .aboveKeyboard
    }

    /// The distance from a screen side that keeps a control clear of a rounded bottom corner; zero on square displays
    /// (no safe area anywhere but the status bar, e.g. iPhone SE).
    public static func cornerClearance(safeArea: KeyBarInsets) -> Double {
        guard safeArea.bottom > 0 || safeArea.left > 0 || safeArea.right > 0 else { return 0 }
        return min(clearance.upperBound, max(clearance.lowerBound, safeArea.bottom * insetToRadius / 2))
    }

    /// The padding at the ends of the key row.
    /// - Parameter safeArea: the larger of the bar's own and the window's safe area insets.
    public static func padding(safeArea: KeyBarInsets, position: KeyBarPosition) -> KeyBarPadding {
        let left = max(0, safeArea.left), right = max(0, safeArea.right)
        switch position {
        case .aboveKeyboard:
            return KeyBarPadding(left: left, right: right)
        case .screenBottom:
            // "Beyond the safe area": a portrait iPhone has no side safe area, so the whole clearance is added.
            let edge = cornerClearance(safeArea: safeArea)
            return KeyBarPadding(left: max(left, edge), right: max(right, edge))
        }
    }
}

/// The bottom of a terminal and of a chat, sized and lined up from one set of numbers: the key bar over the keyboard (a terminal), and
/// the composer over the keyboard (a chat). Pure, so it is tested without a device.
public enum BottomBarGeometry {
    /// The least width and height of anything that is tapped, in points (Apple's Human Interface Guidelines).
    public static let minimumTarget = 44.0
    /// A tap target at an interface scale: it grows with a larger interface and never shrinks below `minimumTarget`, so the 80 %
    /// size makes glyphs and text smaller but leaves every target at 44 points.
    public static func target(scale: Double) -> Double { max(minimumTarget, InterfaceScale.scaled(minimumTarget, by: scale)) }
    /// On glass the key bar's row sits on a capsule set this far in from the bar's sides (none: it spans the bar, between the screen's
    /// horizontal safe-area edges, as the chat composer's field does), and this far from its top and bottom.
    public static let capsuleSideInset = 0.0, capsuleEndInset = 2.0
    /// How far the first key (and, mirrored, Hide) sits in from the bar's sides, beyond the corner padding: on glass clear of the
    /// capsule's rounded ends, otherwise at the edge (the bar is a full-width band).
    public static func keysInset(glass: Bool) -> Double { glass ? 12 : 0 }
    /// The length over which keys scrolled past one end of the row fade out.
    public static let fadeLength = 16.0

    /// The shape the key row is clipped to, in the bar's coordinates. On glass that is the capsule, so a key scrolled past either end
    /// is cut by its rounded end instead of drawing over it; otherwise the whole bar.
    public struct RowClip: Sendable, Equatable {
        public var x: Double, y: Double, width: Double, height: Double, cornerRadius: Double
    }
    public static func rowClip(barWidth: Double, barHeight: Double, glass: Bool) -> RowClip {
        guard glass else { return RowClip(x: 0, y: 0, width: max(0, barWidth), height: max(0, barHeight), cornerRadius: 0) }
        let height = max(0, barHeight - 2 * capsuleEndInset)
        return RowClip(x: capsuleSideInset, y: capsuleEndInset, width: max(0, barWidth - 2 * capsuleSideInset), height: height, cornerRadius: height / 2)
    }

    /// How strongly each end of the scrolling row fades (0 none, 1 fully): only an end that has keys scrolled past it fades, growing
    /// over the first `fadeLength` points of scrolling, so nothing is dimmed while the row rests at that end.
    public static func fade(offset: Double, contentWidth: Double, visibleWidth: Double) -> (leading: Double, trailing: Double) {
        let overflow = max(0, contentWidth - visibleWidth)
        let position = min(overflow, max(0, offset))
        let hiddenTrailing = overflow - position
        func strength(_ hidden: Double) -> Double { min(1, max(0, hidden / fadeLength)) }
        return (strength(position), strength(hiddenTrailing))
    }

    /// The chat composer's insets: its field spans the screen between the horizontal safe-area edges, the same edges as the key bar's
    /// capsule (the paperclip and the trailing button sit inside its ends; only the text's own insets remain), and it stands as close
    /// above the keyboard as the key bar's capsule does.
    public static func composerInsets(glass: Bool) -> (horizontal: Double, bottom: Double) {
        (capsuleSideInset, capsuleEndInset)
    }
}
