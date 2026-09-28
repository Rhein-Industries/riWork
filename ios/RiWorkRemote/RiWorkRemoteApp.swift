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
                    switch phase {
                    case .background: BackgroundGrace.run("RiWork viewport release") { await model.disconnect(background: true) }
                    case .active: Task { await model.resume() }
                    default: break
                    }
                }
        }
    }
}
