import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// The real terminal screen on an iPhone, hosted in a window, against the scripted desktop. A user's finger is emulated the way
/// UIKit reports one to SwiftUI: the scroll view's delegate is told a drag began, the offset moves, and the drag ends.
@MainActor final class ScrollingUITests: ScrollTestCase {
    struct Screen {
        let model: RemoteModel, transport: FixtureTransport, keychain: KeychainStore, window: UIWindow, host: UIHostingController<AnyView>
    }
    private func screen(_ scrollback: ScriptedScrollback, alternate: Bool? = nil) async throws -> Screen {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene to host the screen in") }
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom == .pad, "the iPhone terminal")
        let (model, transport, keychain) = try await rig(scrollback, alternate: alternate)
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data(#"{"id":"\#(project)","name":"Fixture","root":"/fixture","created_at":1}"#.utf8))
        let host = UIHostingController(rootView: AnyView(TerminalTabsView(model: model, project: projectValue, onBack: {}).desktopThemed(model.theme.style)))
        let window = UIWindow(windowScene: scene)
        window.rootViewController = host
        window.makeKeyAndVisible()
        let made = Screen(model: model, transport: transport, keychain: keychain, window: window, host: host)
        await eventually("the terminal is on screen") { self.terminalScroll(made) != nil }
        await settle(made, 400)
        return made
    }
    private func finish(_ screen: Screen) async {
        await screen.model.disconnect()
        screen.window.isHidden = true
        try? screen.keychain.delete()
    }
    /// Runs `body` on a fresh screen and always tears it down.
    private func withScreen(_ scrollback: ScriptedScrollback, alternate: Bool? = nil, _ body: @MainActor (Screen) async throws -> Void) async throws {
        let s = try await screen(scrollback, alternate: alternate)
        do { try await body(s) } catch { await finish(s); throw error }
        await finish(s)
    }
    private func settle(_ screen: Screen, _ milliseconds: Int = 300) async {
        for _ in 0..<max(1, milliseconds / 30) { screen.host.view.layoutIfNeeded(); try? await Task.sleep(for: .milliseconds(30)) }
    }
    private func allScrollViews(_ view: UIView) -> [UIScrollView] {
        var found: [UIScrollView] = []
        if let scroll = view as? UIScrollView { found.append(scroll) }
        for sub in view.subviews { found += allScrollViews(sub) }
        return found
    }
    /// The terminal's scroll view: the one with the most to scroll (the tab strip is a short horizontal one).
    private func terminalScroll(_ screen: Screen) -> UIScrollView? {
        allScrollViews(screen.host.view).filter { $0.contentSize.height > $0.bounds.height + 1 && $0.bounds.height > 100 }.max { $0.contentSize.height < $1.contentSize.height }
    }
    private func maxOffset(_ scroll: UIScrollView) -> Double { Double(scroll.contentSize.height + scroll.adjustedContentInset.bottom - scroll.bounds.height) }
    private func distanceFromBottom(_ scroll: UIScrollView) -> Double { maxOffset(scroll) - Double(scroll.contentOffset.y) }
    private var lineHeight: Double { TerminalFont.cell(size: TerminalFontSize.standard).height }

    /// A finger: begins a drag, moves the view to `offset`, and (unless held) lifts.
    private func drag(_ scroll: UIScrollView, to offset: Double, in screen: Screen, hold: Bool = false) async {
        scroll.delegate?.scrollViewWillBeginDragging?(scroll)
        scroll.contentOffset = CGPoint(x: 0, y: offset)
        await settle(screen, 90)
        if !hold {
            scroll.delegate?.scrollViewDidEndDragging?(scroll, willDecelerate: false)
            await settle(screen, 300)
        }
    }
    private func lift(_ scroll: UIScrollView, in screen: Screen) async {
        scroll.delegate?.scrollViewDidEndDragging?(scroll, willDecelerate: false)
        await settle(screen, 400)
    }

    // MARK: vertical only, opening at the bottom

