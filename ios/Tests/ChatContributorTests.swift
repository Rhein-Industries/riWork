import XCTest
@testable import RiWorkCore

final class ChatContributorTests: XCTestCase {
    private let id = "44444444-4444-4444-8444-444444444444"
    private func info(_ mode: ChatContributorMode = .off, provider: ChatProvider = .muse) -> ChatInfo {
        ChatInfo(id: id, provider: provider, cwd: "/fixture", title: "Muse", createdAtUnix: 1,
                 providerThreadID: "same-session", contributorMode: mode, contributorOptOut: mode == .off, state: .idle)
    }

    func testTheControlIsMuseOnlyEvenWhenAnotherProviderKeepsAMuseMode() throws {
        for provider in [ChatProvider.codex, .claude] {
            for mode in [ChatContributorMode.off, .on] { XCTAssertNil(ChatContributorChoice(info: info(mode, provider: provider))) }
        }
        XCTAssertEqual(try XCTUnwrap(ChatContributorChoice(info: info())).title, "Contributor off")
        XCTAssertEqual(try XCTUnwrap(ChatContributorChoice(info: info(.on))).title, "Contributor on")
    }

    func testBothDirectionsSendOnlyTheSameStandaloneConfigureAsDesktop() throws {
        for (from, to) in [(ChatContributorMode.off, ChatContributorMode.on), (.on, .off)] {
            let choice = try XCTUnwrap(ChatContributorChoice(info: info(from)))
            let request = try ChatCommandRequest(chatID: id, command: choice.command)
            XCTAssertEqual(choice.nextMode, to)
            XCTAssertEqual(request.params, ["chat_id": .string(id), "command": .object([
                "command": .string("configure"), "contributor_mode": .string(to.rawValue)
            ])])
            XCTAssertEqual(try ChatCommandRequest(params: request.params), request)
            XCTAssertNoThrow(try RequestValidation.validate(method: "chat.command", params: request.params, id: id))
        }
    }

    func testAnOlderOrUnreadableModeIsNeverPresentedAsOffAndTheFirstTapAsksOff() throws {
        for field in ["", ",\"contributor_mode\":\"future\"", ",\"contributor_mode\":true", ",\"contributor_mode\":null"] {
            let raw = "{\"id\":\"\(id)\",\"provider\":\"muse\",\"cwd\":\"/fixture\",\"title\":\"Muse\",\"created_at_unix\":1\(field)}"
            let decoded = try JSONDecoder().decode(ChatInfo.self, from: Data(raw.utf8))
            let choice = try XCTUnwrap(ChatContributorChoice(info: decoded))
            XCTAssertNil(choice.mode)
            XCTAssertEqual(choice.title, "Contributor unknown")
            XCTAssertEqual(choice.command, .configure(contributorMode: .off))
            XCTAssertTrue(choice.detail.contains("update RiWork"))
        }
    }

    func testChoosingDoesNotChangeStateAndFreshHostReportsSurviveOlderEventPages() throws {
        let off = info(), on = info(.on)
        var state = ChatContributorState()
        state.observe(off, through: 10)
        XCTAssertEqual(state.choice(fallback: off)?.command, .configure(contributorMode: .on))
        XCTAssertEqual(state.choice(fallback: off)?.mode, .off, "the request is never a state report")
        // The read after Configure reports the restarted process; an earlier event page arrives later.
        state.observe(on, through: 20)
        state.observe(off, through: 15)
        XCTAssertEqual(state.choice(fallback: off)?.mode, .on)
        XCTAssertEqual(state.info?.providerThreadID, "same-session")
        // A failed later resume reports the old mode again; a same-revision read is authoritative too.
        state.observe(off, through: 25)
        state.observe(on, through: 24)
        XCTAssertEqual(state.choice(fallback: on)?.mode, .off)
        state.observe(on, through: 25)
        XCTAssertEqual(state.choice(fallback: off)?.mode, .on)
    }

    func testAListedReportIsNotUndoneByAnOlderPageThatWasOnItsWay() {
        let off = info(), on = info(.on)
        var state = ChatContributorState()
        state.observe(off, through: 10)
        // A page is asked for at 10; an older host then lists the restarted chat, and the page arrives with the old mode.
        let inFlight = state.listing
        XCTAssertNil(inFlight)
        state.observeListed(on)
        state.observe(off, through: 11)
        state.caughtUp(after: inFlight)
        XCTAssertEqual(state.choice(fallback: off)?.mode, .on, "a page asked for before the list does not undo it")
        // A read asked for after the list but cut short has not seen everything the list had yet.
        let next = state.listing
        state.observe(off, through: 12)
        XCTAssertEqual(state.choice(fallback: off)?.mode, .on)
        // Once it reaches the end of the log, the events are what is shown again, and they move on as before.
        state.observe(on, through: 13)
        state.caughtUp(after: next)
        XCTAssertNil(state.listed)
        XCTAssertEqual(state.choice(fallback: off)?.mode, .on)
        state.observe(off, through: 14)
        XCTAssertEqual(state.choice(fallback: on)?.mode, .off)
        // A second listing while a read asked for after the first is out: that read does not end the second.
        state.observeListed(off)
        let first = state.listing
        state.observeListed(on)
        state.caughtUp(after: first)
        XCTAssertEqual(state.choice(fallback: off)?.mode, .on)
    }

    func testRelaunchAndHostRestartUseFreshInfoAndAProviderSwitchHidesTheControl() {
        for mode in [ChatContributorMode.off, .on] {
            let reported = info(mode)
            var relaunched = ChatContributorState()
            XCTAssertEqual(relaunched.choice(fallback: reported)?.mode, mode)
            let snapshot = ChatSnapshotReply(chatID: id, cursor: "fresh", next: 7, before: 0, more: false, items: [], controls: [.info(reported)])
            var feed = ChatFeed()
            feed.install(snapshot)
            relaunched.observe(feed.transcript.info!, through: snapshot.next)
            XCTAssertEqual(relaunched.choice(fallback: info())?.mode, mode)
            // A reset starts a new observation era, so a smaller sequence can be reported.
            relaunched = ChatContributorState()
            relaunched.observe(info(), through: 1)
            XCTAssertEqual(relaunched.choice(fallback: reported)?.mode, .off)
            relaunched.observe(info(.on, provider: .claude), through: 2)
            XCTAssertNil(relaunched.choice(fallback: reported))
        }
    }

    func testBusyAndFailedResumeMessagesKeepTheHostsExplanation() {
        for text in ["Wait for the turn to finish, or interrupt it, before changing contributor mode.",
                     "Contributor mode was not changed: the restart did not complete. This chat keeps contributor mode off."] {
            let error = ChatControlError.from(RemoteError.rpc(code: "cli_error", message: text), operation: .command)
            XCTAssertEqual(error.message, text)
        }
    }

    func testCreatingMuseFromThePhoneStillAsksForOff() throws {
        let request = try ChatCreateRequest(provider: .muse, target: .project("11111111-1111-4111-8111-111111111111"))
        XCTAssertEqual(request.contributorMode, .off)
        XCTAssertEqual(request.params["contributor_opt_out"], .bool(true))
        XCTAssertNil(request.params["contributor_mode"])
    }
}
