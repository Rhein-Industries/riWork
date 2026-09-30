import Foundation

/// How large the app's chrome (headers, lists, key bar, buttons) is drawn: 80-130 % in steps of 5 %.
/// A plain factor applied to fonts and touch targets; Dynamic Type is left alone. The terminal text size is separate.
public enum InterfaceScale {
    public static let range: ClosedRange<Double> = 0.80...1.30
    public static let step = 0.05
    public static let standard = 1.0
    /// The nearest step inside the range; the standard size for anything that is not a number.
    public static func clamped(_ value: Double) -> Double {
        guard value.isFinite else { return standard }
        let percent = (value * 20).rounded() * 5
        return min(range.upperBound, max(range.lowerBound, percent / 100))
    }
    public static func stepped(_ value: Double, by steps: Int) -> Double { clamped(clamped(value) + Double(steps) * step) }
    public static func percent(_ value: Double) -> Int { Int((clamped(value) * 100).rounded()) }
    /// `points` at this scale, on whole points so lines stay crisp.
    public static func scaled(_ points: Double, by scale: Double) -> Double { (points * clamped(scale)).rounded() }
}
