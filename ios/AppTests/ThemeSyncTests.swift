import XCTest
import SwiftUI
import Observation
import RiWorkCore
@testable import RiWorkRemote

private final class Flag: @unchecked Sendable {
    private let lock = NSLock()
    private var raised = false
    func raise() { lock.withLock { raised = true } }
    var value: Bool { lock.withLock { raised } }
}

/// Theme sync against the fake desktop: when it asks, what it keeps, and what the phone draws with.
@MainActor final class ThemeSyncTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let shell = "44444444-4444-4444-8444-444444444444"
    private let routeA = "33333333-3333-4333-8333-333333333333"
    private let routeB = "77777777-7777-4777-8777-777777777777"

    // MARK: fixtures

    private func pairing(route: String, device: String) throws -> Pairing {
        try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"\(device)","route_id":"\(route)","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
    }
    /// One or two desktops. `selected: nil` means nothing was ever chosen.
    private func makeStore(desktops: Int = 1, selected: String? = nil) throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        var list: [SavedDesktop] = []
        for (index, route) in [routeA, routeB].prefix(desktops).enumerated() {
            var desktop = SavedDesktop(name: index == 0 ? "Alpha" : "Beta", pairing: try pairing(route: route, device: "2222222\(index)-2222-4222-8222-222222222222"), allowLocalDevelopment: false)
            desktop.selectedProjectID = project; desktop.selectedSessionID = shell
            list.append(desktop)
        }
        try keychain.write(Library(desktops: list, selectedDesktopID: selected))
        return keychain
    }
    private func scratchDefaults() -> UserDefaults {
        let name = "com.riwork.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }
    private func makeModel(_ transport: FixtureTransport, _ keychain: KeychainStore, defaults: UserDefaults, refresh: Duration = .seconds(60), gap: Duration = .seconds(5)) -> RemoteModel {
        RemoteModel(client: transport, keychain: keychain, pollInterval: .seconds(30), keyFlushInterval: .milliseconds(5), previewDelay: .milliseconds(80),
                    defaults: defaults, themeRefreshInterval: refresh, themeMinimumGap: gap,
                    cellMetrics: { TerminalLayout.approximateCell(fontSize: $0) }, keepAwake: { _ in })
    }
    private func eventually(_ what: String, timeout: Double = 3, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(5)) }
        let met = await condition()
        XCTAssertTrue(met, what)
    }

    /// The wire form of a palette. Defaults are the desktop's RiWork dark palette.
    private func wire(dark: Bool = true, updated: Double = 1_790_000_000, bg: String = "#090d14", panel: String = "#101720", active: String = "#14212a", divider: String = "#253c45",
                      cyan: String = "#55e6dc", magenta: String = "#ce78ef", gold: String = "#f4bf75", text: String = "#d3e1e6", muted: String = "#708993",
                      terminal: (background: String, foreground: String)? = nil) -> JSONValue {
        var fields: [String: JSONValue] = [
            "v": .number(1), "updated_at": .number(updated), "dark": .bool(dark),
            "palette": .object(["bg": .string(bg), "panel": .string(panel), "panel_active": .string(active), "divider": .string(divider), "cyan": .string(cyan),
                                "magenta": .string(magenta), "gold": .string(gold), "text": .string(text), "muted": .string(muted)])
        ]
        if let terminal {
            fields["terminal"] = .object(["background": .string(terminal.background), "foreground": .string(terminal.foreground),
                                          "palette": .array((0..<16).map { _ in .string("#808080") })])
        }
        return .object(fields)
    }
    private func appearance(_ value: JSONValue) throws -> DesktopAppearance { try DesktopAppearance(json: value) }
    private func fixed(_ hex: String) -> ThemeColor { ThemeColor(fixed: RGB(hex: hex)!) }
    private func storedText(_ defaults: UserDefaults, _ route: String) -> String? { defaults.string(forKey: ThemeStore.key(for: route)) }
    private func store(_ value: JSONValue, in defaults: UserDefaults, for route: String) throws {
        ThemeStore(defaults: defaults).receive(try appearance(value), for: route)
    }

    // MARK: when it asks

    func testFetchesOnConnectAndDrawsWithThePalette() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        let transport = FixtureTransport()
        await transport.setAppearance(.ok(wire(cyan: "#40e0d0", gold: "#e0b060")))
        let model = makeModel(transport, keychain, defaults: defaults)
        XCTAssertEqual(model.theme.style.theme, .builtIn, "nothing is known before the first answer")
        await model.connect()
        await eventually("the palette arrived") { model.theme.appearance != nil }
        let requests = await transport.appearanceRequests()
        XCTAssertEqual(requests, 1)
        XCTAssertEqual(model.themeSupport, .available)
        let theme = model.theme.style.theme
        XCTAssertEqual(theme.accent, fixed("#40e0d0"), "cyan is the accent")
        XCTAssertEqual(theme.gold, fixed("#e0b060"))
        XCTAssertEqual(theme.background, fixed("#090d14"))
        XCTAssertEqual(theme.dark, true)
        XCTAssertEqual(model.theme.style.colorScheme, .dark, "the status bar and system controls follow the desktop")
        XCTAssertNotNil(storedText(defaults, routeA), "stored per desktop")
        XCTAssertEqual(model.state, .connected)
        XCTAssertEqual(model.output, "existing session output", "the connection carried on as usual")
        await model.disconnect()
    }
    func testAskingNeverDelaysOrDisturbsTheRestOfTheConnection() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.block("appearance.get")
        let model = makeModel(transport, keychain, defaults: scratchDefaults())
        await model.connect()
        XCTAssertEqual(model.state, .connected, "connect returns while the palette request is still parked")
        XCTAssertEqual(model.output, "existing session output")
        XCTAssertFalse(model.snapshotStale)
        await transport.unblock()
        await model.disconnect()
    }
    /// The wire form with the desktop's Native flag, in Native's own light or dark colors (src/theme.rs NATIVE_LIGHT / NATIVE_DARK).
    private func nativeWire(dark: Bool, updated: Double, native: Bool = true) -> JSONValue {
        let base = dark
            ? wire(dark: true, updated: updated, bg: "#000000", panel: "#1c1c1e", active: "#2c2c2e", divider: "#3a3a3c", cyan: "#ffffff", magenta: "#c7c7cc", gold: "#ff9f0a", text: "#f5f5f7", muted: "#98989d")
            : wire(dark: false, updated: updated, bg: "#ffffff", panel: "#f5f5f7", active: "#e8e8ed", divider: "#d2d2d7", cyan: "#000000", magenta: "#3a3a3c", gold: "#b34000", text: "#1d1d1f", muted: "#636366")
        guard native, case .object(var fields) = base else { return base }
        fields["native"] = .bool(true)
        return .object(fields)
    }

    func testFollowsTheDesktopSwitchingNativeAndItsLightAndDarkLive() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        let transport = FixtureTransport()
        // A desktop without the flag: the terminal look.
        await transport.setAppearance(.ok(wire()))
        let model = makeModel(transport, keychain, defaults: defaults, refresh: .milliseconds(40))
        await model.connect()
        await eventually("first palette") { model.theme.appearance != nil }
        XCTAssertFalse(model.theme.style.native)
        XCTAssertEqual(model.theme.style.cased("New terminal"), "NEW TERMINAL", "the terminal look keeps its capitals")
        // Native switched on, light.
        await transport.setAppearance(.ok(nativeWire(dark: false, updated: 1_790_000_100)))
        await eventually("Native on") { model.theme.style.native }
        XCTAssertEqual(model.theme.style.colorScheme, .light)
        XCTAssertEqual(model.theme.style.cased("New terminal"), "New terminal")
        // macOS goes dark: the same skin, the other side.
        await transport.setAppearance(.ok(nativeWire(dark: true, updated: 1_790_000_200)))
        await eventually("Native dark") { model.theme.style.colorScheme == .dark }
        XCTAssertTrue(model.theme.style.native)
        XCTAssertEqual(model.theme.style.theme.background, fixed("#000000"))
        // Native off with the very same colors: still a change, back to the terminal look.
        await transport.setAppearance(.ok(nativeWire(dark: true, updated: 1_790_000_300, native: false)))
        await eventually("Native off") { !model.theme.style.native }
        XCTAssertEqual(model.theme.style.theme.background, fixed("#000000"))
        // What is stored follows, so a relaunch draws the last skin before the first answer.
        await transport.setAppearance(.ok(nativeWire(dark: false, updated: 1_790_000_400)))
        await eventually("Native on again") { model.theme.style.native }
        await model.disconnect()
        let relaunched = ThemeStore(defaults: defaults)
        relaunched.showInitial(selected: routeA, existing: [routeA])
        XCTAssertTrue(relaunched.style.native)
    }

    /// The palette with the desktop's mic setting on (`"mic": true`), or as sent with it off (no field).
    private func micWire(_ on: Bool, updated: Double) -> JSONValue {
        guard on, case .object(var fields) = wire(updated: updated) else { return wire(updated: updated) }
        fields["mic"] = .bool(true)
        return .object(fields)
    }
    func testFollowsTheDesktopsMicSettingLiveOnTheRefreshOnForegroundAndOnConnect() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        let transport = FixtureTransport()
        // A desktop that predates the setting: off.
        await transport.setAppearance(.ok(wire()))
        let model = makeModel(transport, keychain, defaults: defaults, refresh: .milliseconds(40), gap: .zero)
        await model.connect()
        await eventually("first palette") { model.theme.appearance != nil }
        XCTAssertFalse(model.theme.style.mic)
        XCTAssertFalse(DictationController.shared.isAllowed, "no dictation while it is off")
        // Turned on with the very same colors: still a change, picked up by the periodic refresh, without a reconnect.
        await transport.setAppearance(.ok(micWire(true, updated: 1_790_000_100)))
        await eventually("on, live") { model.theme.style.mic }
        XCTAssertTrue(DictationController.shared.isAllowed)
        XCTAssertEqual(model.theme.style.theme.accent, fixed("#55e6dc"), "the colors are untouched")
        // Off again: the periodic refresh again.
        await transport.setAppearance(.ok(micWire(false, updated: 1_790_000_200)))
        await eventually("off, live") { !model.theme.style.mic }
        XCTAssertFalse(DictationController.shared.isAllowed)
        await model.disconnect()
        // While away the desktop turns it on: coming back to the foreground (a reconnect) picks it up.
        let eager = makeModel(transport, keychain, defaults: defaults, gap: .zero)
        await transport.setAppearance(.ok(micWire(true, updated: 1_790_000_300)))
        await eager.connect()
        await eventually("on connect") { eager.theme.style.mic }
        await transport.setAppearance(.ok(micWire(false, updated: 1_790_000_400)))
        await eager.appDidBecomeActive()
        XCTAssertFalse(eager.theme.style.mic, "on coming back to the foreground")
        // What is stored follows, so a relaunch starts with the last setting before the first answer.
        await transport.setAppearance(.ok(micWire(true, updated: 1_790_000_500)))
        await eager.fetchAppearance()
        XCTAssertTrue(eager.theme.style.mic)
        await eager.disconnect()
        let relaunched = ThemeStore(defaults: defaults)
        relaunched.showInitial(selected: routeA, existing: [routeA])
        XCTAssertTrue(relaunched.style.mic)
        XCTAssertTrue(try XCTUnwrap(storedText(defaults, routeA)).contains("\"mic\":true"))
        // A fresh model starts from no setting until its desktop says.
        _ = makeModel(FixtureTransport(), keychain, defaults: scratchDefaults())
        XCTAssertFalse(DictationController.shared.isAllowed)
    }
    func testRefreshesPeriodicallyWhileConnectedAndPicksUpChanges() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        let transport = FixtureTransport()
        await transport.setAppearance(.ok(wire(cyan: "#55e6dc")))
        let model = makeModel(transport, keychain, defaults: defaults, refresh: .milliseconds(40))
        await model.connect()
        await eventually("first palette") { model.theme.style.theme.accent == self.fixed("#55e6dc") }
        await transport.setAppearance(.ok(wire(updated: 1_790_000_100, cyan: "#ff8800")))
        await eventually("the change is applied live") { model.theme.style.theme.accent == self.fixed("#ff8800") }
        await eventually("asked repeatedly") { await transport.appearanceRequests() >= 4 }
        let stored = try XCTUnwrap(storedText(defaults, routeA))
        XCTAssertTrue(stored.contains("#ff8800"), "the stored copy follows")
        await model.disconnect()
        try await Task.sleep(for: .milliseconds(100))
        let stopped = await transport.appearanceRequests()
        try await Task.sleep(for: .milliseconds(200))
        let after = await transport.appearanceRequests()
        XCTAssertEqual(after, stopped, "no asking once disconnected")
    }
    func testOnlyOneRequestIsInFlightWhateverTriggersIt() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.block("appearance.get")
        let model = makeModel(transport, keychain, defaults: scratchDefaults(), refresh: .milliseconds(10), gap: .zero)
        await model.connect()
        await transport.waitUntilBlocked()
        // Activation, another explicit fetch and the periodic tick all arrive while the first request is parked.
        async let a: Void = model.appDidBecomeActive()
        async let b: Void = model.fetchAppearance()
        _ = await (a, b)
        try await Task.sleep(for: .milliseconds(80))
        let peak = await transport.peakInFlight("appearance.get")
        let total = await transport.appearanceRequests()
        XCTAssertEqual(peak, 1)
        XCTAssertEqual(total, 1)
        await transport.unblock()
        await model.disconnect()
    }
    func testBecomingActiveAsksAgainOnAConnectionThatStayedUpUnlessItJustAsked() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.setAppearance(.ok(wire()))
        let model = makeModel(transport, keychain, defaults: scratchDefaults(), gap: .seconds(60))
        await model.connect()
        await eventually("connect asked") { await transport.appearanceRequests() == 1 }
        await model.appDidBecomeActive()
        try await Task.sleep(for: .milliseconds(50))
        var requests = await transport.appearanceRequests()
        XCTAssertEqual(requests, 1, "a fetch a moment ago is enough")
        await model.disconnect()

        let eager = makeModel(transport, keychain, defaults: scratchDefaults(), gap: .zero)
        await eager.connect()
        await eventually("connect asked") { await transport.appearanceRequests() == 2 }
        await transport.setAppearance(.ok(wire(updated: 1_790_000_500, cyan: "#ff8800")))
        await eager.appDidBecomeActive()
        requests = await transport.appearanceRequests()
        XCTAssertEqual(requests, 3)
        XCTAssertEqual(eager.theme.style.theme.accent, fixed("#ff8800"), "new colors are there when the app comes back")
        await eager.disconnect()
    }
    func testResumingAfterTheBackgroundAsksOnceThroughTheReconnect() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.setAppearance(.ok(wire()))
        let model = makeModel(transport, keychain, defaults: scratchDefaults(), gap: .seconds(60))
        await model.connect()
        await eventually("asked on connect") { await transport.appearanceRequests() == 1 }
        await model.disconnect(background: true)
        XCTAssertEqual(model.state, .suspended)
        await model.appDidBecomeActive()
        XCTAssertEqual(model.state, .connected)
        await eventually("asked on the new connection") { await transport.appearanceRequests() == 2 }
        try await Task.sleep(for: .milliseconds(60))
        let requests = await transport.appearanceRequests()
        XCTAssertEqual(requests, 2, "the reconnect and the activation share one request")
        await model.disconnect()
    }

    // MARK: what it keeps

    func testAnUnchangedPaletteDoesNotTouchAnything() throws {
        let defaults = scratchDefaults()
        let store = ThemeStore(defaults: defaults)
        store.showInitial(selected: routeA, existing: [routeA])
        XCTAssertTrue(store.receive(try appearance(wire()), for: routeA))
        let first = try XCTUnwrap(storedText(defaults, routeA))
        let woken = Flag()
        withObservationTracking { _ = store.style; _ = store.appearance } onChange: { woken.raise() }
        XCTAssertFalse(store.receive(try appearance(wire(updated: 1_790_009_999)), for: routeA), "only the publication time differs")
        XCTAssertFalse(woken.value, "views are not invalidated for nothing")
        XCTAssertEqual(storedText(defaults, routeA), first, "and nothing is rewritten")
        XCTAssertTrue(store.receive(try appearance(wire(cyan: "#ff8800")), for: routeA))
        XCTAssertTrue(woken.value)
    }
    func testThePaletteIsStoredPerDesktopAndAppliedAtLaunchBeforeAnyFetch() async throws {
        let keychain = try makeStore(selected: routeA); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        try store(wire(dark: false, bg: "#fbf1c7", panel: "#f4ebc2", active: "#ede3bc", divider: "#d5ccb6", cyan: "#427b58", magenta: "#8f3f71", gold: "#9d5015", text: "#3c3836", muted: "#756f5e"), in: defaults, for: routeA)
        let transport = FixtureTransport()
        let launched = makeModel(transport, keychain, defaults: defaults)
        XCTAssertEqual(launched.theme.style.theme.background, fixed("#fbf1c7"), "drawn from the very first frame")
        XCTAssertEqual(launched.theme.style.colorScheme, .light)
        XCTAssertEqual(launched.theme.shownDesktopID, routeA)
        let requests = await transport.appearanceRequests()
        XCTAssertEqual(requests, 0, "nothing was asked yet")
        // On connect the stored palette is on screen while the first answer is on its way.
        await transport.block("appearance.get")
        await launched.connect()
        await transport.waitUntilBlocked()
        XCTAssertEqual(launched.theme.style.theme.background, fixed("#fbf1c7"))
        await transport.unblock()
        await launched.disconnect()
    }
    func testAPaletteThatIsNoLongerValidIsNotTrustedFromStorage() throws {
        let defaults = scratchDefaults()
        defaults.set("{\"v\":1,\"dark\":true,\"palette\":{\"bg\":\"nope\"}}", forKey: ThemeStore.key(for: routeA))
        let store = ThemeStore(defaults: defaults)
        store.showInitial(selected: routeA, existing: [routeA])
        XCTAssertEqual(store.style.theme, .builtIn)
        XCTAssertNil(defaults.string(forKey: ThemeStore.key(for: routeA)), "and it is dropped")
    }
    func testTheListWearsTheMostRecentlyUsedDesktopsColorsOrTheBuiltInStyle() async throws {
        let keychain = try makeStore(desktops: 2, selected: nil); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        let never = makeModel(FixtureTransport(), keychain, defaults: defaults)
        XCTAssertEqual(never.theme.style.theme, .builtIn, "nothing chosen yet")

        try store(wire(cyan: "#e070f0"), in: defaults, for: routeB)
        let seeded = ThemeStore(defaults: defaults)
        seeded.select(routeB)
        let relaunched = makeModel(FixtureTransport(), keychain, defaults: defaults)
        XCTAssertEqual(relaunched.theme.shownDesktopID, routeB, "the list uses the most recently used desktop")
        XCTAssertEqual(relaunched.theme.style.theme.accent, fixed("#e070f0"))

        // Choosing the other desktop switches at once, to its own palette or to the built-in style.
        await relaunched.activate(routeA)
        XCTAssertEqual(relaunched.theme.shownDesktopID, routeA)
        XCTAssertEqual(relaunched.theme.style.theme, .builtIn, "Alpha has no palette yet")
        await relaunched.disconnect()
        XCTAssertEqual(relaunched.theme.recentDesktopIDs.first, routeA)
        XCTAssertEqual(relaunched.theme.style(for: routeB).theme.accent, fixed("#e070f0"), "screens that belong to Beta keep using Beta's colors")
        XCTAssertEqual(relaunched.theme.style(for: routeA).theme, .builtIn)
    }
    func testRemovingADesktopForgetsItsPaletteAndFallsBackToTheNextMostRecent() async throws {
        let keychain = try makeStore(desktops: 2, selected: routeA); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        try store(wire(cyan: "#e070f0"), in: defaults, for: routeA)
        try store(wire(cyan: "#40e040"), in: defaults, for: routeB)
        let seeding = ThemeStore(defaults: defaults)
        seeding.select(routeB); seeding.select(routeA)
        let model = makeModel(FixtureTransport(), keychain, defaults: defaults)
        XCTAssertEqual(model.theme.style.theme.accent, fixed("#e070f0"))
        try await model.remove(id: routeA)
        XCTAssertNil(storedText(defaults, routeA))
        XCTAssertEqual(model.theme.shownDesktopID, routeB)
        XCTAssertEqual(model.theme.style.theme.accent, fixed("#40e040"))
        try await model.remove(id: routeB)
        XCTAssertEqual(model.theme.style.theme, .builtIn)
        XCTAssertEqual(model.theme.recentDesktopIDs, [])
    }
    func testNotFoundKeepsTheLastPaletteAndKeepsAsking() async throws {
        let keychain = try makeStore(selected: routeA); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        try store(wire(cyan: "#e070f0"), in: defaults, for: routeA)
        let transport = FixtureTransport()          // answers not_found: the desktop app has not published
        let model = makeModel(transport, keychain, defaults: defaults, refresh: .milliseconds(30))
        await model.connect()
        await eventually("asked repeatedly") { await transport.appearanceRequests() >= 3 }
        XCTAssertEqual(model.theme.style.theme.accent, fixed("#e070f0"), "the last known palette stays")
        XCTAssertNotNil(storedText(defaults, routeA))
        XCTAssertEqual(model.themeSupport, .available, "the desktop understands the request; it just has nothing yet")
        XCTAssertNil(model.error, "no message for something that is not a problem")
        XCTAssertEqual(model.state, .connected)
        // It publishes later: the palette arrives on a later tick.
        await transport.setAppearance(.ok(wire(cyan: "#40e040")))
        await eventually("published") { model.theme.style.theme.accent == self.fixed("#40e040") }
        await model.disconnect()
    }
    func testAnUnsupportedDesktopIsNotAskedAgainOnThisConnectionAndKeepsTheBuiltInStyle() async throws {
        let keychain = try makeStore(selected: routeA); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        let transport = FixtureTransport()
        await transport.setAppearance(.unsupported)
        let model = makeModel(transport, keychain, defaults: defaults, refresh: .milliseconds(20), gap: .zero)
        await model.connect()
        await eventually("marked unsupported") { model.themeSupport == .unavailable }
        try await Task.sleep(for: .milliseconds(250))
        var requests = await transport.appearanceRequests()
        XCTAssertEqual(requests, 1, "asked once, then never again on this connection")
        await model.appDidBecomeActive()
        await model.fetchAppearance()
        requests = await transport.appearanceRequests()
        XCTAssertEqual(requests, 1, "not even when the app becomes active")
        XCTAssertEqual(model.theme.style.theme, .builtIn)
        XCTAssertNil(model.error)
        XCTAssertEqual(model.state, .connected)
        // A new connection probes again: the desktop may have been upgraded meanwhile.
        await transport.setAppearance(.ok(wire(cyan: "#ff8800")))
        await model.disconnect()
        await model.connect()
        await eventually("upgraded desktop") { model.theme.style.theme.accent == self.fixed("#ff8800") }
        XCTAssertEqual(model.themeSupport, .available)
        await model.disconnect()
    }
    func testADesktopThatBecomesUnsupportedDropsItsOldPaletteAndTheBuiltInStyleReturns() async throws {
        let keychain = try makeStore(selected: routeA); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        try store(wire(cyan: "#e070f0"), in: defaults, for: routeA)
        let transport = FixtureTransport()
        await transport.setAppearance(.unsupported)
        let model = makeModel(transport, keychain, defaults: defaults)
        XCTAssertEqual(model.theme.style.theme.accent, fixed("#e070f0"))
        await model.connect()
        await eventually("built-in again") { model.theme.style.theme == .builtIn }
        XCTAssertNil(storedText(defaults, routeA))
        await model.disconnect()
    }
    func testAnUnusablePaletteChangesNothing() async throws {
        let keychain = try makeStore(selected: routeA); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        try store(wire(cyan: "#e070f0"), in: defaults, for: routeA)
        let transport = FixtureTransport()
        await transport.setAppearance(.garbage)
        let model = makeModel(transport, keychain, defaults: defaults, refresh: .milliseconds(30))
        await model.connect()
        await eventually("tried a few times") { await transport.appearanceRequests() >= 3 }
        XCTAssertEqual(model.theme.style.theme.accent, fixed("#e070f0"))
        XCTAssertNil(model.error)
        await transport.setAppearance(.ok(.object(["v": .number(2)])))
        await model.disconnect(); await model.connect()
        await eventually("a newer format is given up on") { model.themeSupport == .unavailable }
        XCTAssertEqual(model.theme.style.theme.accent, fixed("#e070f0"), "and what was shown stays")
        await model.disconnect()
    }
    func testEachDesktopKeepsItsOwnPaletteAndSwitchingAppliesTheRightOneAtOnce() async throws {
        let keychain = try makeStore(desktops: 2, selected: routeA); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        let transport = FixtureTransport()
        await transport.setAppearance(.ok(wire(cyan: "#e070f0")))
        let model = makeModel(transport, keychain, defaults: defaults)
        await model.connect()
        await eventually("Alpha's palette") { model.theme.style.theme.accent == self.fixed("#e070f0") }
        await transport.setAppearance(.notPublished)
        await model.activate(routeB)
        XCTAssertEqual(model.theme.shownDesktopID, routeB)
        XCTAssertEqual(model.theme.style.theme, .builtIn, "Beta has published nothing: Alpha's colors are not carried over")
        await eventually("Beta was asked") { await transport.appearanceRequests() >= 2 }
        XCTAssertNil(storedText(defaults, routeB))
        XCTAssertNotNil(storedText(defaults, routeA), "and Alpha's palette is still there")
        await model.activate(routeA)
        XCTAssertEqual(model.theme.style.theme.accent, fixed("#e070f0"), "back to Alpha: its palette is on screen before the connection is up")
        await model.disconnect()
    }
    func testAFetchThatEndsAfterTheDesktopChangedIsDropped() async throws {
        let keychain = try makeStore(desktops: 2, selected: routeA); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        let transport = FixtureTransport()
        await transport.setAppearance(.ok(wire(cyan: "#e070f0")))
        await transport.block("appearance.get")
        let model = makeModel(transport, keychain, defaults: defaults)
        await model.connect()
        await transport.waitUntilBlocked()
        // Alpha's request is parked. Choosing Beta must not let Alpha's answer land on Beta.
        await transport.setAppearance(.notPublished)
        await model.activate(routeB)
        await transport.unblock()
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertNil(storedText(defaults, routeA), "the abandoned request stored nothing")
        XCTAssertEqual(model.theme.style.theme, .builtIn)
        await model.disconnect()
    }

    // MARK: readability

    func testAPaletteWithUnreadableTextFallsBackToTheBuiltInPair() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        let transport = FixtureTransport()
        // Dark background, nearly the same text.
        await transport.setAppearance(.ok(wire(bg: "#101010", panel: "#141414", active: "#181818", text: "#1c1c1c")))
        let model = makeModel(transport, keychain, defaults: defaults)
        await model.connect()
        await eventually("applied") { model.theme.appearance != nil }
        let theme = model.theme.style.theme
        XCTAssertEqual(theme.text, fixed("#d3e1e6"), "the built-in dark pair, not the unreadable one")
        XCTAssertEqual(theme.background, fixed("#090d14"))
        XCTAssertGreaterThanOrEqual(theme.text.dark.contrast(with: theme.background.dark), DesktopTheme.minimumContrast)
        XCTAssertEqual(theme.dark, true, "the status bar still follows the desktop")
        XCTAssertTrue(try XCTUnwrap(storedText(defaults, routeA)).contains("#1c1c1c"), "what the desktop said is what is stored; the guard is applied when drawing")
        await model.disconnect()
    }
    func testTheTerminalUsesItsOwnColorsAndFallsBackToThePaletteIfTheyClash() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.setAppearance(.ok(wire(terminal: (background: "#101418", foreground: "#e0e8ec"))))
        let model = makeModel(transport, keychain, defaults: scratchDefaults())
        await model.connect()
        await eventually("applied") { model.theme.appearance != nil }
        var theme = model.theme.style.theme
        XCTAssertEqual(theme.terminalBackground, fixed("#101418"))
        XCTAssertEqual(theme.terminalForeground, fixed("#e0e8ec"))
        XCTAssertEqual(theme.terminalCursor, fixed("#e0e8ec"))
        await transport.setAppearance(.ok(wire(updated: 1_790_000_900, terminal: (background: "#101418", foreground: "#121a1e"))))
        await model.fetchAppearance()
        theme = model.theme.style.theme
        XCTAssertEqual(theme.terminalBackground, theme.background, "an unreadable terminal pair is replaced by palette.bg / palette.text")
        XCTAssertEqual(theme.terminalForeground, theme.text)
        await model.disconnect()
    }

    // MARK: not disturbing the person typing

    func testANewPaletteWhileTypingLosesNothingAndKeepsTheKeyboardUp() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.setAppearance(.ok(wire(cyan: "#55e6dc")))
        let model = makeModel(transport, keychain, defaults: scratchDefaults(), refresh: .milliseconds(15))
        await model.connect()
        await eventually("palette") { model.theme.appearance != nil }
        let expected = String((0..<40).map { Character(String($0 % 10)) })
        for (index, character) in expected.enumerated() {
            XCTAssertEqual(model.type(KeyMapper.items(for: String(character))), .accepted)
            if index % 8 == 0 { await transport.setAppearance(.ok(wire(updated: Double(1_790_001_000 + index), cyan: index % 16 == 0 ? "#ff8800" : "#55e6dc"))) }
            try? await Task.sleep(for: .milliseconds(4))
        }
        await eventually("all delivered") { await transport.delivered(shell: self.shell) == expected }
        XCTAssertEqual(model.state, .connected)
        XCTAssertEqual(model.typedCount, 40)
        XCTAssertNil(model.error)
        await model.disconnect()
    }
    func testRecoloringTheKeyBarKeepsTheViewItsFocusAndItsState() async throws {
        let focus = KeyFocus()
        let dark = DesktopStyle(DesktopTheme.resolve(try appearance(wire())))
        let light = DesktopStyle(DesktopTheme.resolve(try appearance(wire(dark: false, bg: "#fbf1c7", panel: "#f4ebc2", active: "#ede3bc", divider: "#d5ccb6", cyan: "#427b58", magenta: "#8f3f71", gold: "#9d5015", text: "#3c3836", muted: "#756f5e"))))
        func root(_ style: DesktopStyle) -> some View {
            KeyCapture(focus: focus, isEnabled: true, label: "Terminal input", onItems: { _ in true }).frame(width: 1, height: 1).environment(\.desktopStyle, style)
        }
        let host = UIHostingController(rootView: root(dark))
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 402, height: 874))
        window.rootViewController = host
        window.makeKeyAndVisible()
        host.view.layoutIfNeeded()
        let view = try XCTUnwrap(focus.view)
        XCTAssertTrue(view.becomeFirstResponder())
        view.bar.tapped(.control)
        XCTAssertTrue(view.mapper.controlArmed)
        XCTAssertEqual(view.bar.style, dark)

        host.rootView = root(light)
        for _ in 0..<80 where view.bar.style != light { host.view.layoutIfNeeded(); try await Task.sleep(for: .milliseconds(25)) }
        XCTAssertTrue(focus.view === view, "the same UIKit view: nothing was rebuilt")
        XCTAssertTrue(view.isFirstResponder, "the keyboard stayed up")
        XCTAssertTrue(focus.isActive)
        XCTAssertTrue(view.mapper.controlArmed, "sticky Ctrl survived")
        XCTAssertEqual(view.bar.style, light, "and the bar has the new colors")
        XCTAssertEqual(view.bar.backgroundColor, light.panelUI)
        XCTAssertEqual(view.bar.buttons[.control]?.configuration?.baseForegroundColor, light.accentUI, "armed Ctrl uses the new accent")
        XCTAssertEqual(view.bar.buttons[.key(.escape)]?.configuration?.baseForegroundColor, light.textUI)
        _ = view.resignFirstResponder()
        window.isHidden = true
    }
}
