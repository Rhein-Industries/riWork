import XCTest
@testable import RiWorkCore

final class HostDashboardTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)
    private let project = "11111111-1111-4111-8111-111111111111"
    private let chat = "22222222-2222-4222-8222-222222222222"

    private func snapshot(projects: Int = 2, chatsKnown: Bool = true, extra: String = "") throws -> DashboardSnapshot {
        let wire = """
        {"v":1,"chats_known":\(chatsKnown),"totals":{"projects":\(projects),"live_shells":1,"live_chats":3,"agents":{"working":1,"waiting":2},"tasks":{"todo":2,"in_progress":1}},
         "projects":[{"id":"\(project)","name":"Same project"}],
         "working":[{"session":{"target":{"type":"chat","id":"\(chat)","project_id":"\(project)"},"agent":"Codex","is_chat":true},"activity":"working","since_unix":1799999900}]
         \(extra)}
        """
        return try DashboardSnapshot(wire: JSONDecoder().decode(JSONValue.self, from: Data(wire.utf8)))
    }

    func testPartialSumsKeepUnknownCoverageAndStaleReads() throws {
        let first = HostDashboardEntry(id: "first", name: "Laptop", snapshot: try snapshot(), readAt: now, state: .ready)
        let second = HostDashboardEntry(id: "second", name: "Studio", snapshot: try snapshot(projects: 4, chatsKnown: false),
                                        readAt: now.addingTimeInterval(-100), state: .offline, failure: "Offline")
        let third = HostDashboardEntry(id: "third", name: "Offline", state: .offline)
        let aggregate = HostDashboardAggregate(hosts: [first, second, third], now: now)
        XCTAssertEqual(aggregate.projects.value, 6)
        XCTAssertEqual(aggregate.projects.knownHosts, 2)
        XCTAssertEqual(aggregate.projects.unknownHosts, 1)
        XCTAssertFalse(aggregate.projects.isComplete)
        XCTAssertEqual(aggregate.liveChats.value, 3, "unknown chat figures do not become zero")
        XCTAssertEqual(aggregate.liveChats.unknownHosts, 2)
        XCTAssertEqual(aggregate.openTasks.value, 6)
        XCTAssertNil(aggregate.attention.value, "no host sent this count")
        XCTAssertEqual(aggregate.sampledHostCount, 2)
        XCTAssertEqual(aggregate.freshHostCount, 1)
        XCTAssertEqual(aggregate.staleHostCount, 1)
        XCTAssertEqual(aggregate.unavailableHostCount, 1)
        XCTAssertEqual(aggregate.workingRows.map(\.stale), [false, true])
        XCTAssertEqual(second.freshness(now: now).text, "Offline · showing the read from \(DashboardText.duration(100)) ago")
        let invite = HostDashboardEntry(id: "invite", name: "New Mac", state: .offline, failure: "Open this desktop once to finish pairing.")
        XCTAssertEqual(invite.freshness(now: now).text, "Open this desktop once to finish pairing.", "the reason, not just unavailable")
        XCTAssertEqual(third.freshness(now: now).text, "Host unavailable")
    }

    func testRowsWithTheSameProjectAndChatIDsStayNamespacedByHost() throws {
        let read = try snapshot()
        let aggregate = HostDashboardAggregate(hosts: [
            HostDashboardEntry(id: "route-a", name: "Laptop", snapshot: read, readAt: now, state: .ready),
            HostDashboardEntry(id: "route-b", name: "Studio", snapshot: read, readAt: now, state: .ready),
        ], now: now)
        XCTAssertEqual(aggregate.projectRows.count, 2)
        XCTAssertEqual(Set(aggregate.projectRows.map(\.id)).count, 2)
        XCTAssertEqual(aggregate.projectRows.map(\.hostID), ["route-a", "route-b"])
        XCTAssertEqual(aggregate.workingRows.map(\.value.session.target), [.chat(id: chat, projectID: project, orchestrator: nil), .chat(id: chat, projectID: project, orchestrator: nil)])
        XCTAssertEqual(Set(aggregate.workingRows.map(\.id)).count, 2)
        XCTAssertEqual(aggregate.projects.value, 4, "two projects on each host, even when their local ids collide")
        XCTAssertTrue(aggregate.projects.isComplete)
    }

    func testAttentionAcrossHostsKeepsWaitingFirstAndNewestFirst() throws {
        let failure = try snapshot(extra: """
        ,"attention":[{"kind":"chat_failed","target":{"type":"chat","id":"\(chat)"},"at":1799999999}]
        """)
        let waiting = try snapshot(extra: """
        ,"attention":[{"kind":"waiting","target":{"type":"chat","id":"\(chat)"},"at":1799999000}]
        """)
        let newer = try snapshot(extra: """
        ,"attention":[{"kind":"waiting","target":{"type":"chat","id":"\(chat)"},"at":1799999900}]
        """)
        let aggregate = HostDashboardAggregate(hosts: [
            HostDashboardEntry(id: "failed", name: "Failed", snapshot: failure, readAt: now, state: .ready),
            HostDashboardEntry(id: "waiting", name: "Waiting", snapshot: waiting, readAt: now, state: .ready),
            HostDashboardEntry(id: "newer", name: "Newer", snapshot: newer, readAt: now, state: .ready),
        ], now: now)
        XCTAssertEqual(aggregate.attentionRows.map(\.hostID), ["newer", "waiting", "failed"])
        XCTAssertEqual(Set(aggregate.attentionRows.map(\.id)).count, 3)
    }

    func testEmptyAndNeverReadHostsHaveUnknownCounts() {
        let empty = HostDashboardAggregate(hosts: [], now: now)
        XCTAssertNil(empty.projects.value)
        XCTAssertFalse(empty.projects.isComplete)
        XCTAssertEqual(empty.unavailableHostCount, 0)
        let unread = HostDashboardAggregate(hosts: [HostDashboardEntry(id: "a", name: "A")], now: now)
        XCTAssertNil(unread.projects.value)
        XCTAssertEqual(unread.projects.unknownHosts, 1)
        XCTAssertEqual(unread.freshHostCount, 0)
        XCTAssertEqual(unread.unavailableHostCount, 1)
    }

    func testAHostBeingReadAgainAfterAFailureIsNotCurrent() throws {
        let read = try snapshot()
        XCTAssertFalse(HostDashboardEntry(id: "a", name: "A", snapshot: read, readAt: now, state: .ready).freshness(now: now).stale)
        // An offline host's next attempt says it is being read; its retained figures stay saved until the host answers.
        let retry = HostDashboardEntry(id: "b", name: "B", snapshot: read, readAt: now.addingTimeInterval(-5), state: .refreshing)
        XCTAssertTrue(retry.freshness(now: now).stale)
        XCTAssertEqual(HostDashboardAggregate(hosts: [retry], now: now).freshHostCount, 0)
    }

    func testDecisionsAcrossHostsAreNewestFirstSoOneHostCannotCrowdOutAnother() throws {
        func decisions(_ times: [Int], prefix: String) throws -> DashboardSnapshot {
            let rows = times.enumerated().map { index, time in
                let chat = String(format: "cccccccc-%04d-4000-8000-000000000001", index + (prefix == "a" ? 0 : 100))
                return """
                {"chat_id":"\(chat)","request_id":"\(prefix)-\(index)","kind":"approval","observed_at":\(time),
                 "session":{"target":{"type":"chat","id":"\(chat)"},"agent":"Claude","is_chat":true},
                 "source":{"chat_id":"\(chat)","event_seq":1,"event_id":"chat:\(chat):seq:1"},
                 "approval":{"request_id":"\(prefix)-\(index)","kind":"command","title":"Run","choices":["accept"]}}
                """
            }
            return try snapshot(extra: ",\"decisions\":[\(rows.joined(separator: ","))]")
        }
        let busy = HostDashboardEntry(id: "busy", name: "Busy", snapshot: try decisions([1_799_999_000, 1_799_999_001, 1_799_999_002], prefix: "a"), readAt: now, state: .ready)
        let quiet = HostDashboardEntry(id: "quiet", name: "Quiet", snapshot: try decisions([1_799_999_500], prefix: "b"), readAt: now, state: .ready)
        let rows = HostDashboardAggregate(hosts: [busy, quiet], now: now).decisionRows
        XCTAssertEqual(rows.map(\.value.requestID), ["b-0", "a-2", "a-1", "a-0"])
        XCTAssertEqual(rows.prefix(2).map(\.hostID), ["quiet", "busy"])
    }

    func testSuspendingAndAgingAHostMarksItsRetainedRowsStale() throws {
        var host = HostDashboardEntry(id: "a", name: "A", snapshot: try snapshot(), readAt: now, state: .ready)
        XCTAssertFalse(host.freshness(now: now).stale)
        XCTAssertTrue(host.freshness(now: now.addingTimeInterval(31)).stale)
        host.state = .paused
        XCTAssertTrue(host.freshness(now: now).stale)
        XCTAssertEqual(HostDashboardAggregate(hosts: [host], now: now).projectRows.first?.stale, true)
    }
}
