import Foundation

/// Local metadata. The pairing still authenticates the gateway; this pins the execution host behind it.
public struct GatewayRoute: Codable, Sendable, Equatable {
    public let routeID: String
    public let hostID: String
    public var gatewayName: String
    public init(routeID: String, hostID: String, gatewayName: String) {
        self.routeID = routeID; self.hostID = hostID; self.gatewayName = gatewayName
    }
}

public struct GatewayHost: Sendable, Equatable {
    public let routeID: String
    public let hostID: String
    public let label: String

    public static func list(_ value: JSONValue) throws -> [GatewayHost] {
        guard value["v"] == .number(1), case .array(let rows) = value["hosts"], rows.count <= 64 else {
            throw RemoteError.protocolViolation("Invalid gateway host inventory.")
        }
        var seen: Set<String> = []
        return try rows.map { row in
            guard row["transport"] == .string("ssh"), let route = row["route_id"].string, let host = row["host_id"].string,
                  GatewayRequests.isUUID(route), GatewayRequests.isUUID(host), seen.insert(route).inserted,
                  let label = row["label"].string, !label.isEmpty, label.utf8.count <= 1024,
                  !label.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
                throw RemoteError.protocolViolation("Invalid gateway host inventory.")
            }
            return GatewayHost(routeID: route, hostID: host, label: String(label.prefix(80)))
        }
    }
}

/// The outer gateway request retains the inner request's validation, budget, timeout and delivery semantics.
public enum GatewayRequests {
    public static func isUUID(_ value: String) -> Bool { UUID(uuidString: value)?.uuidString.lowercased() == value }
    public static func inner(method: String, params: [String: JSONValue]) -> (method: String, params: [String: JSONValue])? {
        guard method == "hosts.call", let nested = params["method"]?.string, !nested.hasPrefix("hosts."),
              case .object(let arguments)? = params["params"] else { return nil }
        return (nested, arguments)
    }
    public static func validate(method: String, params: [String: JSONValue]) throws -> Bool {
        guard method.hasPrefix("hosts.") else { return false }
        func exact(_ names: Set<String>) throws {
            guard Set(params.keys) == names else { throw RemoteError.protocolViolation("Invalid gateway request parameters.") }
        }
        func uuid(_ name: String) throws {
            guard let value = params[name]?.string, isUUID(value) else { throw RemoteError.protocolViolation("Invalid gateway identity.") }
        }
        switch method {
        case "hosts.list": try exact([])
        case "hosts.open": try exact(["route_id", "host_id"]); try uuid("route_id"); try uuid("host_id")
        case "hosts.close": try exact(["session"]); try uuid("session")
        case "hosts.call":
            try exact(["session", "request_id", "method", "params"])
            try uuid("session"); try uuid("request_id")
            guard let nested = inner(method: method, params: params), nested.method != "link.configure", !nested.method.hasPrefix("pty.") else {
                throw RemoteError.protocolViolation("Unsupported gateway operation.")
            }
            try RequestValidation.validate(method: nested.method, params: nested.params, id: params["request_id"]!.string!)
        default: throw RemoteError.protocolViolation("Unsupported gateway operation.")
        }
        return true
    }
    public static func isMutation(method: String, params: [String: JSONValue]) -> Bool {
        guard let nested = inner(method: method, params: params) else { return false }
        if nested.method == "native.invoke" {
            return NativeInvokeRequests.isWrite(module: nested.params["module"]?.string ?? "", method: nested.params["method"]?.string ?? "") == true
        }
        return ["tabs.open", "tabs.update", "shell.input", "shell.keys", "shell.resize", "shell.resize.clear", "shell.create", "shell.close", "project.create", "project.memo.set", "orchestrator.create", "chat.create", "chat.command", "chat.stop", "upload.begin", "upload.chunk", "upload.finish", "upload.cancel", "shell.paste"].contains(nested.method)
    }
}
