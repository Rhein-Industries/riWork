import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// The real terminal screen on an iPhone, hosted in a window, against the scripted desktop. A user's finger is emulated by moving the
/// scroll view's offset, which is all a finger does to the surface (it has no other state to correct).
@MainActor final class ScrollingUITests: ScrollTestCase {
    struct Screen {
        let model: RemoteModel, transport: FixtureTransport, keychain: KeychainStore, window: UIWindow, host: UIHostingController<AnyView>
    }
    private func screen(_ scrollback: ScriptedScrollback, alternate: Bool? = nil, prefetch: Bool = false,
                        watcher: StaticLinkWatcher = StaticLinkWatcher(), link: (fixed: Duration, bytesPerSecond: Double?)? = nil) async throws -> Screen {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene to host the screen in") }
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom == .pad, "the iPhone terminal")
        let (model, transport, keychain) = try await rig(scrollback, alternate: alternate, prefetch: prefetch, watcher: watcher, link: link)
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data(#"{"id":"\#(project)","name":"Fixture","root":"/fixture","created_at":1}"#.utf8))
        let host = UIHostingController(rootView: AnyView(TerminalTabsView(model: model, project: projectValue, onBack: {}).desktopThemed(model.theme.style)))
        let window = UIWindow(windowScene: scene)
        window.rootViewController = host
        window.makeKeyAndVisible()
        let made = Screen(model: model, transport: transport, keychain: keychain, window: window, host: host)
        await eventually("the terminal is on screen") { self.surface(made) != nil }
        await settle(made, 400)
        return made
    }
    private func finish(_ screen: Screen) async {
        await screen.model.disconnect()
        screen.window.isHidden = true
        try? screen.keychain.delete()
    }
    /// Runs `body` on a fresh screen and always tears it down.
    private func withScreen(_ scrollback: ScriptedScrollback, alternate: Bool? = nil, prefetch: Bool = false, watcher: StaticLinkWatcher = StaticLinkWatcher(),
                            link: (fixed: Duration, bytesPerSecond: Double?)? = nil, _ body: @MainActor (Screen) async throws -> Void) async throws {
        let s = try await screen(scrollback, alternate: alternate, prefetch: prefetch, watcher: watcher, link: link)
        do { try await body(s) } catch { await finish(s); throw error }
        await finish(s)
    }
    private func settle(_ screen: Screen, _ milliseconds: Int = 300) async {
        for _ in 0..<max(1, milliseconds / 30) { screen.host.view.layoutIfNeeded(); try? await Task.sleep(for: .milliseconds(30)) }
    }
    private func surfaces(in view: UIView) -> [TerminalSurfaceView] {
        var found: [TerminalSurfaceView] = []
        if let surface = view as? TerminalSurfaceView { found.append(surface) }
        for sub in view.subviews { found += surfaces(in: sub) }
        return found
    }
    private func surface(_ screen: Screen) -> TerminalSurfaceView? { surfaces(in: screen.host.view).first }
    private var lineHeight: Double { TerminalFont.cell(size: TerminalFontSize.standard).height }
    private func offset(_ surface: TerminalSurfaceView) -> Double { Double(surface.scrollView.contentOffset.y) }
    private func distanceFromBottom(_ surface: TerminalSurfaceView) -> Double { surface.geometry.distanceFromBottom(offset: offset(surface)) }
    /// The text of the rows in view, top to bottom, with where each sits in the view.
    private func visible(_ surface: TerminalSurfaceView) -> [(index: Int, y: Double, text: String)] {
        surface.shownRows.keys.sorted().compactMap { index in
            guard let line = surface.shownRows[index]?.line else { return nil }
            return (index, surface.geometry.y(ofRow: index, offset: offset(surface)), line.text)
        }
    }

    /// A finger: moves the view to `offset`.
    private func drag(_ surface: TerminalSurfaceView, to offset: Double, in screen: Screen) async {
        surface.scrollView.contentOffset = CGPoint(x: 0, y: offset)
        await settle(screen, 90)
    }

    // MARK: vertical only, opening at the bottom

