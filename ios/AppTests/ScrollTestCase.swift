import XCTest
import SwiftUI
import RiWorkCore
@testable import RiWorkRemote

/// Shared set-up for the scrolling tests: a connected model following a scripted desktop, and the checks on what it holds.
@MainActor class ScrollTestCase: XCTestCase {
    let project = "11111111-1111-4111-8111-111111111111"
    let shell = "44444444-4444-4444-8444-444444444444"
    let other = "55555555-5555-4555-8555-555555555555"

    func makeStore() throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    func scratchDefaults() -> UserDefaults {
        let name = "com.riwork.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }
    func makeModel(_ transport: FixtureTransport, _ keychain: KeychainStore, defaults: UserDefaults? = nil) -> RemoteModel {
        RemoteModel(client: transport, keychain: keychain, pollInterval: .milliseconds(40), keyFlushInterval: .milliseconds(5), previewDelay: .milliseconds(80),
                    reconnectBackoff: .milliseconds(10), defaults: defaults ?? scratchDefaults(), cellMetrics: { TerminalLayout.approximateCell(fontSize: $0) },
                    keepAwake: { _ in }, liveWaitMilliseconds: LiveSync.waitMilliseconds)
    }
    func session(_ id: String) throws -> RemoteSession {
        try JSONDecoder().decode(RemoteSession.self, from: Data("""
        {"id":"\(id)","project_id":"\(project)","kind":"project","cwd":"/fixture","alive":true,"created_at_unix":2}
        """.utf8))
    }
    func eventually(_ what: String, timeout: Double = 4, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(5)) }
        let met = await condition()
        XCTAssertTrue(met, what)
    }
    /// A connected model with the terminal on screen, following a scripted desktop with a long-poll-capable screen.
    func rig(_ scrollback: ScriptedScrollback, reportsHistorySize: Bool = true, mode: FixtureTransport.HistoryMode = .ok,
                     alternate: Bool? = nil, defaults: UserDefaults? = nil) async throws -> (RemoteModel, FixtureTransport, KeychainStore) {
        let keychain = try makeStore()
        let transport = FixtureTransport()
        await transport.setHashMode(true, cap: .milliseconds(120))
        await transport.setScrollback(scrollback, reportsHistorySize: reportsHistorySize)
        await transport.setHistoryMode(mode)
        if alternate != nil { await transport.setAlternate(alternate) }
        let model = makeModel(transport, keychain, defaults: defaults)
        model.setTerminalVisible(true)
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        await eventually("the first screen is in") { model.outputSessionID == self.shell && (!model.terminal.isEmpty || model.alternateScreen) }
        try? await Task.sleep(for: .milliseconds(200))
        return (model, transport, keychain)
    }
    /// The lines held are consecutive lines of the desktop's output, each under the index it has always had.
    func assertConsistent(_ model: RemoteModel, file: StaticString = #filePath, line: UInt = #line) {
        let buffer = model.terminal
        guard let first = buffer[buffer.start], let serial = Int(first.text.dropFirst()) else { return XCTFail("no first line", file: file, line: line) }
        let offset = serial - buffer.start
        for index in buffer.indices where buffer[index]?.text != "L\(index + offset)" {
            return XCTFail("index \(index) holds \(String(describing: buffer[index]?.text)), expected L\(index + offset)", file: file, line: line)
        }
    }
}