    func testTheTerminalScrollsOnlyVerticallyClipsLongLinesAndOpensAtTheBottom() async throws {
        var scrollback = ScriptedScrollback(history: 3000)
        scrollback.longLine = 2990
        try await withScreen(scrollback) { s in
            let scroll = try XCTUnwrap(terminalScroll(s))
            XCTAssertLessThanOrEqual(scroll.contentSize.width, scroll.bounds.width + 0.5, "a 400-character line does not widen the content")
            XCTAssertFalse(scroll.alwaysBounceHorizontal)
            XCTAssertGreaterThan(scroll.contentSize.height, Double(512) * lineHeight, "all the lines loaded are in the content, under lazy rows")
            XCTAssertLessThanOrEqual(distanceFromBottom(scroll), lineHeight, "opens pinned to the bottom")
            XCTAssertTrue(s.model.scrollFollow.following)
        }
    }

    func testNewOutputIsFollowedWhileAtTheBottom() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let scroll = try XCTUnwrap(terminalScroll(s))
            let height = scroll.contentSize.height
            await s.transport.write(6)
            await eventually("the lines are in") { s.model.terminal.screenTop == 3006 }
            await settle(s, 400)
            XCTAssertEqual(scroll.contentSize.height, height + 6 * lineHeight, accuracy: 1)
            XCTAssertLessThanOrEqual(distanceFromBottom(scroll), lineHeight, "and the view went down with them")
            XCTAssertTrue(s.model.scrollFollow.following)
            XCTAssertNil(s.model.scrollFollow.pill)
        }
    }

    func testWithinOneLineOfTheBottomStillFollows() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let scroll = try XCTUnwrap(terminalScroll(s))
            await drag(scroll, to: maxOffset(scroll) - lineHeight * 0.6, in: s)
            XCTAssertTrue(s.model.scrollFollow.following, "half a line up is still the bottom")
            await s.transport.write(2)
            await eventually("in") { s.model.terminal.screenTop == 3002 }
            await settle(s, 400)
            XCTAssertLessThanOrEqual(distanceFromBottom(scroll), lineHeight)
        }
    }

    // MARK: sticky bottom

    func testScrolledUpNothingMovesWhileOutputArrivesAndThePillCountsAndBringsTheReaderBack() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let scroll = try XCTUnwrap(terminalScroll(s))
            await drag(scroll, to: maxOffset(scroll) - 400, in: s)
            XCTAssertFalse(s.model.scrollFollow.following)
            XCTAssertNil(s.model.scrollFollow.pill, "nothing new yet, and not far away")
            let offset = scroll.contentOffset.y
            let height = scroll.contentSize.height
            await s.transport.write(7)
            await eventually("new output is in") { s.model.terminal.screenTop == 3007 }
            await settle(s, 500)
            XCTAssertEqual(scroll.contentOffset.y, offset, accuracy: 0.5, "what is being read did not move")
            XCTAssertEqual(scroll.contentSize.height, height + 7 * lineHeight, accuracy: 1, "the lines were added below")
            XCTAssertEqual(s.model.scrollFollow.newLines, 7)
            XCTAssertEqual(s.model.scrollFollow.pill?.label, "↓ Live · 7 new")
            // the pill
            s.model.jumpToLatest()
            await settle(s, 500)
            XCTAssertLessThanOrEqual(distanceFromBottom(scroll), lineHeight, "tapping the pill went to the bottom")
            XCTAssertTrue(s.model.scrollFollow.following)
            XCTAssertEqual(s.model.scrollFollow.newLines, 0)
            XCTAssertNil(s.model.scrollFollow.pill)
            // and follows again
            await s.transport.write(2)
            await eventually("in") { s.model.terminal.screenTop == 3009 }
            await settle(s, 400)
            XCTAssertLessThanOrEqual(distanceFromBottom(scroll), lineHeight)
        }
    }

    func testTypingBringsTheReaderBackToThePrompt() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let scroll = try XCTUnwrap(terminalScroll(s))
            await drag(scroll, to: maxOffset(scroll) - 2500, in: s)
            XCTAssertFalse(s.model.scrollFollow.following)
            s.model.type(KeyMapper.items(for: "l"))
            XCTAssertTrue(s.model.scrollFollow.following)
            await settle(s, 500)
            XCTAssertLessThanOrEqual(distanceFromBottom(scroll), lineHeight)
        }
    }

    func testAReaderFarFromTheBottomGetsThePillEvenWithoutNewOutput() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let scroll = try XCTUnwrap(terminalScroll(s))
            await drag(scroll, to: maxOffset(scroll) - 4000, in: s)
            XCTAssertEqual(s.model.scrollFollow.pill?.label, "↓ Live")
        }
    }

    func testASwitchToAnotherShellStartsAtItsBottom() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            await s.transport.setSessions([try session(shell), try session(other)])
            await s.model.refresh()
            let scroll = try XCTUnwrap(terminalScroll(s))
            await drag(scroll, to: maxOffset(scroll) - 3000, in: s)
            XCTAssertFalse(s.model.scrollFollow.following)
            await s.model.chooseSession(try session(other))
            await eventually("the other shell is shown") { s.model.outputSessionID == self.other && !s.model.terminal.isEmpty }
            await settle(s, 600)
            let switched = try XCTUnwrap(terminalScroll(s))
            XCTAssertLessThanOrEqual(distanceFromBottom(switched), lineHeight, "pinned to the bottom of the other shell")
            XCTAssertTrue(s.model.scrollFollow.following)
        }
    }

    func testTheBottomStaysClearOfThePendingInputChipWhileFollowing() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let scroll = try XCTUnwrap(terminalScroll(s))
            // Keys the desktop refuses are held and shown in a chip that floats over the bottom of the pane.
            await s.transport.setKeysMode(.inputUnavailable)
            s.model.type(KeyMapper.items(for: "x"))
            await eventually("the chip is up") { s.model.keyPreview != nil }
            await settle(s, 500)
            XCTAssertGreaterThan(scroll.adjustedContentInset.bottom, 10, "room is kept under the last line for the chip")
            XCTAssertLessThanOrEqual(distanceFromBottom(scroll), lineHeight)
            await s.transport.write(4)
            await eventually("in") { s.model.terminal.screenTop == 3004 }
            await settle(s, 500)
            XCTAssertLessThanOrEqual(distanceFromBottom(scroll), lineHeight, "the last line is above the chip, not under it")
            XCTAssertTrue(s.model.scrollFollow.following, "and following was not lost on the way")
        }
    }

    // MARK: history paging

    func testAPageFetchedNearTheTopIsPrependedWithoutMovingTheReader() async throws {
        try await withScreen(ScriptedScrollback(history: 3000)) { s in
            let scroll = try XCTUnwrap(terminalScroll(s))
            // Far from the top: no request.
            await drag(scroll, to: maxOffset(scroll) - 2000, in: s)
            var requests = await s.transport.historyRequests().count
            XCTAssertEqual(requests, 0)
            // Within a screen of the top of what is loaded.
            let topInset = Double(scroll.adjustedContentInset.top)
            let lineHeight = self.lineHeight
            let topLine = { s.model.terminal.start + Int((Double(scroll.contentOffset.y) + topInset - 4 - lineHeight) / lineHeight) }
            let start = s.model.terminal.start
            let heightBefore = scroll.contentSize.height
            let target = -topInset + 12 * lineHeight
            scroll.delegate?.scrollViewWillBeginDragging?(scroll)
            scroll.contentOffset = CGPoint(x: 0, y: target)
            let line = topLine()
            await settle(s, 90)
            scroll.delegate?.scrollViewDidEndDragging?(scroll, willDecelerate: false)
            await eventually("the page is in") { s.model.terminal.heldHistory == 800 }
            await settle(s, 600)
            XCTAssertEqual(s.model.terminal.start, start - 300)
            XCTAssertEqual(scroll.contentSize.height, heightBefore + 300 * lineHeight, accuracy: 1)
            XCTAssertEqual(scroll.contentOffset.y, target + 300 * lineHeight, accuracy: lineHeight, "moved down by exactly the lines added above")
            XCTAssertEqual(topLine(), line, "the same line is at the top of the view")
            assertConsistent(s.model)
            requests = await s.transport.historyRequests().count
            XCTAssertEqual(requests, 1, "the reader is now 300 lines from the top: no further request")
            XCTAssertFalse(s.model.scrollFollow.following)
        }
    }

    func testPagingContinuesAsTheReaderKeepsGoingUpAndEndsAtTheBeginning() async throws {
        try await withScreen(ScriptedScrollback(history: 900)) { s in
            let scroll = try XCTUnwrap(terminalScroll(s))
            let topInset = Double(scroll.adjustedContentInset.top)
            await drag(scroll, to: -topInset + 5 * lineHeight, in: s)
            await eventually("the first page is in") { s.model.terminal.heldHistory == 800 }
            await settle(s, 500)
            XCTAssertFalse(s.model.terminal.atTop)
            // The reader goes on up to the top of what is loaded now.
            await drag(scroll, to: -topInset + 5 * lineHeight, in: s)
            await eventually("the last page is in") { s.model.terminal.atTop }
            await settle(s, 500)
            XCTAssertEqual(s.model.terminal.start, 0)
            XCTAssertEqual(s.model.terminal.heldHistory, 900)
            assertConsistent(s.model)
            let requests = await s.transport.historyRequests()
            XCTAssertEqual(requests.count, 2, "500 held, then 300, then the last 100: complete")
            await drag(scroll, to: -topInset, in: s)
            let after = await s.transport.historyRequests().count
            XCTAssertEqual(after, 2, "and nothing more to ask at the top")
        }
    }

    // MARK: alternate screen

    func testAFullScreenProgramReplacesTheScrollViewAndNormalScrollingComesBack() async throws {
        try await withScreen(ScriptedScrollback(history: 3000), alternate: false) { s in
            XCTAssertNotNil(terminalScroll(s))
            await s.transport.setAlternate(true)
            await eventually("the program's screen") { s.model.alternateScreen }
            await settle(s, 500)
            XCTAssertNil(terminalScroll(s), "nothing to scroll: the program has the whole screen")
            await s.transport.setAlternate(false)
            await eventually("the shell again") { !s.model.alternateScreen }
            await settle(s, 600)
            let scroll = try XCTUnwrap(terminalScroll(s), "normal scrolling is back")
            XCTAssertLessThanOrEqual(distanceFromBottom(scroll), lineHeight, "at the bottom")
            XCTAssertTrue(s.model.scrollFollow.following)
        }
    }

    // MARK: performance

    func testScrollingThroughTwentyThousandLinesStaysCheap() async throws {
        try await withScreen(ScriptedScrollback(history: 25_000)) { s in
            var pages = 0
            while !s.model.terminal.limitReached, pages < 100 {
                s.model.loadOlderHistory()
                while s.model.historyTask != nil { try? await Task.sleep(for: .milliseconds(5)) }
                pages += 1
            }
            await settle(s, 600)
            XCTAssertEqual(s.model.terminal.heldHistory, HistoryLimits.heldLines)
            let scroll = try XCTUnwrap(terminalScroll(s))
            XCTAssertGreaterThan(scroll.contentSize.height, Double(20_000) * lineHeight)
            let start = Date()
            let steps = 40
            for step in 0..<steps {
                scroll.contentOffset = CGPoint(x: 0, y: maxOffset(scroll) * Double(step) / Double(steps))
                s.host.view.layoutIfNeeded()
            }
            let perJump = Date().timeIntervalSince(start) * 1000 / Double(steps)
            print("SCROLLPERF \(perJump) ms per jump across 20k lines")
            XCTAssertLessThan(perJump, 150, "a jump to anywhere in 20,000 lines lays out only the rows on screen")
            // A live update with 20,000 lines held.
            let update = Date()
            await s.transport.write(1)
            await eventually("in") { s.model.terminal.screenTop == 25_001 }
            print("SCROLLPERF live update with 20k held \(Date().timeIntervalSince(update) * 1000) ms")
        }
    }
}