    func testTheTerminalScrollsOnlyVerticallyClipsLongLinesAndOpensAtTheBottom() async throws {
        var scrollback = ScriptedScrollback(history: 3000)
        scrollback.longLine = 2990
        try await withScreen(scrollback) { s in
            let view = try XCTUnwrap(surface(s))
            XCTAssertLessThanOrEqual(view.scrollView.contentSize.width, view.bounds.width + 0.5, "a 400-character line does not widen the content")
            XCTAssertFalse(view.scrollView.alwaysBounceHorizontal)
            XCTAssertEqual(view.geometry.endRow, s.model.terminal.endIndex)
            XCTAssertLessThanOrEqual(distanceFromBottom(view), lineHeight, "opens pinned to the bottom")
            XCTAssertTrue(s.model.scrollFollow.following)
            let rows = view.shownRows.values
            XCTAssertTrue(rows.allSatisfy { $0.frame.maxX <= view.bounds.width + 0.5 }, "rows are as wide as the pane, never wider")
            XCTAssertGreaterThan(rows.count, 20, "a screenful of rows is drawn, and not 3,000")
            XCTAssertLessThan(rows.count, 120)
            XCTAssertEqual(view.scrollView.contentOffset.x, 0)
        }
    }

    func testRowsAreWholePixelsTallAndSitOnThePixelGrid() async throws {
        try await withScreen(ScriptedScrollback(history: 500)) { s in
            let view = try XCTUnwrap(surface(s))
            let scale = Double(view.traitCollection.displayScale)
            let pitch = view.geometry.lineHeight
            XCTAssertEqual(pitch * scale, (pitch * scale).rounded(), accuracy: 0.0001, "the line height is a whole number of device pixels")
            XCTAssertEqual(pitch, TerminalFont.cell(size: TerminalFontSize.standard).height, "and it is what the desktop grid is worked out from")
            for row in view.shownRows.values { XCTAssertEqual(Double(row.frame.height), pitch, accuracy: 0.0001) }
        }
    }

