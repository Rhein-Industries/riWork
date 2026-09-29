import Foundation
import RiWorkCore

@main struct Smoke {
    static func main() async {
        do {
            let args = CommandLine.arguments
            guard args.count >= 2 else { throw RemoteError.remote("Usage: riwork-ios-smoke PAIRING_FILE [--project UUID] [--shell UUID] [--send LINE] [--columns 43 --rows 17] [--local] [--idle-seconds N]") }
            func value(_ flag: String) -> String? { guard let index = args.firstIndex(of: flag), index + 1 < args.count else { return nil }; return args[index + 1] }
            let local = args.contains("--local")
            let pairing = try Pairing.parse(String(contentsOfFile: args[1], encoding: .utf8), allowLocalDevelopment: local)
            let client = RelayClient()
            try await client.connect(pairing: pairing, allowLocalDevelopment: local)
            let projects = try await client.request(method: "projects.list")
            print("Authenticated; projects: \(projects["projects"].array.count)")
            if let project = value("--project") {
                for method in ["worktrees.list", "tasks.list", "shells.list"] {
                    let result = try await client.request(method: method, params: ["project_id": .string(project)])
                    print("\(method): \(result[method.components(separatedBy: ".")[0]].array.count)")
                }
            }
            let orchestrators = try await client.request(method: "orchestrators.list")
            print("orchestrators: \(orchestrators["orchestrators"].array.count)")
            if let shell = value("--shell") {
                let viewport: TerminalViewport?
                if let columnsText = value("--columns"), let rowsText = value("--rows"),
                   let columns = Int(columnsText), let rows = Int(rowsText),
                   (20...300).contains(columns), (8...160).contains(rows) {
                    viewport = TerminalViewport(columns: columns, rows: rows)
                } else if args.contains("--columns") || args.contains("--rows") {
                    throw RemoteError.remote("Specify both columns (20…300) and rows (8…160).")
                } else { viewport = nil }
                func resize() async throws {
                    guard let viewport else { return }
                    let result = try await client.request(method: "shell.resize", params: ["shell_id": .string(shell), "columns": .number(Double(viewport.columns)), "rows": .number(Double(viewport.rows))])
                    guard result["shell_id"].string == shell, result["columns"] == .number(Double(viewport.columns)), result["rows"] == .number(Double(viewport.rows)) else { throw RemoteError.protocolViolation("Resize acknowledgement mismatch.") }
                    print("Resized existing session to \(viewport.columns)x\(viewport.rows)")
                }
                func clear() async throws {
                    guard viewport != nil else { return }
                    let result = try await client.request(method: "shell.resize.clear", params: ["shell_id": .string(shell)])
                    guard result["shell_id"].string == shell, result["status"].string == "cleared" else { throw RemoteError.protocolViolation("Release acknowledgement mismatch.") }
                    print("Released desktop terminal dimensions")
                }
                try await resize()
                let params: [String: JSONValue] = ["shell_id": .string(shell), "lines": .number(100)]
                let before = try await client.request(method: "shell.output", params: params)
                guard before["shell_id"].string == shell, let output = before["output"].string else { throw RemoteError.protocolViolation("Output ID mismatch.") }
                print("Existing session \(shell):\n\(TerminalText.readable(output))")
                // Eight real overlapping actor requests exercise sendTail / wire counter ordering.
                // The relay's per-socket queue is 16; only one bounded batch is outstanding.
                let concurrentReads = try await withThrowingTaskGroup(of: Int.self) { group in
                    for index in 0..<8 {
                        group.addTask {
                            let result = try await client.request(method: "shell.output", params: params)
                            guard result["shell_id"].string == shell,
                                  let text = result["output"].string, !text.isEmpty else {
                                throw RemoteError.protocolViolation("Concurrent output identity or content mismatch.")
                            }
                            return index
                        }
                    }
                    var completed: Set<Int> = []
                    for try await index in group { completed.insert(index) }
                    return completed
                }
                guard concurrentReads == Set(0..<8), await client.isConnected() else {
                    throw RemoteError.protocolViolation("Concurrent read batch did not complete on the authenticated connection.")
                }
                print("Concurrent wire reads: 8/8 validated on one authenticated connection")
                var inputID: String?
                if let line = value("--send") {
                    try InputValidation.validate(line)
                    let id = UUID().uuidString.lowercased(); inputID = id
                    let ack = try await client.request(method: "shell.input", params: ["shell_id": .string(shell), "line": .string(line)], id: id)
                    guard ack["shell_id"].string == shell, ack["status"].string == "sent" else { throw RemoteError.protocolViolation("Input acknowledgement mismatch.") }
                    print("Acknowledged one input request \(id)")
                }
                try await clear()
                await client.disconnect()
                try await client.connect(pairing: pairing, allowLocalDevelopment: local)
                try await resize()
                if let id = inputID, let line = value("--send") {
                    // Explicit isolated-fixture smoke verifies desktop dedup with the SAME UUID.
                    let cached = try await client.request(method: "shell.input", params: ["shell_id": .string(shell), "line": .string(line)], id: id)
                    guard cached["status"].string == "sent" else { throw RemoteError.protocolViolation("Dedup acknowledgement missing.") }
                    print("Same UUID returned cached acknowledgement after fresh reconnect")
                }
                let after = try await client.request(method: "shell.output", params: params)
                guard after["shell_id"].string == shell, let output = after["output"].string else { throw RemoteError.protocolViolation("Reconnected to different session.") }
                print("Reconnected to same existing session:\n\(TerminalText.readable(output))")
                // Deliberately close without explicit clear to verify peer-loss restoration.
            }
            if let idleText = value("--idle-seconds"), let idle = Double(idleText), idle > 0 {
                let deadline = Date().addingTimeInterval(idle)
                while Date() < deadline {
                    if !(await client.isConnected()) { throw RemoteError.remote("Keepalive dropped the idle connection.") }
                    try await Task.sleep(for: .seconds(1))
                }
                if !(await client.isConnected()) { throw RemoteError.remote("Keepalive dropped the idle connection.") }
                print("Idle keepalive held for \(idleText)s")
            }
            await client.disconnect()
            print("PASS")
        } catch {
            // Never print credentials, URLs with tokens, pairing JSON, or raw frames.
            fputs("Smoke failed: \(error.localizedDescription)\n", stderr)
            exit(1)
        }
    }
}
