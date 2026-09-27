import SwiftUI

@main struct RiWorkRemoteApp: App {
    @State private var model = RemoteModel()
    @Environment(\.scenePhase) private var scenePhase
    var body: some Scene {
        WindowGroup {
            RemoteRootView(model: model)
                .tint(Color(red: 0.23, green: 0.43, blue: 0.69))
                .onChange(of: scenePhase) { _, phase in
                    Task { if phase == .background { await model.disconnect(background: true) } else if phase == .active { await model.resume() } }
                }
        }
    }
}
