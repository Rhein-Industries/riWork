import XCTest
@testable import RiWorkCore

final class ProjectSortTests: XCTestCase {
    private func project(_ id: String, _ name: String, created: UInt64 = 1, edited: UInt64? = nil) throws -> RemoteProject {
        let editedField = edited.map { ",\"last_edited_unix\":\($0)" } ?? ""
        return try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(id)\",\"name\":\"\(name)\",\"root\":\"/\(id)\",\"created_at\":\(created)\(editedField)}".utf8))
    }
    private func order(_ projects: [RemoteProject], _ sort: ProjectSort, touched: [String: UInt64] = [:]) -> [String] {
        ProjectSorting.sorted(projects, by: sort, touched: touched).map(\.id)
    }

    // MARK: Recent

    func testRecentIsNewestEditFirst() throws {
        let list = [try project("a", "Alpha", created: 50, edited: 100), try project("b", "Beta", created: 10, edited: 300), try project("c", "Gamma", created: 90, edited: 200)]
        XCTAssertEqual(order(list, .recent), ["b", "c", "a"])
        // The order of the input does not matter.
        XCTAssertEqual(order(list.reversed(), .recent), ["b", "c", "a"])
    }
    func testRecentPutsProjectsWithoutAnEditTimeAfterTheDatedOnesByWhenTheyWereAdded() throws {
        let list = [try project("a", "Alpha", created: 999),                       // newest added, but no edit time
                    try project("b", "Beta", created: 1, edited: 5),                // oldest everything, but dated
                    try project("c", "Gamma", created: 500),
                    try project("d", "Delta", created: 700, edited: 6)]
        XCTAssertEqual(order(list, .recent), ["d", "b", "a", "c"])
    }
    func testAnEditTimeOfZeroIsNoEditTime() throws {
        let zero = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"z\",\"name\":\"Zed\",\"root\":\"/z\",\"created_at\":900,\"last_edited_unix\":0}".utf8))
        let list = [zero, try project("b", "Beta", created: 1, edited: 5)]
        XCTAssertEqual(order(list, .recent), ["b", "z"])
    }
    func testAnOlderDesktopOrdersRecentByDateAdded() throws {
        let list = [try project("a", "Alpha", created: 10), try project("b", "Beta", created: 30), try project("c", "Gamma", created: 20)]
        XCTAssertEqual(order(list, .recent), ["b", "c", "a"])
        XCTAssertEqual(order(list, .recent), order(list, .dateAdded))
    }
    func testProjectsWithNoFigureAtAllComeLast() throws {
        let list = [try project("a", "Alpha", created: 0), try project("b", "Beta", created: 0, edited: 4), try project("c", "Gamma", created: 3)]
        XCTAssertEqual(order(list, .recent), ["b", "c", "a"])
        XCTAssertEqual(order(list, .dateAdded), ["c", "a", "b"], "an edit time does not count for Date added")
    }
    func testRecentTiesFallBackToNameThenId() throws {
        let list = [try project("3", "beta", edited: 10), try project("2", "Alpha", edited: 10), try project("1", "alpha", edited: 10), try project("4", "Beta", edited: 10),
                    try project("6", "same", edited: 10), try project("5", "same", edited: 10)]
        // Same time: names without regard to case, then as written ("A" before "a"), then id.
        XCTAssertEqual(order(list, .recent), ["2", "1", "4", "3", "5", "6"])
        // The same among the projects without an edit time and with the same date added.
        let undated = list.map { try! project($0.id, $0.name, created: 77) }
        XCTAssertEqual(order(undated, .recent), ["2", "1", "4", "3", "5", "6"])
    }
    func testAProjectJustMadeHereIsAtTheTopUntilTheDesktopHasAFigureForIt() throws {
        let list = [try project("a", "Alpha", created: 10, edited: 5_000), try project("b", "Beta", created: 20, edited: 4_000), try project("new", "Fresh", created: 6_000)]
        XCTAssertEqual(order(list, .recent), ["a", "b", "new"], "undated, and so after the dated ones, as far as the desktop says")
        XCTAssertEqual(order(list, .recent, touched: ["new": 6_000]), ["new", "a", "b"])
        // Edited since by someone else: the later of the two times counts.
        let edited = [try project("a", "Alpha", created: 10, edited: 9_000), try project("new", "Fresh", created: 6_000, edited: 4_000)]
        XCTAssertEqual(order(edited, .recent, touched: ["new": 6_000]), ["a", "new"])
        let editedLater = [try project("a", "Alpha", created: 10, edited: 5_000), try project("new", "Fresh", created: 6_000, edited: 8_000)]
        XCTAssertEqual(order(editedLater, .recent, touched: ["new": 6_000]), ["new", "a"])
        // Other orders ignore it.
        XCTAssertEqual(order(list, .name, touched: ["new": 6_000]), ["a", "b", "new"])
        XCTAssertEqual(order(list, .dateAdded, touched: ["new": 6_000]), ["new", "b", "a"])
    }

    // MARK: Name and Date added

    func testNameIsAToZIgnoringCaseWhateverTheTimes() throws {
        let list = [try project("1", "delta", edited: 900), try project("2", "Alpha", edited: 1), try project("3", "charlie"), try project("4", "Bravo", edited: 500)]
        XCTAssertEqual(order(list, .name), ["2", "4", "3", "1"])
    }
    func testNameTiesAreCaseThenId() throws {
        let list = [try project("3", "app"), try project("2", "App"), try project("1", "app")]
        XCTAssertEqual(order(list, .name), ["2", "1", "3"])
    }
    func testDateAddedIsNewestFirstIgnoringEdits() throws {
        let list = [try project("a", "Alpha", created: 10, edited: 900), try project("b", "Beta", created: 30), try project("c", "Gamma", created: 20, edited: 1)]
        XCTAssertEqual(order(list, .dateAdded), ["b", "c", "a"])
    }
    func testDateAddedPutsProjectsWithoutOneLastAndBreaksTiesByName() throws {
        let list = [try project("a", "Zulu", created: 0), try project("b", "Yankee", created: 20), try project("c", "Xray", created: 20), try project("d", "Whiskey", created: 0)]
        XCTAssertEqual(order(list, .dateAdded), ["c", "b", "d", "a"])
    }

    // MARK: Search and stability

    func testFilteringKeepsTheOrder() throws {
        let list = [try project("a", "web-app", created: 1, edited: 10), try project("b", "api", created: 2, edited: 40), try project("c", "web-site", created: 3, edited: 30),
                    try project("d", "docs", created: 4), try project("e", "webhook", created: 5, edited: 20)]
        for sort in ProjectSort.allCases {
            let matches: (RemoteProject) -> Bool = { $0.name.localizedCaseInsensitiveContains("WEB") }
            let visible = ProjectSorting.visible(list, matching: "WEB", by: sort).map(\.id)
            XCTAssertEqual(visible, ProjectSorting.sorted(list, by: sort).filter(matches).map(\.id), "\(sort)")
            XCTAssertEqual(Set(visible), ["a", "c", "e"], "\(sort)")
            // No search shows everything, in the same order.
            XCTAssertEqual(ProjectSorting.visible(list, matching: "", by: sort).map(\.id), ProjectSorting.sorted(list, by: sort).map(\.id))
            XCTAssertTrue(ProjectSorting.visible(list, matching: "zzz", by: sort).isEmpty)
        }
        XCTAssertEqual(ProjectSorting.visible(list, matching: "web", by: .recent).map(\.id), ["c", "e", "a"])
        XCTAssertEqual(ProjectSorting.visible(list, matching: "web", by: .name).map(\.id), ["a", "c", "e"], "web-app, web-site, webhook: the dash sorts before h")
    }
    func testTheOrderDoesNotDependOnTheInputOrder() throws {
        let names = ["x", "y", "X"]
        var list: [RemoteProject] = []
        for n in 0..<12 {
            let edited: UInt64? = n % 2 == 0 ? UInt64(n % 5) : nil
            list.append(try project("p\(n)", names[n % 3], created: UInt64(n % 4), edited: edited))
        }
        for sort in ProjectSort.allCases {
            let expected = order(list, sort)
            XCTAssertEqual(order(list.reversed(), sort), expected, "\(sort)")
            XCTAssertEqual(order(list.shuffled(), sort), expected, "\(sort)")
        }
    }
    func testNothingIsLostOrInvented() throws {
        let list = [try project("a", "A", edited: 3), try project("b", "B"), try project("c", "C", created: 0)]
        for sort in ProjectSort.allCases { XCTAssertEqual(Set(order(list, sort)), ["a", "b", "c"]) }
        XCTAssertTrue(ProjectSorting.sorted([], by: .recent).isEmpty)
    }

    // MARK: The choice

    func testTheTitlesMirrorTheDesktopAndRecentIsTheDefault() {
        XCTAssertEqual(ProjectSort.standard, .recent)
        XCTAssertEqual(ProjectSort.allCases.map(\.title), ["Recent", "Name", "Date added"])
        XCTAssertEqual(ProjectSort.allCases.map(\.detail), ["Newest first", "A to Z", "Newest first"])
    }
    func testNextStepsThroughAllThreeAndWraps() {
        XCTAssertEqual(ProjectSort.recent.next, .name)
        XCTAssertEqual(ProjectSort.name.next, .dateAdded)
        XCTAssertEqual(ProjectSort.dateAdded.next, .recent)
    }
    func testTheChoiceIsRemembered() throws {
        let name = "com.riwork.tests.projectsort.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        defer { UserDefaults().removePersistentDomain(forName: name) }
        XCTAssertEqual(ProjectSort.stored(in: defaults), .recent, "nothing stored: Recent")
        for sort in ProjectSort.allCases {
            sort.remember(in: defaults)
            XCTAssertEqual(ProjectSort.stored(in: defaults), sort)
        }
        // A fresh read of the same store (a relaunch) finds the last choice.
        ProjectSort.name.remember(in: defaults)
        XCTAssertEqual(ProjectSort.stored(in: try XCTUnwrap(UserDefaults(suiteName: name))), .name)
    }
    func testAnUnknownStoredChoiceIsRecent() throws {
        let name = "com.riwork.tests.projectsort.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        defer { UserDefaults().removePersistentDomain(forName: name) }
        for bad in ["", "liveSessions", "NAME", "last_edited"] {
            defaults.set(bad, forKey: ProjectSort.defaultsKey)
            XCTAssertEqual(ProjectSort.stored(in: defaults), .recent, bad)
        }
        defaults.set(3, forKey: ProjectSort.defaultsKey)
        XCTAssertEqual(ProjectSort.stored(in: defaults), .recent)
    }
}
