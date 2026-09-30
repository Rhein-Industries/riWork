import Foundation
import RiWorkCore

// Display settings: the size of the app's chrome, the terminal text size and the latency overlay. Each persists on its own and
// takes effect at once. A size that changes the terminal pane changes the grid, and so the desktop pane, through the usual
// (debounced) viewport update.
extension RemoteModel {
    /// Scales headers, lists, key bar and buttons. The colors and the terminal text are not affected.
    func setInterfaceScale(_ value: Double) {
        let scale = InterfaceScale.clamped(value)
        guard scale != interfaceScale else { return }
        interfaceScale = scale
        defaults.set(scale, forKey: Self.interfaceScaleKey)
        theme.setScale(scale)
        // The chrome around the terminal grew or shrank, so the view reports a new pane size; recompute from what is known now too.
        recomputeViewport()
    }
    func stepInterfaceScale(_ steps: Int) { setInterfaceScale(InterfaceScale.stepped(interfaceScale, by: steps)) }

    func setShowLatency(_ on: Bool) {
        guard showLatency != on else { return }
        showLatency = on
        defaults.set(on, forKey: Self.showLatencyKey)
    }
    func setBoldIsBright(_ on: Bool) {
        guard boldIsBright != on else { return }
        boldIsBright = on
        defaults.set(on, forKey: Self.boldIsBrightKey)
    }
}