    func testNewOutputIsFollowedWhileAtTheBottom() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            let height = view.scrollView.contentSize.height
            await s.transport.write(6)
            await eventually("the lines are in") { s.model.terminal.screenTop == 3006 }
            await settle(s, 400)
            XCTAssertEqual(view.scrollView.contentSize.height, height + 6 * lineHeight, accuracy: 0.5)
            XCTAssertLessThanOrEqual(distanceFromBottom(view), lineHeight, "and the view went down with them")
            XCTAssertTrue(s.model.scrollFollow.following)
            XCTAssertNil(s.model.scrollFollow.pill)
            XCTAssertEqual(visible(view).last?.text, "L3017", "the newest line is the last one in view")
        }
    }

    func testWithinOneLineOfTheBottomStillFollows() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            await drag(view, to: view.geometry.maxOffset - lineHeight * 0.6, in: s)
            XCTAssertTrue(s.model.scrollFollow.following, "half a line up is still the bottom")
            await s.transport.write(2)
            await eventually("in") { s.model.terminal.screenTop == 3002 }
            await settle(s, 400)
            XCTAssertLessThanOrEqual(distanceFromBottom(view), lineHeight)
        }
    }

    // MARK: sticky bottom

    func testScrolledUpNothingMovesWhileOutputArrivesAndThePillCountsAndBringsTheReaderBack() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            await drag(view, to: view.geometry.maxOffset - 400, in: s)
            XCTAssertFalse(s.model.scrollFollow.following)
            XCTAssertNil(s.model.scrollFollow.pill, "nothing new yet, and not far away")
            let before = offset(view)
            let seen = visible(view)
            await s.transport.write(7)
            await eventually("new output is in") { s.model.terminal.screenTop == 3007 }
            await settle(s, 500)
            XCTAssertEqual(offset(view), before, "what is being read did not move")
            XCTAssertEqual(visible(view).map(\.text), seen.map(\.text))
            XCTAssertEqual(visible(view).map(\.y), seen.map(\.y), "not by a point")
            XCTAssertEqual(s.model.scrollFollow.newLines, 7)
            XCTAssertEqual(s.model.scrollFollow.pill?.label, "↓ Live · 7 new")
            // the pill
            s.model.jumpToLatest()
            await settle(s, 500)
            XCTAssertLessThanOrEqual(distanceFromBottom(view), lineHeight, "tapping the pill went to the bottom")
            XCTAssertTrue(s.model.scrollFollow.following)
            XCTAssertEqual(s.model.scrollFollow.newLines, 0)
            XCTAssertNil(s.model.scrollFollow.pill)
            // and follows again
            await s.transport.write(2)
            await eventually("in") { s.model.terminal.screenTop == 3009 }
            await settle(s, 400)
            XCTAssertLessThanOrEqual(distanceFromBottom(view), lineHeight)
        }
    }

    func testTypingBringsTheReaderBackToThePrompt() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            await drag(view, to: view.geometry.maxOffset - 2500, in: s)
            XCTAssertFalse(s.model.scrollFollow.following)
            s.model.type(KeyMapper.items(for: "l"))
            XCTAssertTrue(s.model.scrollFollow.following)
            await settle(s, 500)
            XCTAssertLessThanOrEqual(distanceFromBottom(view), lineHeight)
        }
    }

    func testAReaderFarFromTheBottomGetsThePillEvenWithoutNewOutput() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            await drag(view, to: view.geometry.maxOffset - 4000, in: s)
            XCTAssertEqual(s.model.scrollFollow.pill?.label, "↓ Live")
        }
    }

    func testASwitchToAnotherShellStartsAtItsBottom() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            await s.transport.setSessions([try session(shell), try session(other)])
            await s.model.refresh()
            let view = try XCTUnwrap(surface(s))
            await drag(view, to: view.geometry.maxOffset - 3000, in: s)
            XCTAssertFalse(s.model.scrollFollow.following)
            await s.model.chooseSession(try session(other))
            await eventually("the other shell is shown") { s.model.outputSessionID == self.other && !s.model.terminal.isEmpty }
            await settle(s, 600)
            let switched = try XCTUnwrap(surface(s))
            XCTAssertLessThanOrEqual(distanceFromBottom(switched), lineHeight, "pinned to the bottom of the other shell")
            XCTAssertTrue(s.model.scrollFollow.following)
        }
    }

    func testTheBottomStaysClearOfThePendingInputChipWhileFollowing() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            // Keys the desktop refuses are held and shown in a chip that floats over the bottom of the pane.
            await s.transport.setKeysMode(.inputUnavailable)
            s.model.type(KeyMapper.items(for: "x"))
            await eventually("the chip is up") { s.model.keyPreview != nil }
            await settle(s, 500)
            XCTAssertGreaterThan(view.scrollView.contentInset.bottom, 10, "room is kept under the last line for the chip")
            XCTAssertLessThanOrEqual(distanceFromBottom(view), lineHeight)
            await s.transport.write(4)
            await eventually("in") { s.model.terminal.screenTop == 3004 }
            await settle(s, 500)
            XCTAssertLessThanOrEqual(distanceFromBottom(view), lineHeight, "the last line is above the chip, not under it")
            XCTAssertTrue(s.model.scrollFollow.following, "and following was not lost on the way")
        }
    }

    // MARK: history: nothing moves when lines are added above

    func testAPageIsPrependedWithoutMovingTheReaderByAPoint() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            // Near the top of what is loaded, in the middle of a drag: the worst moment for a page.
            let start = s.model.terminal.start
            await drag(view, to: view.geometry.minOffset + 6 * lineHeight, in: s)
            let before = (offset: offset(view), rows: visible(view), height: view.scrollView.contentSize.height, inset: Double(view.scrollView.contentInset.top))
            s.model.loadOlderHistory()
            await eventually("the page is in") { s.model.terminal.heldHistory == 800 }
            await settle(s, 300)
            XCTAssertEqual(s.model.terminal.start, start - 300)
            XCTAssertEqual(offset(view), before.offset, "the offset did not change")
            let after = visible(view)
            XCTAssertEqual(after.map(\.index), before.rows.map(\.index), "the same lines are in view")
            XCTAssertEqual(after.map(\.text), before.rows.map(\.text))
            XCTAssertEqual(after.map(\.y), before.rows.map(\.y), "each of them exactly where it was")
            XCTAssertEqual(view.scrollView.contentSize.height, before.height, "nothing was added below")
            XCTAssertEqual(Double(view.scrollView.contentInset.top), before.inset + 300 * lineHeight, accuracy: 0.001, "the room above grew by the lines added (the top inset is the negative of the way up)")
            XCTAssertEqual(-Double(view.scrollView.adjustedContentInset.top), view.geometry.minOffset, accuracy: 0.001, "and UIKit's own limit is where the geometry says")
            XCTAssertEqual(view.geometry.minOffset, before.offset - 6 * lineHeight - 300 * lineHeight, accuracy: lineHeight + 0.5)
            assertConsistent(s.model)
            XCTAssertFalse(s.model.scrollFollow.following)
        }
    }

    func testAReaderStandingAtTheTopOfWhatIsLoadedIsNotMovedByAPageEither() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            await drag(view, to: view.geometry.minOffset, in: s)
            let before = (offset: offset(view), rows: visible(view))
            s.model.loadOlderHistory()
            await eventually("the page is in") { s.model.terminal.heldHistory == 800 }
            await settle(s, 300)
            XCTAssertEqual(offset(view), before.offset, "UIKit does not carry the offset along with the inset")
            // The lines that were in view are where they were; the page's lines are above them, where the header was.
            let after = visible(view).filter { line in before.rows.contains { $0.index == line.index } }
            XCTAssertEqual(after.map(\.y), before.rows.map(\.y))
            XCTAssertEqual(after.map(\.text), before.rows.map(\.text))
            XCTAssertGreaterThan(visible(view).count, before.rows.count, "and the lines that came show above them")
            XCTAssertGreaterThan(offset(view) - view.geometry.minOffset, 299 * lineHeight, "and there is room to scroll up into the lines that came")
        }
    }

    func testTheTopOfWhatIsLoadedIsAWallAndMovesUpWhenAPageArrives() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            let scroll = view.scrollView
            // The limits UIKit scrolls between are the ones the geometry says: the first row (the header's) under the top padding,
            // and the last line above the bottom padding.
            XCTAssertEqual(-Double(scroll.adjustedContentInset.top), view.geometry.minOffset, accuracy: 0.001)
            XCTAssertEqual(Double(scroll.contentSize.height + scroll.adjustedContentInset.bottom - scroll.bounds.height), view.geometry.maxOffset, accuracy: 0.001)
            XCTAssertEqual(view.geometry.firstRow, s.model.terminal.start - 1, "the header row is the first")
            await drag(view, to: view.geometry.minOffset, in: s)
            XCTAssertTrue(view.headerShown)
            XCTAssertEqual(view.geometry.y(ofRow: view.geometry.firstRow, offset: offset(view)), 4, "the header sits under the top padding")
            s.model.loadOlderHistory()
            await eventually("page") { s.model.terminal.heldHistory == 800 }
            await settle(s, 200)
            XCTAssertEqual(-Double(scroll.adjustedContentInset.top), view.geometry.minOffset, accuracy: 0.001, "the wall moved up with the page")
            XCTAssertEqual(view.geometry.firstRow, s.model.terminal.start - 1)
            XCTAssertEqual(view.geometry.minOffset, Double(s.model.terminal.start - 1) * lineHeight - 4, accuracy: 0.001)
            // The sticky bottom still ends at the last line.
            await drag(view, to: view.geometry.maxOffset, in: s)
            XCTAssertEqual(distanceFromBottom(view), 0, accuracy: 0.5)
        }
    }

    func testPagesKeepComingAsTheReaderGoesUpAndEndAtTheBeginning() async throws {
        try await withScreen(ScriptedScrollback(history: 900)) { s in
            let view = try XCTUnwrap(surface(s))
            await drag(view, to: view.geometry.minOffset + 5 * lineHeight, in: s)
            s.model.loadOlderHistory()
            await eventually("the first page is in") { s.model.terminal.heldHistory == 800 }
            XCTAssertFalse(s.model.terminal.atTop)
            await settle(s, 200)
            await drag(view, to: view.geometry.minOffset + 5 * lineHeight, in: s)
            s.model.loadOlderHistory()
            await eventually("the last page is in") { s.model.terminal.atTop }
            await settle(s, 300)
            XCTAssertEqual(s.model.terminal.start, 0)
            XCTAssertEqual(s.model.terminal.heldHistory, 900)
            assertConsistent(s.model)
            await drag(view, to: view.geometry.minOffset, in: s)
            XCTAssertEqual(view.headerRowText, "Beginning of history")
            XCTAssertTrue(view.headerShown)
            let requests = await s.transport.historyRequests()
            XCTAssertEqual(requests.count, 2, "500 held, then 300, then the last 100: complete")
        }
    }

    func testTheHeaderRowSaysLoadingFailedAndRetriesOnATap() async throws {
        try await withScreen(ScriptedScrollback(history: 3000), prefetch: false) { s in
            await s.transport.setHistoryMode(.failing(code: "cli_error"))
            s.model.loadOlderHistory()
            await eventually("failed") { s.model.historyFailed }
            let view = try XCTUnwrap(surface(s))
            await drag(view, to: view.geometry.minOffset, in: s)
            XCTAssertEqual(view.headerRowText, "Couldn't load older lines · tap to retry")
            XCTAssertTrue(view.headerShown)
            await s.transport.setHistoryMode(.ok)
            // A tap on that row asks again.
            s.model.historyRetryAfter = nil
            let tap = try XCTUnwrap(view.scrollView.gestureRecognizers?.compactMap { $0 as? UITapGestureRecognizer }.first)
            XCTAssertTrue(tap.cancelsTouchesInView == false, "the tap does not take the touch from a scroll")
            s.model.retryHistory()
            await eventually("works again") { !s.model.historyFailed && s.model.terminal.heldHistory == 800 }
        }
    }

    // MARK: background fetch, end to end

    func testTheHistoryFillsInTheBackgroundOnAFastLinkWithoutTheReaderScrolling() async throws {
        try await withScreen(ScriptedScrollback(history: 6000), prefetch: true) { s in
            let view = try XCTUnwrap(surface(s))
            await eventually("everything is loaded", timeout: 15) { s.model.terminal.atTop }
            XCTAssertEqual(s.model.terminal.start, 0)
            XCTAssertEqual(s.model.terminal.heldHistory, 6000)
            assertConsistent(s.model)
            await settle(s, 300)
            XCTAssertTrue(s.model.scrollFollow.following, "the reader at the bottom was not disturbed")
            XCTAssertLessThanOrEqual(distanceFromBottom(view), lineHeight)
            // Scrolling to the very top is now local: no request goes out.
            let asked = await s.transport.historyRequests().count
            await drag(view, to: view.geometry.minOffset, in: s)
            await drag(view, to: view.geometry.minOffset + 2000 * lineHeight, in: s)
            let after = await s.transport.historyRequests().count
            XCTAssertEqual(after, asked)
            XCTAssertEqual(view.headerRowText, "Beginning of history")
            let peak = await s.transport.peakInFlight("shell.history")
            XCTAssertEqual(peak, 1, "one request at a time")
        }
    }

    // MARK: alternate screen

    func testAFullScreenProgramReplacesTheSurfaceAndNormalScrollingComesBack() async throws {
        try await withScreen(ScriptedScrollback(history: 3000), alternate: false) { s in
            XCTAssertNotNil(surface(s))
            await s.transport.setAlternate(true)
            await eventually("the program's screen") { s.model.alternateScreen }
            await settle(s, 500)
            XCTAssertNil(surface(s), "nothing to scroll: the program has the whole screen")
            await s.transport.setAlternate(false)
            await eventually("the shell again") { !s.model.alternateScreen }
            await settle(s, 600)
            let view = try XCTUnwrap(surface(s), "normal scrolling is back")
            XCTAssertLessThanOrEqual(distanceFromBottom(view), lineHeight, "at the bottom")
            XCTAssertTrue(s.model.scrollFollow.following)
        }
    }

    func testAFullScreenProgramWithMoreRowsThanFitStaysInsideThePane() async throws {
        // 80 rows from the desktop, more than any iPhone has room for: what happens while the keyboard comes up and the desktop has
        // not followed the smaller pane yet. The screen stays the height it was given, so the header stays below the status bar and
        // the last rows above the bottom, and the pane does not measure itself 80 rows tall and ask the desktop for that.
        try await withScreen(ScriptedScrollback(history: 100, rows: 80), alternate: false) { s in
            await s.transport.setAlternate(true)
            await eventually("the program's screen") { s.model.alternateScreen }
            await settle(s, 500)
            let area = try XCTUnwrap(s.model.terminalArea)
            let bounds = s.host.view.bounds, safe = s.host.view.safeAreaInsets
            XCTAssertLessThan(Double(area.height), Double(bounds.height - safe.top - safe.bottom), "the pane is inside the safe area")
            XCTAssertLessThan(Double(area.height), 80 * lineHeight, "not as tall as the desktop's screen")
            let rows = try XCTUnwrap(s.model.terminalViewport?.rows)
            XCTAssertLessThan(rows, 80, "the desktop is asked for the rows that fit")
        }
    }

    // MARK: size changes

    func testAChangeOfTextSizeKeepsTheLineAtTheTopOfTheView() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            await drag(view, to: view.geometry.maxOffset - 2000.5, in: s)
            let top = view.geometry.topRow(offset: offset(view))
            s.model.setTerminalFontSize(16)
            await settle(s, 500)
            XCTAssertNotEqual(view.geometry.lineHeight, lineHeight)
            let after = view.geometry.topRow(offset: offset(view))
            XCTAssertEqual(after.row, top.row, "the same line is at the top")
            XCTAssertFalse(s.model.scrollFollow.following)
            s.model.setTerminalFontSize(TerminalFontSize.standard)
            await settle(s, 300)
        }
    }

    func testRenumberedLinesKeepTheReaderTheSameDistanceFromTheBottom() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let view = try XCTUnwrap(surface(s))
            await drag(view, to: view.geometry.maxOffset - 900, in: s)
            let away = distanceFromBottom(view)
            let epoch = s.model.terminal.epoch
            // The pane was re-wrapped by a resize: nothing matches, everything is numbered again.
            var drawn = ScriptedScrollback(history: 3000)
            drawn.longLine = nil
            await s.transport.setScrollback(drawn, reportsHistorySize: true)
            await s.transport.setOutput("rewrapped")
            await s.transport.write(0)
            _ = epoch
            await settle(s, 300)
            XCTAssertEqual(distanceFromBottom(view), away, accuracy: lineHeight + 0.5)
        }
    }

    // MARK: copy

    func testALongPressCopiesALineWithoutTheTextPresentationSelector() async throws {
        XCTAssertEqual(TerminalSurfaceView.copyable("done \u{23FA}\u{FE0E} ok"), "done \u{23FA} ok")
        try await withScreen(ScriptedScrollback(history: 500)) { s in
            let view = try XCTUnwrap(surface(s))
            let value = try XCTUnwrap(view.accessibilityValue)
            XCTAssertTrue(value.contains("L\(s.model.terminal.screenTop)"), "VoiceOver reads the lines in view")
            XCTAssertEqual(view.accessibilityLabel, "Terminal output")
            XCTAssertTrue(view.scrollView.gestureRecognizers?.contains { $0 is UILongPressGestureRecognizer } == true)
        }
    }

    // MARK: performance

    func testScrollingThroughFiftyThousandLinesStaysCheap() async throws {
        try await withScreen(ScriptedScrollback(history: 80_000)) { s in
            var pages = 0
            while !s.model.terminal.limitReached, pages < 400 {
                s.model.loadOlderHistory()
                while s.model.historyTask != nil { try? await Task.sleep(for: .milliseconds(2)) }
                pages += 1
            }
            await settle(s, 600)
            XCTAssertEqual(s.model.terminal.heldHistory, HistoryLimits.heldLines)
            let view = try XCTUnwrap(surface(s))
            XCTAssertGreaterThan(view.geometry.contentHeight - view.geometry.minOffset, Double(50_000) * lineHeight)
            // Jumps to anywhere: only the rows in view are laid out and painted.
            let start = Date()
            let steps = 60
            for step in 0..<steps {
                view.scrollView.contentOffset = CGPoint(x: 0, y: view.geometry.minOffset + (view.geometry.maxOffset - view.geometry.minOffset) * Double(step) / Double(steps))
                view.layoutIfNeeded()
                s.host.view.layoutIfNeeded()
            }
            let perJump = Date().timeIntervalSince(start) * 1000 / Double(steps)
            print("SCROLLPERF \(perJump) ms per jump across 50k lines (rows painted: \(view.shownRows.count))")
            XCTAssertLessThan(perJump, 25, "a jump to anywhere in 50,000 lines paints only the rows on screen")
            XCTAssertLessThan(view.shownRows.count, 120)
            // A live update with 50,000 lines held touches only what changed.
            await drag(view, to: view.geometry.maxOffset, in: s)
            let update = Date()
            await s.transport.write(1)
            await eventually("in") { s.model.terminal.screenTop == 80_001 }
            print("SCROLLPERF live update with 50k held \(Date().timeIntervalSince(update) * 1000) ms (includes the wait for the answer)")
        }
    }
}
