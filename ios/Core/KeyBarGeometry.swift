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
    /// The bar's height at an interface scale (see `InterfaceScale`).
    public static func height(scale: Double) -> Double { InterfaceScale.scaled(height, by: scale) }
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
