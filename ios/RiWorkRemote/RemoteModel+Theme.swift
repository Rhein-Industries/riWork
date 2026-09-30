import Foundation
import RiWorkCore

/// Whether the connected desktop offers its colors (`appearance.get`), learned from the first answer.
enum ThemeSyncSupport: Equatable {
    /// Not asked yet on this connection.
    case unknown
    /// The desktop answered, or said it has nothing published yet (`not_found`); asking again later can succeed.
    case available
    /// An older desktop, or a newer format than this app reads. Not asked again on this connection.
    case unavailable
}

// Theme sync: the phone draws with the desktop's current colors. It asks on connect, when the app becomes active, and once
// a minute while connected, one request at a time. The answer never interrupts anything: it only swaps colors in place.
extension RemoteModel {
    /// Fetches the desktop's colors once. No-op while not connected, while a fetch is in flight, or once the desktop has said it
    /// cannot supply them.
    func fetchAppearance() async {
        guard state == .connected, themeSupport != .unavailable, !appearanceInFlight, let desktopID = selectedDesktopID else { return }
        let token = generation
        // A fetch left over from an earlier connection must not block, or later clear, this one.
        let flight = UUID()
        appearanceFlight = flight
        lastAppearanceFetch = ContinuousClock.now
        defer { if appearanceFlight == flight { appearanceFlight = nil } }
        do {
            let fetched = try await client.appearance()
            guard generation == token, selectedDesktopID == desktopID else { return }
            themeSupport = .available
            theme.receive(fetched, for: desktopID)
        } catch {
            guard !(error is CancellationError), generation == token, selectedDesktopID == desktopID else { return }
            if RemoteError.isUnsupportedMethod(error) {
                // An older desktop has no colors to give (and any it once gave are stale): built-in style, and stop asking.
                themeSupport = .unavailable
                theme.clear(desktopID)
            } else if case RemoteError.rpc("not_found", _) = error {
                // The desktop app is not running or has not published yet. Keep the last known palette and ask again later.
                themeSupport = .available
            } else if error as? AppearanceError == .unsupportedVersion {
                // A newer format than this app reads: keep what is shown and stop asking on this connection.
                themeSupport = .unavailable
            }
            // Anything else (a bad field, a dropped link) is not worth a message: the last palette stays and the next tick retries.
        }
    }

    /// The app became active. Reconnecting is `resume()`'s job (its `connect()` fetches by itself); when the connection stayed up,
    /// colors may have changed meanwhile, so ask, unless a fetch just happened.
    func appDidBecomeActive() async {
        await resume()
        guard state == .connected else { return }
        if let last = lastAppearanceFetch, ContinuousClock.now - last < themeMinimumGap { return }
        await fetchAppearance()
    }

    /// Fetches now and then every `themeRefreshInterval` for as long as this connection lives.
    func startThemeSync(token: UUID) {
        themeTask?.cancel()
        themeTask = Task { [weak self, interval = themeRefreshInterval] in
            while !Task.isCancelled {
                // The model is held only for the request, never while waiting for the next one.
                guard await self?.themeTick(token: token) == true else { return }
                try? await Task.sleep(for: interval)
            }
        }
    }
    private func themeTick(token: UUID) async -> Bool {
        guard generation == token, state == .connected, themeSupport != .unavailable else { return false }
        await fetchAppearance()
        return generation == token && themeSupport != .unavailable
    }
    func stopThemeSync() {
        themeTask?.cancel(); themeTask = nil
    }
}
