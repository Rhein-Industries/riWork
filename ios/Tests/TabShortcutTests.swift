import XCTest
@testable import RiWorkCore

final class TabShortcutTests: XCTestCase {
    private func event(_ text: String, phase: KeyEventRecord.Phase = .down, modifiers: ChordModifiers = []) -> KeyEventRecord {
        KeyEventRecord(phase: phase, keyCode: text == "\t" ? HIDKey.tab : text == "esc" ? HIDKey.escape : HIDKey.code(forCharacter: text.lowercased()) ?? 0x38,
                       modifiers: modifiers, characters: text, charactersIgnoringModifiers: text)
    }

    func testTabIsAConsumedPrefixAndOnlyTabQSendsALiteralTab() {
        var state = TabShortcutState()
        XCTAssertFalse(state.handle(event("q")).consumed, "ordinary typing is untouched")
        XCTAssertTrue(state.handle(event("\t")).consumed)
        XCTAssertTrue(state.armed)
        XCTAssertTrue(state.handle(event("\t", phase: .up)).consumed)
        XCTAssertTrue(state.armed, "a tap leaves the leader waiting for its second key")
        XCTAssertEqual(state.handle(event("q")).action, .literalTab)
        XCTAssertFalse(state.armed)
        XCTAssertFalse(state.handle(event("x")).consumed)
    }

    func testEveryDocumentedShortcutUsesAnUnmodifiedSecondKey() {
        let map: [(String, TabShortcutAction)] = [("e",.escape),("w",.up),("a",.left),("s",.down),("d",.right),
                                                 ("k",.keyMenu),("n",.newTerminal),("p",.projects),("m",.modelPicker)]
        for (text, action) in map + (1...9).map({ (String($0),.selectTab($0)) }) {
            var state = TabShortcutState()
            _ = state.handle(event("\t")); _ = state.handle(event("\t",phase:.up))
            XCTAssertEqual(state.handle(event(text)).action,action,text)
            XCTAssertFalse(state.guideVisible)
        }
    }

    func testOneSecondOfHoldingShowsTheGuideAndReleaseKeepsItOpen() {
        var state = TabShortcutState()
        _ = state.handle(event("\t"))
        XCTAssertFalse(state.guideVisible)
        state.heldLongEnough()
        XCTAssertTrue(state.guideVisible)
        _ = state.handle(event("\t",phase:.up))
        XCTAssertTrue(state.guideVisible)
        XCTAssertEqual(state.handle(event("e")).action,.escape)
        XCTAssertFalse(state.guideVisible)
    }

    func testATabKeyCommandArmsButIsNeverHeld() {
        var state = TabShortcutState()
        XCTAssertTrue(state.handle(event("\t", phase: .command)).consumed)
        XCTAssertTrue(state.armed)
        XCTAssertFalse(state.tabIsHeld, "no release follows a key command")
        state.heldLongEnough()
        XCTAssertFalse(state.guideVisible)
    }

    func testReleasingTabBeforeTheHoldThresholdCannotShowTheGuideLater() {
        var state = TabShortcutState()
        _ = state.handle(event("\t")); _ = state.handle(event("\t",phase:.up))
        state.heldLongEnough()
        XCTAssertFalse(state.guideVisible)
        XCTAssertTrue(state.armed)
    }

    func testUnknownPrefixStaysInTheGuideAndNeverTypesABareLetter() {
        var state = TabShortcutState()
        _ = state.handle(event("\t")); _ = state.handle(event("\t",phase:.up))
        let unknown = state.handle(event("x"))
        XCTAssertTrue(unknown.consumed); XCTAssertNil(unknown.action)
        XCTAssertTrue(state.guideVisible); XCTAssertTrue(state.unknownShortcut)
        XCTAssertEqual(state.handle(event("q")).action,.literalTab)
    }

    func testPhysicalEscapeCancelsTheLeaderWithoutSendingEscapeToTheShell() {
        var state = TabShortcutState()
        _ = state.handle(event("\t")); state.heldLongEnough()
        let escaped = state.handle(event("esc"))
        XCTAssertTrue(escaped.consumed); XCTAssertNil(escaped.action)
        XCTAssertFalse(state.armed); XCTAssertFalse(state.guideVisible)
        XCTAssertFalse(state.handle(event("esc")).consumed, "ordinary Esc is the screen's again")
    }

    func testHelpKeysShowTheMapAndModifiedTabKeepsItsConventionalMeaning() {
        for help in ["h","?"] {
            var state = TabShortcutState()
            XCTAssertFalse(state.handle(event("\t",modifiers:.shift)).consumed)
            _ = state.handle(event("\t")); _ = state.handle(event("\t",phase:.up))
            XCTAssertNil(state.handle(event(help)).action)
            XCTAssertTrue(state.guideVisible)
        }
    }

    func testCommandECancelsTheLeaderWithoutSendingEscapeAndTabEStillSendsEscape() {
        var state = TabShortcutState()
        _ = state.handle(event("\t")); _ = state.handle(event("\t",phase:.up))
        let cancelled = state.handle(event("e",modifiers:.command))
        XCTAssertTrue(cancelled.consumed); XCTAssertNil(cancelled.action)
        XCTAssertFalse(state.armed)
        XCTAssertFalse(state.handle(event("e",modifiers:.command)).consumed,"the normal alias is restored outside a prefix")
        _ = state.handle(event("\t")); _ = state.handle(event("\t",phase:.up))
        XCTAssertEqual(state.handle(event("e")).action,.escape)
    }

    func testCancellationDropsTheLeaderAndCommandModifiersCannotRunAnAppAction() {
        var state = TabShortcutState()
        _ = state.handle(event("\t")); _ = state.handle(event("\t",phase:.up))
        let command = state.handle(event("n",modifiers:.command))
        XCTAssertTrue(command.consumed); XCTAssertNil(command.action)
        XCTAssertTrue(state.guideVisible)
        _ = state.handle(event("n",phase:.cancelled))
        XCTAssertFalse(state.armed); XCTAssertFalse(state.guideVisible)
    }
}
