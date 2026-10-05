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
