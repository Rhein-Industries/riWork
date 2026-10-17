import SwiftUI
import UIKit

@main struct RiWorkRemoteApp: App {
    @State private var model = RemoteModel(linkWatcher: SystemLinkWatcher())
    @Environment(\.scenePhase) private var scenePhase
    init() {
        // The terminal's rows are whole device pixels tall; this is what a pixel is.
        TerminalFont.pixelsPerPoint = Double(UITraitCollection.current.displayScale > 0 ? UITraitCollection.current.displayScale : UIScreen.main.scale)
    }
    var body: some Scene {
        WindowGroup {
            RemoteRootView(model: model)
                .desktopThemed(model.theme.style)
                .modifier(TerminalPixelScale(model: model))
                // Dictation is biased towards the session on screen, read fresh each time it starts.
                .onAppear { DictationController.shared.sessionSources = { [model] in model.speechSources() } }
                .onChange(of: scenePhase) { _, phase in
                    switch phase {
                    case .background:
                        model.setAppActive(false)
                        BackgroundGrace.run("RiWork viewport release") { await model.disconnect(background: true) }
                    case .active: Task { await model.appDidBecomeActive() }
                    default: break
                    }
                }
        }
    }
}

/// The window's own pixels per point, kept in `TerminalFont.pixelsPerPoint`: in Stage Manager the window can move to a display with
/// another scale than the iPad's, and the terminal's rows must stay whole pixels tall there. The grid is worked out again with it.
private struct TerminalPixelScale: ViewModifier {
    let model: RemoteModel
    @Environment(\.displayScale) private var displayScale
    func body(content: Content) -> some View {
        content.onChange(of: displayScale, initial: true) { _, scale in
            guard scale > 0, Double(scale) != TerminalFont.pixelsPerPoint else { return }
            TerminalFont.pixelsPerPoint = Double(scale)
            model.recomputeViewport()
        }
    }
}
