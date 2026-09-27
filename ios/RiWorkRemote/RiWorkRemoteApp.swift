import SwiftUI

@main struct RiWorkRemoteApp: App {
    @State private var model = RemoteModel()
    @Environment(\.scenePhase) private var scenePhase
    var body: some Scene {
        WindowGroup {
            RemoteRootView(model: model)
                .tint(DesktopStyle.accent)
                .foregroundStyle(DesktopStyle.text)
                .font(.custom("Menlo", size: 13, relativeTo: .body))
                .buttonStyle(DesktopButtonStyle())
                .onChange(of: scenePhase) { _, phase in
                    Task { if phase == .background { await model.disconnect(background: true) } else if phase == .active { await model.resume() } }
                }
        }
    }
}
