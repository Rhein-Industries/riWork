import XCTest
@testable import RiWorkCore

final class GatewayTests: XCTestCase {
    private let route = "44444444-4444-4444-8444-444444444444"
    private let target = "55555555-5555-4555-8555-555555555555"
    private let session = "66666666-6666-4666-8666-666666666666"
    private let request = "77777777-7777-4777-8777-777777777777"
    private func pairing() -> Pairing {
        Pairing(v: 1, relay_url: "wss://example.invalid/v1/ws", desktop_id: "11111111-1111-4111-8111-111111111111",
                device_id: "22222222-2222-4222-8222-222222222222", route_id: "33333333-3333-4333-8333-333333333333", device_name: "Fixture",
                pairing_secret: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8", relay_token: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8")
    }
    private func wrap(_ method: String, _ params: [String: JSONValue]) -> [String: JSONValue] {
        ["session": .string(session), "request_id": .string(request), "method": .string(method), "params": .object(params)]
    }
    func testLocalRouteKeepsGatewayAuthButBindsExecutionIdentity() throws {
        let base = pairing(), routed = base.routed(to: .init(routeID: route, hostID: target, gatewayName: "Studio"))
        XCTAssertEqual(routed.direct, base)
        XCTAssertEqual(try routed.identityBytes(), try base.identityBytes())
        XCTAssertEqual(routed.executionHostID, target)
        XCTAssertNotEqual(routed.connectionID, base.connectionID)
        XCTAssertNotEqual(UploadConnectionIdentity(pairing: routed), UploadConnectionIdentity(pairing: base))
        let other = base.routed(to: .init(routeID: route, hostID: request, gatewayName: "Studio"))
        XCTAssertNotEqual(routed.connectionID, other.connectionID)
        XCTAssertNotEqual(UploadConnectionIdentity(pairing: routed), UploadConnectionIdentity(pairing: other))
        let encoded = try JSONEncoder().encode(routed)
        XCTAssertEqual(try JSONDecoder().decode(Pairing.self, from: encoded), routed)
        XCTAssertThrowsError(try Pairing.parse(String(decoding: encoded, as: UTF8.self)))
        XCTAssertEqual(try Pairing.parse(String(decoding: JSONEncoder().encode(base), as: UTF8.self)), base)
    }
    func testDirectRelayRejectsVirtualRouteBeforeOpeningSocket() async {
        let value = pairing().routed(to: .init(routeID: route, hostID: target, gatewayName: "Studio"))
        do { _ = try await RelayClient().connect(pairing: value); XCTFail("Opened a direct socket for a virtual route") }
        catch { XCTAssertTrue(error.localizedDescription.contains("routed transport")) }
    }
    func testWrappedValidationBudgetAndTimeoutKeepInnerSemantics() throws {
        let poll = wrap("chat.events", ["chat_id": .string(target), "since": .number(0), "wait_ms": .number(25000)])
        XCTAssertNoThrow(try RequestValidation.validate(method: "hosts.call", params: poll, id: request))
        XCTAssertEqual(RequestBudget.kind(method: "hosts.call", params: poll), .longPoll)
        XCTAssertEqual(RelayClient.timeout(for: "hosts.call", params: poll, default: .seconds(15)),
                       RelayClient.timeout(for: "chat.events", params: ["wait_ms": .number(25000)], default: .seconds(15)))
        let write = wrap("shell.input", ["shell_id": .string(target), "line": .string("hello")])
        XCTAssertTrue(GatewayRequests.isMutation(method: "hosts.call", params: write))
        XCTAssertFalse(GatewayRequests.isMutation(method: "hosts.call", params: poll))
        XCTAssertThrowsError(try RequestValidation.validate(method: "hosts.call", params: wrap("hosts.list", [:]), id: request))
        XCTAssertThrowsError(try RequestValidation.validate(method: "hosts.call", params: wrap("pty.open", [:]), id: request))
        XCTAssertThrowsError(try RequestValidation.validate(method: "hosts.call", params: wrap("shell.close", ["shell_id": .string("bad")]), id: request))
        var extra = write; extra["host_id"] = .string(target)
        XCTAssertThrowsError(try RequestValidation.validate(method: "hosts.call", params: extra, id: request))
    }
    func testInventoryAcceptsOnlyBoundedUniqueSSHDescriptors() throws {
        let row: JSONValue = .object(["route_id": .string(route), "host_id": .string(target), "label": .string("Studio"), "transport": .string("ssh")])
        XCTAssertEqual(try GatewayHost.list(.object(["v": .number(1), "hosts": .array([row])])).first?.hostID, target)
        XCTAssertThrowsError(try GatewayHost.list(.object(["v": .number(1), "hosts": .array([row, row])])))
        let unicode: JSONValue = .object(["route_id": .string(route), "host_id": .string(target), "label": .string(String(repeating: "💻", count: 80)), "transport": .string("ssh")])
        XCTAssertEqual(try GatewayHost.list(.object(["v": .number(1), "hosts": .array([unicode])])).first?.label.count, 80)
        XCTAssertThrowsError(try GatewayHost.list(.object(["v": .number(1), "hosts": .array([.object(["route_id": .string(route), "host_id": .string(target), "label": .string("Relay"), "transport": .string("relay")])])])))
        XCTAssertFalse(DesktopFeatures().hosts)
        XCTAssertTrue(DesktopFeatures(ready: .object(["features": .object(["hosts": .number(1)])])).hosts)
    }
}
