import XCTest
@testable import RiWorkCore

final class TabScreenChromeTests: XCTestCase {
    func testEveryTabKindGetsTheSameRow() {
        let chromes = TabScreenContent.allCases.map { TabScreenChrome.decide(content: $0, focusMode: false) }
        for chrome in chromes {
            XCTAssertEqual(chrome.header, .navigationRow)
            XCTAssertEqual(chrome.tabLines, 1)
            XCTAssertFalse(chrome.statusBarHidden)
        }
        for a in chromes { for b in chromes { XCTAssertFalse(a.movesTop(comparedTo: b), "\(a) vs \(b)") } }
        XCTAssertEqual(TabScreenChrome.rowHeight, 44)
    }

    func testSwitchingBetweenShellAndChatNeverMovesTheTop() {
        // Focus mode is a setting that stays on while a chat is shown: the chat still has its row, and the shell has none again after.
        for focus in [false, true] {
            let shell = TabScreenChrome.decide(content: .terminal, focusMode: focus)
            let chat = TabScreenChrome.decide(content: .chat, focusMode: focus)
            XCTAssertEqual(chat.header, .navigationRow)
            XCTAssertEqual(shell.movesTop(comparedTo: chat), focus, "only focus mode drops the row")
        }
    }

    func testOnlyATerminalHasTerminalActionsAndFocusMode() {
        XCTAssertTrue(TabScreenChrome.decide(content: .terminal, focusMode: false).terminalActions)
        for content in [TabScreenContent.chat, .unavailable, .none] {
            let chrome = TabScreenChrome.decide(content: content, focusMode: true)
            XCTAssertFalse(chrome.terminalActions)
            XCTAssertEqual(chrome.header, .navigationRow, "focus mode is the terminal's alone")
            XCTAssertFalse(chrome.statusBarHidden)
        }
        let focused = TabScreenChrome.decide(content: .terminal, focusMode: true)
        XCTAssertEqual(focused.header, .hidden)
        XCTAssertTrue(focused.statusBarHidden)
    }
}
