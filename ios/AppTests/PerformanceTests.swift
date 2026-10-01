import XCTest
import SwiftUI
import UIKit
import Observation
import RiWorkCore
@testable import RiWorkRemote

/// What the phone itself spends on a live terminal: the main thread's CPU time per answer (decode, apply, SwiftUI, the surface, the
/// painting), per frame of a scroll, and per row painted, against a scripted desktop with 50,000 lines of styled scrollback.
///
/// The times are for reading (they print `PERF …`); what the tests assert is the work done: how many views were rebuilt, how many rows
/// were painted, how many round trips a reconnect takes. Main-thread CPU time (not wall time) is what a frame is made of, and it
/// does not count the waits of the test itself.
@MainActor final class PerformanceTests: ScrollTestCase {
    @MainActor struct Hosted {
        let model: RemoteModel, transport: FixtureTransport, keychain: KeychainStore, window: UIWindow, host: UIHostingController<AnyView>
        var surface: TerminalSurfaceView? {
            func find(_ view: UIView) -> TerminalSurfaceView? {
                if let found = view as? TerminalSurfaceView { return found }
                for sub in view.subviews { if let found = find(sub) { return found } }
                return nil
            }
            return find(host.view)
        }
    }

    // MARK: Measuring

    private lazy var mainThread = mach_thread_self()
    /// CPU time the main thread has used so far, in seconds.
    private func cpu() -> Double {
        var info = thread_basic_info()
        var count = mach_msg_type_number_t(MemoryLayout<thread_basic_info>.size / MemoryLayout<integer_t>.size)
        let result = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { thread_info(mainThread, thread_flavor_t(THREAD_BASIC_INFO), $0, &count) }
        }
        guard result == KERN_SUCCESS else { return 0 }
        func seconds(_ t: time_value_t) -> Double { Double(t.seconds) + Double(t.microseconds) / 1e6 }
        return seconds(info.user_time) + seconds(info.system_time)
    }
    private func report(_ name: String, _ value: Double, unit: String = "ms", per: String = "") {
        print(String(format: "PERF %-52@ %9.4f %@%@", name as NSString, value, unit as NSString, (per.isEmpty ? "" : " / \(per)") as NSString))
    }

    // MARK: Rig

    private func richScrollback(history: Int, rows: Int = 40) -> ScriptedScrollback {
        var scrollback = ScriptedScrollback(history: history, rows: rows)
        scrollback.content = { SyntheticTranscript.line($0) }
        return scrollback
    }
    /// The real terminal screen in a window, following a scripted desktop with `history` lines of styled scrollback. With `prefetch`
    /// the lines are loaded the way the app loads them (in the background, a page at a time) and this returns once they are held.
    private func hosted(history: Int, prefetch: Bool = true) async throws -> Hosted {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene to host the screen in") }
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom == .pad, "the iPhone terminal")
        let (model, transport, keychain) = try await rig(richScrollback(history: history), prefetch: prefetch)
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data(#"{"id":"\#(project)","name":"Fixture","root":"/fixture","created_at":1}"#.utf8))
        let host = UIHostingController(rootView: AnyView(TerminalTabsView(model: model, project: projectValue, onBack: {}).desktopThemed(model.theme.style)))
        let window = UIWindow(windowScene: scene)
        window.rootViewController = host
        window.makeKeyAndVisible()
        let made = Hosted(model: model, transport: transport, keychain: keychain, window: window, host: host)
        await eventually("the terminal is on screen") { made.surface != nil }
        if prefetch {
            let wanted = min(history, HistoryLimits.heldLines) - 200
            await eventually("the history is held", timeout: 60) { model.terminal.heldHistory >= wanted || model.terminal.atTop }
        }
        await settle(made, 300)
        return made
    }
    private func finish(_ screen: Hosted) async {
        await screen.model.disconnect()
        screen.window.isHidden = true
        try? screen.keychain.delete()
    }
    private func settle(_ screen: Hosted, _ milliseconds: Int = 300) async {
        for _ in 0..<max(1, milliseconds / 30) { screen.host.view.layoutIfNeeded(); try? await Task.sleep(for: .milliseconds(30)) }
    }

    /// Runs `answers` live answers, each caused by `change` on the desktop, and reports what the main thread spent on them.
    private func liveAnswers(_ name: String, _ screen: Hosted, answers: Int, gap: Int = 18, change: (inout ScriptedScrollback) -> Void) async {
        let model = screen.model
        // The cost of just waiting for the same time, to be taken off.
        let idleStart = cpu()
        for _ in 0..<answers { try? await Task.sleep(for: .milliseconds(gap)) }
        let idle = cpu() - idleStart

        Perf.reset()
        let start = cpu()
        var applied = 0
        for _ in 0..<answers {
            var scrollback = await screen.transport.scriptedScrollback()!
            change(&scrollback)
            let before = model.outputVersion
            await screen.transport.setScrollback(scrollback)
            var waited = 0
            while model.outputVersion == before, waited < 400 { try? await Task.sleep(for: .milliseconds(2)); waited += 2 }
            if model.outputVersion != before { applied += 1 }
            try? await Task.sleep(for: .milliseconds(gap))
        }
        let used = cpu() - start - idle
        let n = Double(max(1, applied))
        report("\(name): main-thread CPU", max(0, used) * 1000 / n, per: "answer")
        let counts = Perf.counts
        func per(_ key: String) -> String { String(format: "%.2f", Double(counts[key] ?? 0) / n) }
        print("PERF \(name): per answer: TerminalTabsView body \(per("body.TerminalTabsView")), WorkspaceBar \(per("body.WorkspaceBar")), SessionConsole \(per("body.SessionConsole")), PhoneTerminal \(per("body.PhoneTerminal")), updateUIView \(per("update.TerminalSurface")), surface.refresh \(per("surface.refresh")), rows painted \(per("rowPaint"))")
        XCTAssertGreaterThan(applied, answers * 9 / 10, "the answers arrive")
        #if DEBUG || PERF_COUNTERS
        // A live answer changes the terminal and nothing around it: the header, the console and the phone terminal are not rebuilt for it.
        for key in ["body.TerminalTabsView", "body.WorkspaceBar", "body.SessionConsole", "body.PhoneTerminal"] {
            XCTAssertEqual(counts[key] ?? 0, 0, "\(name): \(key) was rebuilt by live answers")
        }
        XCTAssertLessThanOrEqual(Double(counts["surface.refresh"] ?? 0) / n, 1.2, "\(name): the surface is refreshed once per answer")
        #endif
    }

    // MARK: Typing

    /// A transport that notes when each `shell.keys` request reaches it, and answers the rest from the fixture.
    actor Stopwatch: RemoteTransport {
        let inner: FixtureTransport
        private(set) var keyArrivals: [ContinuousClock.Instant] = []
        init(_ inner: FixtureTransport) { self.inner = inner }
        func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { try await inner.connect(pairing: pairing, allowLocalDevelopment: allowLocalDevelopment) }
        func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
            if method == "shell.keys" { keyArrivals.append(.now) }
            return try await inner.request(method: method, params: params, id: id)
        }
        func disconnect() async { await inner.disconnect() }
        func isConnected() async -> Bool { await inner.isConnected() }
        func arrivals() -> [ContinuousClock.Instant] { keyArrivals }
    }

    /// Keys typed one at a time with the screen echoing each one: the time from `type` to the request reaching the transport, and what the
    /// main thread spends per key.
    func testTypingWithTheScreenEchoing() async throws {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene to host the screen in") }
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom == .pad, "the iPhone terminal")
        let keychain = try makeStore()
        defer { try? keychain.delete() }
        let inner = FixtureTransport()
        await inner.setHashMode(true, cap: .milliseconds(120))
        await inner.setScrollback(richScrollback(history: 3000), reportsHistorySize: true)
        let watch = Stopwatch(inner)
        let model = RemoteModel(client: watch, keychain: keychain, pollInterval: .milliseconds(40), keyFlushInterval: .milliseconds(40), previewDelay: .milliseconds(300),
                                reconnectBackoff: .milliseconds(10), defaults: scratchDefaults(), cellMetrics: { TerminalLayout.approximateCell(fontSize: $0) },
                                keepAwake: { _ in }, liveWaitMilliseconds: 200, linkWatcher: StaticLinkWatcher(), prefetch: false)
        model.setTerminalVisible(true)
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data(#"{"id":"\#(project)","name":"Fixture","root":"/fixture","created_at":1}"#.utf8))
        let host = UIHostingController(rootView: AnyView(TerminalTabsView(model: model, project: projectValue, onBack: {}).desktopThemed(model.theme.style)))
        let window = UIWindow(windowScene: scene)
        window.rootViewController = host
        window.makeKeyAndVisible()
        await model.connect()
        await eventually("the first screen is in") { model.outputSessionID == self.shell && !model.terminal.isEmpty }
        for _ in 0..<10 { host.view.layoutIfNeeded(); try? await Task.sleep(for: .milliseconds(30)) }
        model.keysSupport = .supported

        let keys = 80
        // The cost of just waiting for the same time, to be taken off.
        let idleStart = cpu()
        for _ in 0..<keys { try? await Task.sleep(for: .milliseconds(60)) }
        let idle = cpu() - idleStart
        Perf.reset()
        let start = cpu()
        var sentAt: [ContinuousClock.Instant] = []
        for i in 0..<keys {
            sentAt.append(.now)
            XCTAssertEqual(model.type([.text(String(UnicodeScalar(UInt8(97 + i % 26))))]), .accepted)
            // The desktop echoes it.
            var scrollback = await inner.scriptedScrollback()!
            scrollback.echo += "x"
            await inner.setScrollback(scrollback)
            try? await Task.sleep(for: .milliseconds(60))
        }
        try? await Task.sleep(for: .milliseconds(400))
        let used = max(0, cpu() - start - idle)
        let arrivals = await watch.arrivals()
        XCTAssertGreaterThanOrEqual(arrivals.count, keys / 2)
        var latencies: [Double] = []
        for (i, arrival) in arrivals.enumerated() where i < sentAt.count {
            let d = (arrival - sentAt[i]).components
            latencies.append(Double(d.seconds) * 1000 + Double(d.attoseconds) / 1e15)
        }
        latencies.sort()
        report("typing: key → request at the transport (median)", latencies[latencies.count / 2])
        report("typing: key → request at the transport (worst)", latencies.last ?? 0)
        report("typing: main-thread CPU", used * 1000 / Double(keys), per: "key")
        let counts = Perf.counts
        func per(_ key: String) -> String { String(format: "%.2f", Double(counts[key] ?? 0) / Double(keys)) }
        print("PERF typing: per key: TerminalTabsView body \(per("body.TerminalTabsView")), WorkspaceBar \(per("body.WorkspaceBar")), SessionConsole \(per("body.SessionConsole")), PhoneTerminal \(per("body.PhoneTerminal")), updateUIView \(per("update.TerminalSurface")), surface.refresh \(per("surface.refresh")), rows painted \(per("rowPaint"))")
        print("PERF typing: key batches sent \(arrivals.count) for \(keys) keys")
        #if DEBUG || PERF_COUNTERS
        // A key typed rebuilds the part of the screen that shows the keys pending, and nothing else.
        for key in ["body.TerminalTabsView", "body.WorkspaceBar", "body.SessionConsole", "body.PhoneTerminal"] {
            XCTAssertEqual(counts[key] ?? 0, 0, "\(key) was rebuilt by typing")
        }
        XCTAssertLessThanOrEqual(Double(counts["surface.refresh"] ?? 0) / Double(keys), 1.2, "the surface is refreshed once per answer, not once more per key")
        #endif
        await model.disconnect()
        window.isHidden = true
    }

    // MARK: Scrolling

    /// A fling: the offset moves `step` points per frame, and every frame is laid out and drawn.
    private func fling(_ name: String, _ screen: Hosted, step: Double, frames: Int = 400) async throws {
        let surface = try XCTUnwrap(screen.surface)
        let scroll = surface.scrollView
        let g = surface.geometry
        var offset = g.maxOffset - 3 * g.viewportHeight
        scroll.setContentOffset(CGPoint(x: 0, y: offset), animated: false)
        screen.host.view.layoutIfNeeded(); CATransaction.flush()
        try? await Task.sleep(for: .milliseconds(100))
        Perf.reset()
        let start = cpu()
        var worst = 0.0
        for frame in 0..<frames {
            let began = cpu()
            offset -= step * (frame < frames / 2 ? 1 : -1)
            scroll.contentOffset = CGPoint(x: 0, y: offset)
            screen.host.view.layoutIfNeeded()
            CATransaction.flush()
            worst = max(worst, cpu() - began)
        }
        let used = cpu() - start
        report("\(name): main-thread CPU", used * 1000 / Double(frames), per: "frame (worst \(String(format: "%.2f", worst * 1000)) ms)")
        let painted = Double(Perf.counts["rowPaint"] ?? 0)
        print(String(format: "PERF %@: rows painted %.2f / frame, %.1f rows scrolled / frame", name, painted / Double(frames), step / g.lineHeight))
    }

    /// One screen, loaded once: live answers that only edit the last line (what typing causes), live answers that scroll two lines in
    /// (what output causes), and scrolling at three speeds.
    func testLiveAnswersAndScrollingWithFiftyThousandLines() async throws {
        let screen = try await hosted(history: 50_000)
        print("PERF held lines: \(screen.model.terminal.lines.count), asked for \(screen.model.liveScrollbackLines) lines of scrollback per answer")
        await liveAnswers("echo (last line edited), 50k held", screen, answers: 120) { $0.echo += "x" }
        await liveAnswers("output (+2 lines), 50k held", screen, answers: 120) { $0.write(2) }
        let line = TerminalFont.cell(size: TerminalFontSize.standard).height
        try await fling("slow scroll (1 row/frame)", screen, step: line)
        try await fling("fling 3000 pt/s at 120 Hz", screen, step: 25)
        try await fling("fast fling 8000 pt/s at 120 Hz", screen, step: 67)
        try await pinch(screen)
        await finish(screen)
    }

    /// A pinch: the text size changes with every frame of the gesture, and the whole screen is laid out and painted at the new size.
    private func pinch(_ screen: Hosted) async throws {
        let sizes: [Double] = Array(stride(from: 12.0, through: 18.0, by: 1)) + Array(stride(from: 17.0, through: 12.0, by: -1))
        Perf.reset()
        var worst = 0.0
        let start = cpu()
        for size in sizes {
            let began = cpu()
            screen.model.setTerminalFontSize(size)
            screen.host.view.layoutIfNeeded(); CATransaction.flush()
            try? await Task.sleep(for: .milliseconds(20))
            worst = max(worst, cpu() - began)
        }
        let idle = 0.0
        report("pinch (text size step): main-thread CPU", (cpu() - start - idle) * 1000 / Double(sizes.count), per: "step (worst \(String(format: "%.1f", worst * 1000)) ms)")
        print(String(format: "PERF pinch: rows painted %.1f / step", Double(Perf.counts["rowPaint"] ?? 0) / Double(sizes.count)))
    }

    // MARK: For a profiler

    /// Runs one scenario for a while so that Instruments can be attached: `TEST_RUNNER_PERF_PROFILE=echo|output|scroll xcodebuild test
    /// -only-testing:RiWorkAppTests/PerformanceTests/testForAProfiler`. Skipped otherwise.
    func testForAProfiler() async throws {
        guard let kind = ProcessInfo.processInfo.environment["PERF_PROFILE"]?.lowercased() else { throw XCTSkip("set PERF_PROFILE to run a scenario for a profiler") }
        let screen = try await hosted(history: 50_000)
        print("PROFILE READY \(kind)")
        let end = Date().addingTimeInterval(40)
        switch kind {
        case "scroll":
            let surface = try XCTUnwrap(screen.surface)
            var offset = surface.geometry.maxOffset - 3 * surface.geometry.viewportHeight
            var direction = -1.0
            while Date() < end {
                offset += direction * 40
                if offset < surface.geometry.minOffset + 4000 || offset > surface.geometry.maxOffset - 2000 { direction = -direction }
                surface.scrollView.contentOffset = CGPoint(x: 0, y: offset)
                screen.host.view.layoutIfNeeded(); CATransaction.flush()
                try? await Task.sleep(for: .milliseconds(8))
            }
        case "type":
            var i = 0
            while Date() < end {
                _ = screen.model.type([.text(String(UnicodeScalar(UInt8(97 + i % 26))))]); i += 1
                var scrollback = await screen.transport.scriptedScrollback()!
                scrollback.echo += "x"; if scrollback.echo.count > 30 { scrollback.echo = "" }
                await screen.transport.setScrollback(scrollback)
                try? await Task.sleep(for: .milliseconds(45))
            }
        default:
            while Date() < end {
                var scrollback = await screen.transport.scriptedScrollback()!
                if kind == "output" { scrollback.write(2) } else { scrollback.echo += "x"; if scrollback.echo.count > 30 { scrollback.echo = "" } }
                let before = screen.model.outputVersion
                await screen.transport.setScrollback(scrollback)
                var waited = 0
                while screen.model.outputVersion == before, waited < 400 { try? await Task.sleep(for: .milliseconds(1)); waited += 1 }
                try? await Task.sleep(for: .milliseconds(15))
            }
        }
        print("PROFILE DONE")
        await finish(screen)
    }

    // MARK: Painting

    func testPaintingRows() throws {
        let scale = 3.0
        TerminalFont.pixelsPerPoint = scale
        let settings = TerminalRenderer.Settings(style: .builtIn, dark: true, showCursor: true, committedSize: TerminalFontSize.standard, boldIsBright: false)
        let lines = (0..<400).map { TerminalText.styledLines(page: SyntheticTranscript.line(1000 + $0), expecting: 1)[0] }
        let cell = TerminalFont.cell(size: TerminalFontSize.standard)
        let size = CGSize(width: 390, height: cell.height)
        let format = UIGraphicsImageRendererFormat(); format.scale = scale; format.opaque = false
        let renderer = UIGraphicsImageRenderer(size: size, format: format)
        func paint(_ line: StyledLine) { _ = renderer.image { context in TerminalRowPainter.draw(line, cursorColumn: nil, settings: settings, fontSize: TerminalFontSize.standard, in: context.cgContext, scale: scale) } }
        func empty() { _ = renderer.image { _ in } }
        for line in lines.prefix(40) { paint(line) }
        let clock = ContinuousClock()
        var best = Double.infinity, bestEmpty = Double.infinity
        for _ in 0..<7 {
            let a = clock.now
            for line in lines { paint(line) }
            let b = clock.now
            for _ in lines { empty() }
            let c = clock.now
            func ms(_ d: Duration) -> Double { Double(d.components.seconds) * 1000 + Double(d.components.attoseconds) / 1e15 }
            best = min(best, ms(b - a) / Double(lines.count)); bestEmpty = min(bestEmpty, ms(c - b) / Double(lines.count))
        }
        report("paint one row (with bitmap setup)", best, per: "row")
        report("bitmap setup alone", bestEmpty, per: "row")
        report("paint one row (Core Text work)", best - bestEmpty, per: "row")
    }

    // MARK: Reconnect

    /// A transport that takes `delay` for every request, like a relay a round trip away.
    actor SlowLink: RemoteTransport {
        let inner: FixtureTransport
        let delay: Duration
        private(set) var requests: [String] = []
        init(_ inner: FixtureTransport, delay: Duration) { self.inner = inner; self.delay = delay }
        func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing {
            try await Task.sleep(for: delay * 3)   // the handshake is a few round trips
            return try await inner.connect(pairing: pairing, allowLocalDevelopment: allowLocalDevelopment)
        }
        func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
            requests.append(method)
            try await Task.sleep(for: delay)
            let result = try await inner.request(method: method, params: params, id: id)
            try await Task.sleep(for: delay)
            return result
        }
        func disconnect() async { await inner.disconnect() }
        func isConnected() async -> Bool { await inner.isConnected() }
        func methods() -> [String] { requests }
    }

    /// The two ways the phone gets a screen over a link a round trip away: the first connect of a launch, and the return from the
    /// background (the terminal is on screen and its grid is known, so the resize comes first, then the screen).
    func testTimeFromConnectToTheFirstScreen() async throws {
        let keychain = try makeStore()
        defer { try? keychain.delete() }
        let inner = FixtureTransport()
        await inner.setHashMode(true, cap: .milliseconds(120))
        await inner.setScrollback(richScrollback(history: 2000), reportsHistorySize: true)
        let delay = Duration.milliseconds(25)
        let link = SlowLink(inner, delay: delay)
        let model = RemoteModel(client: link, keychain: keychain, pollInterval: .milliseconds(40), keyFlushInterval: .milliseconds(5), previewDelay: .milliseconds(80),
                                reconnectBackoff: .milliseconds(10), defaults: scratchDefaults(), cellMetrics: { TerminalLayout.approximateCell(fontSize: $0) },
                                keepAwake: { _ in }, liveWaitMilliseconds: 200, linkWatcher: StaticLinkWatcher(), prefetch: false)
        model.setTerminalVisible(true)
        model.reportTerminalArea(CGSize(width: 390, height: 600))
        func milliseconds(_ d: Duration) -> Double { Double(d.components.seconds) * 1000 + Double(d.components.attoseconds) / 1e15 }
        let clock = ContinuousClock()

        var began = clock.now
        await model.connect()
        await eventually("the first screen is in", timeout: 10) { model.outputSessionID == self.shell && !model.terminal.isEmpty && !model.snapshotStale }
        report("first connect → first screen (round trip \(delay * 2))", milliseconds(clock.now - began))
        print("PERF first connect: requests in order: \(await link.methods().prefix(12).joined(separator: " → "))")

        await model.disconnect(background: true)
        XCTAssertTrue(model.snapshotStale)
        let used = await link.methods().count
        began = clock.now
        await model.resume()
        await eventually("the screen is fresh again", timeout: 10) { !model.snapshotStale && model.viewportReady }
        report("resume from background → fresh screen (round trip \(delay * 2))", milliseconds(clock.now - began))
        let sequence = Array(await link.methods().dropFirst(used))
        print("PERF resume: requests in order: \(sequence.prefix(12).joined(separator: " → "))")
        await model.disconnect()
    }

    /// Holds the four list requests of a (re)connect until all four are on the wire. A model that asks for them one after the other never
    /// gets there, and the test says so.
    actor Barrier: RemoteTransport {
        let inner: FixtureTransport
        private let lists: Set<String> = ["projects.list", "orchestrators.list", "worktrees.list", "shells.list"]
        private(set) var arrived: Set<String> = []
        private(set) var released = false
        init(_ inner: FixtureTransport) { self.inner = inner }
        func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { try await inner.connect(pairing: pairing, allowLocalDevelopment: allowLocalDevelopment) }
        func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
            if lists.contains(method), !released {
                arrived.insert(method)
                let deadline = ContinuousClock.now + .seconds(2)
                while arrived.count < lists.count, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(2)) }
                if arrived.count == lists.count { released = true }
            }
            return try await inner.request(method: method, params: params, id: id)
        }
        func disconnect() async { await inner.disconnect() }
        func isConnected() async -> Bool { await inner.isConnected() }
        func allArrivedTogether() -> Bool { released }
    }

    func testTheListsOfAConnectGoOutTogether() async throws {
        let keychain = try makeStore()
        defer { try? keychain.delete() }
        let barrier = Barrier(FixtureTransport())
        let model = RemoteModel(client: barrier, keychain: keychain, pollInterval: .milliseconds(40), keyFlushInterval: .milliseconds(5), previewDelay: .milliseconds(80),
                                reconnectBackoff: .milliseconds(10), defaults: scratchDefaults(), cellMetrics: { TerminalLayout.approximateCell(fontSize: $0) },
                                keepAwake: { _ in }, liveWaitMilliseconds: 200, linkWatcher: StaticLinkWatcher(), prefetch: false)
        await model.connect()
        let together = await barrier.allArrivedTogether()
        XCTAssertTrue(together, "projects, orchestrators, worktrees and shells are asked for in one round trip")
        XCTAssertEqual(model.state, .connected)
        XCTAssertEqual(model.projects.count, 1)
        XCTAssertEqual(model.shells.map(\.id), [shell])
        XCTAssertEqual(model.sessionID, shell)
        await model.disconnect()
    }

    // MARK: What views read

    /// Views read `hasOutput` instead of `output`: it changes when the screen is first filled or emptied, not with every answer.
    func testHasOutputFollowsOutputButOnlyChangesWithEmptiness() throws {
        let model = RemoteModel(client: FixtureTransport(), keychain: try makeStore(), defaults: scratchDefaults())
        XCTAssertFalse(model.hasOutput)
        final class Wakes: @unchecked Sendable { var count = 0 }
        let wakes = Wakes()
        func watch() { withObservationTracking { _ = model.hasOutput } onChange: { wakes.count += 1 } }
        watch()
        model.output = "first screen"
        XCTAssertTrue(model.hasOutput)
        XCTAssertEqual(wakes.count, 1, "the first screen wakes the readers")
        watch()
        model.output = "a second, different screen"
        model.output = "a third"
        XCTAssertEqual(wakes.count, 1, "new text does not")
        model.output = ""
        XCTAssertFalse(model.hasOutput)
        XCTAssertEqual(wakes.count, 2, "emptying the screen does")
    }

    // MARK: Launch

    /// What the app does on the main thread before its first frame that is its own: reading the paired desktops from the Keychain, the
    /// last palette from the defaults, and setting the model up.
    func testModelSetUpAtLaunch() throws {
        let keychain = try makeStore()
        defer { try? keychain.delete() }
        let defaults = scratchDefaults()
        let transport = FixtureTransport()
        var best = Double.infinity
        let clock = ContinuousClock()
        for _ in 0..<20 {
            let began = clock.now
            let model = RemoteModel(client: transport, keychain: keychain, defaults: defaults)
            let d = (clock.now - began).components
            best = min(best, Double(d.seconds) * 1000 + Double(d.attoseconds) / 1e15)
            XCTAssertEqual(model.desktops.count, 1)
        }
        report("launch: RemoteModel.init (Keychain read, defaults, theme)", best)
    }
}
