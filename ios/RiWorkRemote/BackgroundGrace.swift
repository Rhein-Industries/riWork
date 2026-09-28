import UIKit

/// iOS can suspend the app moments after it enters the background, before an awaited RPC completes.
/// This keeps it running long enough to release the desktop terminal size before the socket closes.
@MainActor enum BackgroundGrace {
    @MainActor private final class Grant {
        var identifier = UIBackgroundTaskIdentifier.invalid
        func end() {
            guard identifier != .invalid else { return }
            UIApplication.shared.endBackgroundTask(identifier)
            identifier = .invalid
        }
    }
    static func run(_ name: String, _ work: @escaping @MainActor () async -> Void) {
        let grant = Grant()
        // If time runs out the desktop still restores its size within 15 s of losing the connection.
        grant.identifier = UIApplication.shared.beginBackgroundTask(withName: name) { grant.end() }
        Task { await work(); grant.end() }
    }
}
