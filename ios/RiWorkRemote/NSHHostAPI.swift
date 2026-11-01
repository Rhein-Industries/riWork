import Foundation
import NativeShell
import Observation
import RiWorkCore
import UIKit

/// The host calls the shared TSX screens use on iPhone and iPad (`apps/riwork/client`, `apps/riwork-mobile/swift_transport.ts`;
/// the table is in docs/nsh-ios-migration.md). They run over the existing `RemoteModel` and `NSHMobileBridge`: no second transport,
/// socket or credential store. JS sees host ids and labels; pairing keys, tokens and URLs with credentials never cross.
///
/// - `relay.call` reaches only the interactive host's connection, only for the methods in `callable`, and through `RequestValidation`.
///   JS never long-polls (`wait_ms` above 0) or opens `pty.*`.
/// - Decisions go through the tested Swift flow (`NSHMobileBridge.respondToDecision`, `DashboardResponder`): the request is checked
///   again, fresh, against the host before one `chat.command` goes out; it is never repeated, and a response already sent (or whose
///   delivery is uncertain) from this phone is refused.
/// - Rejections read `"<code>: <message>"`, the codes of `PortError` (`offline`, `unsupported`, `not_found`, `conflict`, `limit`,
///   `uncertain`, `failed`).
@MainActor final class NSHHostAPI {
    static let calls = ["relay.hosts", "relay.select", "relay.disconnect", "relay.call", "relay.decision.review", "relay.decision.respond",
                        "platform.prefs.get", "platform.prefs.set", "nav.present"]
    /// What `relay.call` lets the shared screens ask: the reads of the host's dashboard, Projects and project pages, and the writes
    /// those pages make (tabs, terminals, projects). Each still passes `RequestValidation`; JS never long-polls or opens `pty.*`.
    static let callable: Set<String> = [
        "dashboard.get", "projects.list", "worktrees.list", "shells.list", "chats.list", "tabs.list",
        "tabs.open", "tabs.update", "shell.create", "shell.close", "project.create", "native.invoke",
    ]
    /// The writes among them: never repeated, and an answer lost after sending is `uncertain` (the page reads the list again).
    static let writes: Set<String> = ["tabs.open", "tabs.update", "shell.create", "shell.close", "project.create"]
    /// The `native.invoke` methods the shared screens read (`Module.method`), each a read of `NativeInvokeRequests.methods`.
    static let nativeReads: Set<String> = ["Store.snapshot"]
    /// Creations may take a minute and a half on the Mac (protocol "Terminal creation", "Project creation").
    static let createTimeout: Duration = .seconds(100)
    /// Reviews kept for a response: the newest of them, by token.
    static let reviewLimit = 32

    private unowned let bridge: NSHMobileBridge
    private var model: RemoteModel { bridge.model }
    private weak var app: NshAppView?
    private var observation = UUID()
    private var sentHosts: Data?
    private var sentLinks: [String: Data] = [:]
    private var sentTheme: Data?
    private var reviews: [(id: String, item: HostedDashboardDecision)] = []

    init(bridge: NSHMobileBridge) { self.bridge = bridge }

    func attach(to app: NshAppView) {
        if self.app !== app { detach() }
        self.app = app
        func on(_ name: String, _ handler: @escaping @MainActor (NSHHostAPI, [String: Any]) async throws -> Any?) {
            app.handle(name) { [weak self] (data: Any?) async throws -> Any? in
                guard let self else { throw HostCallError("offline", "The mobile view has closed.") }
                do { return try await handler(self, data as? [String: Any] ?? [:]) }
                catch { throw HostCallError.from(error) }
            }
        }
        on("relay.hosts") { api, _ in ["hosts": api.hostsPayload()] }
        // As `mobile.action` `connect` and `disconnect`: never under a native screen, which uses this connection and is not dismissed here.
        on("relay.select") { api, data in
            try api.requireNoNativeScreen()
            try await api.bridge.connect(hostID: try Self.string(data, "host"))
            return [String: Any]()
        }
        on("relay.disconnect") { api, data in
            try api.requireNoNativeScreen()
            if try Self.string(data, "host") == api.model.selectedDesktopID { await api.bridge.disconnectInteractive() }
            return [String: Any]()
        }
        on("relay.call") { api, data in try await api.call(data) }
        on("relay.decision.review") { api, data in try api.review(data) }
        on("relay.decision.respond") { api, data in try await api.respond(data) }
        on("platform.prefs.get") { api, data in ["value": try api.preference(Self.string(data, "name"))] }
        on("platform.prefs.set") { api, data in
            try api.setPreference(Self.string(data, "name"), data["value"])
            return [String: Any]()
        }
        on("nav.present") { api, data in
            try await api.present(data)
            return [String: Any]()
        }
        sentHosts = nil; sentLinks = [:]; sentTheme = nil
        publish()
    }

    func detach() {
        observation = UUID()
        for name in Self.calls { app?.removeHandler(name) }
        app = nil
        reviews.removeAll()
    }

    /// `host.params` for a bundle about to load: the theme the first frame is drawn in.
    func params() -> [String: Any] { ["theme": themePayload()] }

    /// Sends `relay.hosts`, `relay.link` and `platform.theme` for what changed since the last send. Called on every bridge snapshot,
    /// and on its own when what it reads changes.
    func publish() {
        guard let app else { return }
        let token = UUID(); observation = token
        let (hosts, theme) = withObservationTracking { (hostsPayload(), themePayload()) } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, self.observation == token else { return }
                self.publish()
            }
        }
        if let data = Self.canonical(hosts), data != sentHosts {
            sentHosts = data
            app.send("relay.hosts", ["hosts": hosts])
            var links: [String: Data] = [:]
            for host in hosts {
                guard let id = host["id"] as? String, let link = host["link"], let encoded = Self.canonical(link) else { continue }
                links[id] = encoded
                if sentLinks[id] != encoded { app.send("relay.link", ["host": id, "link": link]) }
            }
            sentLinks = links
        }
        if let data = Self.canonical(theme), data != sentTheme {
            sentTheme = data
            app.send("platform.theme", theme)
        }
    }

    // MARK: Hosts and links

    private func hostsPayload() -> [[String: Any]] {
        let interactive = bridge.interactiveHostID
        return model.desktops.map { desktop in
            var value: [String: Any] = ["id": desktop.id, "name": desktop.name, "selected": desktop.id == interactive,
                                        "link": link(desktop.id), "features": features(desktop.id)]
            // The relay's host name only: the pairing URL carries a token.
            value["address"] = URLComponents(string: desktop.pairing.relay_url)?.host
            return value
        }
    }

    /// The link as the pages see it. A connection is `connected` only once what the Mac offers is known (`capabilitiesKnown`: its
    /// `ready.features` and first lists): before that a page would read a Mac that shares tabs as one that does not, and offer a
    /// close that goes straight to `shell.close`.
    private func link(_ id: String) -> [String: Any] {
        guard id == model.selectedDesktopID else {
            if id == bridge.interactiveHostID { return ["state": "connecting", "label": ConnectionState.connecting.label] }
            return ["state": "offline", "label": "Not connected"]
        }
        let state: String = switch model.state {
        case .connected: model.capabilitiesKnown ? "connected" : "connecting"
        case .connecting: "connecting"
        case .suspended: "suspended"
        case .failed: "failed"
        case .disconnected: "offline"
        }
        var value: [String: Any] = ["state": state, "label": model.state.label]
        if model.state != .connected, let error = model.error { value["error"] = error }
        return value
    }

    /// What the interactive host's connection announced, by its `ready.features` names; none until the link is `connected`.
    private func features(_ id: String) -> [String] {
        guard id == model.selectedDesktopID, model.state == .connected, model.capabilitiesKnown else { return [] }
        let features = model.desktopFeatures
        let flags: [(String, Bool)] = [
            ("deflate", features.deflate), ("chat", features.chat), ("tabs", features.tabs), ("upload", features.upload != nil),
            ("orchestrator_create", features.orchestratorCreate), ("shell_create_as_settings", features.shellCreateAsSettings),
            ("chat_provider_switch", features.chatProviderSwitch), ("chat_models", features.chatModels),
            ("chat_suggestions", features.chatSuggestions), ("project_memo", features.projectMemo),
            // A Mac whose connector is newer than its RiWork announces it and then refuses it.
            ("dashboard", features.dashboard && model.dashboardSupport != .unsupported),
            ("native", !features.nativeMethods.isEmpty),
        ]
        // `native:Module.method` for each native read the shared screens may make on this Mac.
        let native = features.nativeMethods.intersection(Self.nativeReads).sorted().map { "native:\($0)" }
        return flags.filter(\.1).map(\.0) + native
    }

    // MARK: relay.call

    private func call(_ data: [String: Any]) async throws -> Any? {
        let hostID = try Self.string(data, "host"), method = try Self.string(data, "method")
        guard !method.hasPrefix("pty."), Self.callable.contains(method) else {
            throw HostCallError("unsupported", "The shared screens cannot ask for \(method).")
        }
        guard case .object(let params) = try Self.json(data["params"] ?? [String: Any]()) else {
            throw HostCallError("failed", "The request's params must be an object.")
        }
        if let wait = params["wait_ms"], wait != .number(0) { throw HostCallError("failed", "Long polls stay in Swift.") }
        if method == "native.invoke" {
            // Only the reads the shared screens make, and only those this Mac lists; `RequestValidation` checks the arguments.
            guard case .string(let module)? = params["module"], case .string(let name)? = params["method"],
                  Self.nativeReads.contains("\(module).\(name)"), NativeInvokeRequests.isWrite(module: module, method: name) == false else {
                throw HostCallError("unsupported", "The shared screens cannot ask for that.")
            }
            guard model.desktopFeatures.nativeMethods.contains("\(module).\(name)") else {
                throw HostCallError("unsupported", "The RiWork on this Mac does not offer this yet. Update RiWork on the Mac.")
            }
        }
        guard hostID == model.selectedDesktopID else { throw HostCallError("offline", "Open this desktop first.") }
        guard model.appActive else { throw HostCallError("offline", ConnectionState.suspended.label) }
        guard model.state == .connected else { throw HostCallError("offline", model.state.label) }
        guard model.capabilitiesKnown else { throw HostCallError("offline", ConnectionState.connecting.label) }
        if method == "shell.close" { try closable(params) }
        if method == "dashboard.get", !model.desktopFeatures.dashboard || model.dashboardSupport == .unsupported {
            throw HostCallError("unsupported", model.dashboardUnavailableReason ?? "Update RiWork on the Mac to see its dashboard here.")
        }
        let write = Self.writes.contains(method), creates = method == "shell.create" || method == "project.create"
        let ceiling = creates ? Self.createTimeout : .seconds(60)
        let timeout = (data["timeoutMs"] as? NSNumber).map { min(ceiling, Duration.milliseconds(max(1_000, $0.intValue))) } ?? (creates ? ceiling : .seconds(30))
        let relay = model, token = relay.generation
        do {
            let reply = try await Self.within(timeout) { try await relay.rpc(method, params) }
            if method == "dashboard.get" { relay.installDashboard(reply, token: token) }
            relay.installSharedRead(method, params: params, reply: reply, token: token)
            return Self.foundation(reply)
        } catch {
            if method == "dashboard.get" { await relay.dashboardReadFailed(error, token: token) }
            // A write that may have reached the Mac is never sent again: the page reads its list instead.
            if write, Self.answerLost(error) {
                throw HostCallError("uncertain", "The Mac's answer was lost; it may have done this. Read the list again before trying again.")
            }
            throw HostCallError.from(error, dashboard: method == "dashboard.get")
        }
    }

    /// `shell.close` from the pages, refused as `RemoteModel.closeTerminal` refuses it: only a project terminal of the project Swift
    /// follows (the page's), and never a shell this device has ever seen as a shared worker, which is only ever detached.
    private func closable(_ params: [String: JSONValue]) throws {
        guard case .string(let id)? = params["shell_id"] else { throw HostCallError("failed", "The request names no terminal.") }
        if model.everSharedWorkers.contains(id) || model.sharedEntry(ofSession: id)?.isWorker == true {
            throw HostCallError("failed", "A worker's shell is detached, not closed, from the phone.")
        }
        guard let session = model.shells.first(where: { $0.id == id }) ?? model.orchestrators.first(where: { $0.id == id }) else {
            throw HostCallError("failed", "Still reading this project's terminals. Try again in a moment.")
        }
        guard session.kind == "project" else { throw HostCallError("failed", "Orchestrators are closed on the Mac.") }
    }

    /// The request may have gone out and its answer did not come back: the link dropped, or the wait ran out.
    private static func answerLost(_ error: any Error) -> Bool {
        if error is CancellationError { return true }
        if let known = error as? HostCallError { return known.code == "failed" && known.message == HostCallError.noAnswer }
        guard let remote = error as? RemoteError else { return false }
        switch remote {
        case .disconnected, .relayClosed, .timeout, .uncertainDelivery: return true
        case .rpc(let code, _): return code == "outcome_unknown"
        default: return false
        }
    }

    /// Waits at most `limit` for `work`; past it the waiter is let go (`RelayClient` abandons it) and the connection stays.
    private static func within<T: Sendable>(_ limit: Duration, _ work: @escaping @Sendable () async throws -> T) async throws -> T {
        try await withThrowingTaskGroup(of: T?.self) { group in
            group.addTask { try await work() }
            group.addTask { try? await Task.sleep(for: limit); return nil }
            defer { group.cancelAll() }
            guard let first = try await group.next(), let value = first else {
                throw HostCallError("failed", HostCallError.noAnswer)
            }
            return value
        }
    }

    // MARK: Decisions

    /// A request of the interactive host's dashboard as the shared `DecisionReview` reads it. The token stands for the exact request
    /// this phone parsed (`DashboardDecision`); the response names only the token and the answer.
    private func review(_ data: [String: Any]) throws -> Any? {
        let hostID = try Self.string(data, "host"), chatID = try Self.string(data, "chatId"), requestID = try Self.string(data, "requestId")
        let kind = data["kind"] as? String
        let reviewID = UUID().uuidString.lowercased()
        func unavailable(_ message: String) -> [String: Any] {
            var value: [String: Any] = ["reviewId": reviewID, "chatId": chatID, "requestId": requestID, "title": "Request",
                                        "kind": kind == "question" ? "question" : "approval", "status": "unavailable"]
            value["error"] = message
            return value
        }
        guard hostID == model.selectedDesktopID, let desktop = model.desktops.first(where: { $0.id == hostID }) else {
            return unavailable(DashboardResponseError.hostUnavailable.localizedDescription)
        }
        guard let rows = model.dashboard?.decisions.rows else { return unavailable("Read the dashboard again, then review the request.") }
        guard let decision = rows.first(where: { $0.chatID == chatID && $0.requestID == requestID && (kind == nil || Self.kind($0) == kind) }) else {
            return unavailable("This request is no longer listed, or this phone cannot read all of it. Open the chat to answer it.")
        }
        let item = HostedDashboardDecision(hostID: hostID, hostName: desktop.name, decision: decision)
        reviews.removeAll { $0.item.id == item.id }
        reviews.append((reviewID, item))
        if reviews.count > Self.reviewLimit { reviews.removeFirst(reviews.count - Self.reviewLimit) }
        if bridge.isDecisionAnswered(item) {
            return Self.reviewPayload(reviewID, item.decision, status: "uncertain", error: DashboardResponseError.alreadySent.localizedDescription)
        }
        if case .approval(let approval) = decision.content, approval.choices.isEmpty {
            return Self.reviewPayload(reviewID, decision, status: "unavailable", error: "This host offers choices this app cannot read. Open the chat to answer.")
        }
        return Self.reviewPayload(reviewID, decision, status: "pending", error: nil)
    }

    /// One response, through the bridge's tested path. What it answers says what happened: `sent`; `pending` with why when nothing
    /// went out and it may be sent again; `unavailable` when this request can no longer be answered here; `uncertain` when it may
    /// have gone out (never sent again from this phone).
    private func respond(_ data: [String: Any]) async throws -> Any? {
        let reviewID = try Self.string(data, "reviewId")
        guard let item = reviews.first(where: { $0.id == reviewID })?.item,
              data["host"] as? String == nil || data["host"] as? String == item.hostID else {
            throw HostCallError("not_found", "This review has closed. Read the request again.")
        }
        let command: ChatCommand
        switch item.decision.content {
        case .approval(let approval):
            guard let word = data["decision"] as? String, let choice = ChatDecision(rawValue: word), approval.choices.contains(choice) else {
                return Self.reviewPayload(reviewID, item.decision, status: "pending", error: "Choose one of the answers this request offers.")
            }
            command = .approve(requestID: item.decision.requestID, decision: choice)
        case .question:
            guard let answers = data["answers"] as? [[String]] else {
                return Self.reviewPayload(reviewID, item.decision, status: "pending", error: "Answer every question before sending.")
            }
            command = .answer(requestID: item.decision.requestID, answers: answers)
        }
        do {
            // The shared review reads the dashboard and its lists again itself once it has the answer (`onResponded`).
            try await bridge.respondToDecision(item, command: command, refresh: false)
            return Self.reviewPayload(reviewID, item.decision, status: "sent", error: nil)
        } catch let failure as DashboardResponseError {
            // Every one of these sent nothing.
            let status: String = switch failure {
            case .alreadySent: "uncertain"
            case .staleRequest, .unverifiable: "unavailable"
            case .hostUnavailable, .changedHost, .notSent: "pending"
            }
            return Self.reviewPayload(reviewID, item.decision, status: status, error: failure.localizedDescription)
        } catch let failure as ChatControlError {
            // The desktop refused it (nothing happened) or the connection lost it (it may have happened).
            return Self.reviewPayload(reviewID, item.decision, status: failure.outcomeIsUncertain ? "uncertain" : "pending", error: failure.message)
        } catch {
            return Self.reviewPayload(reviewID, item.decision, status: "uncertain",
                                      error: "Delivery could not be confirmed: \(error.localizedDescription). Open the chat to see whether the provider received it.")
        }
    }

    private static func kind(_ decision: DashboardDecision) -> String {
        if case .question = decision.content { return "question" }
        return "approval"
    }

    /// `DashboardDecisionReview`, the shape the Mac's native review answers (`chat/view.rs` `approval_dto`, `question_dto`).
    private static func reviewPayload(_ reviewID: String, _ decision: DashboardDecision, status: String, error: String?) -> [String: Any] {
        var value: [String: Any] = ["reviewId": reviewID, "chatId": decision.chatID, "requestId": decision.requestID,
                                    "title": decision.title, "kind": kind(decision), "status": status]
        switch decision.content {
        case .approval(let approval):
            let heading = switch approval.kind {
            case .command: "Run a command?"
            case .fileChange: "Change files?"
            case .permissions: "Grant more access?"
            case .tool: "Use a tool?"
            }
            value["approval"] = ["requestId": approval.requestID, "kind": approval.kind.rawValue, "heading": heading,
                                 "title": approval.title, "detail": approval.detail,
                                 "choices": approval.choices.map { ["decision": $0.rawValue, "label": ChatApprovalBar.title($0)] }]
        case .question(let question):
            value["question"] = ["requestId": question.requestID, "prompts": question.questions.map { prompt -> [String: Any] in
                var entry: [String: Any] = ["question": prompt.question, "multiSelect": prompt.multiSelect,
                                            "options": prompt.options.map { ["label": $0.label, "description": $0.description] }]
                entry["header"] = prompt.header
                return entry
            }]
        }
        value["error"] = error
        return value
    }

    // MARK: Preferences and theme

    /// Preferences by name, kept where the Swift screens keep them. `projectTab.<project>`: the interactive host's last tab of a
    /// project (`SavedDesktop.projectTabKeys`, in the Keychain library), `shell:<id>` or `chat:<id>`.
    private func preference(_ name: String) throws -> Any {
        if let project = Self.projectTabName(name) { return model.desktop?.projectTabKeys?[project] ?? NSNull() }
        switch name {
        case "projectSort": return model.projectSort.rawValue
        case "newTerminal.kind": return model.lastTerminalKind.rawValue
        default: throw HostCallError("unsupported", "No preference is named \(name).")
        }
    }
    private func setPreference(_ name: String, _ value: Any?) throws {
        if let project = Self.projectTabName(name) {
            guard let key = value as? String, let separator = key.firstIndex(of: ":"), ["shell", "chat"].contains(String(key[..<separator])),
                  Self.isUUID(String(key[key.index(after: separator)...])) else { throw HostCallError("failed", "Unknown tab.") }
            guard model.desktop != nil, model.desktop?.projectTabKeys?[project] != key else { return }
            do { try model.updateDesktop { var keys = $0.projectTabKeys ?? [:]; keys[project] = key; $0.projectTabKeys = keys } }
            catch { throw HostCallError("failed", error.localizedDescription) }
            return
        }
        switch name {
        case "projectSort":
            guard let sort = (value as? String).flatMap(ProjectSort.init(rawValue:)) else { throw HostCallError("failed", "Unknown project order.") }
            model.setProjectSort(sort)
        case "newTerminal.kind":
            guard let kind = (value as? String).flatMap(NewTerminalKind.init(rawValue:)), NewTerminalKind.allCases.contains(kind) else {
                throw HostCallError("failed", "Unknown terminal kind.")
            }
            model.rememberTerminalKind(kind)
        default: throw HostCallError("unsupported", "No preference is named \(name).")
        }
    }
    /// The project of a `projectTab.<project>` preference name (a canonical UUID), else nil.
    private static func projectTabName(_ name: String) -> String? {
        guard name.hasPrefix("projectTab.") else { return nil }
        let project = String(name.dropFirst("projectTab.".count))
        return isUUID(project) ? project : nil
    }

    /// `{theme: InterfaceTheme, native, mic, dark}` (`apps/riwork/theme.ts`), from what `ThemeStore` resolved: the phone's palette
    /// choice, the desktop's synced palette on iPad, the contrast fixes, the skin and the desktop's mic switch.
    private func themePayload() -> [String: Any] {
        let style = model.theme.style
        let resolved = bridge.themeDTO()
        let colors = resolved["palette"] as? [String: String] ?? [:]
        func color(_ key: String) -> String { colors[key] ?? "#000000" }
        let palette: [String: String] = [
            "bg": color("bg"), "panel": color("panel"), "panelActive": color("panelActive"), "divider": color("divider"),
            "cyan": color("cyan"), "magenta": color("magenta"), "gold": color("gold"), "text": color("text"), "muted": color("muted"),
            "focus": color("cyan"), "working": color("working"), "danger": color("error"),
        ]
        return ["theme": ["palette": palette, "scale": style.scale, "family": style.native ? "system-ui" : "Menlo", "monoFamily": "Menlo"],
                "native": style.native, "mic": style.mic, "dark": resolved["dark"] as? Bool ?? false]
    }

    // MARK: nav.present

    /// A screen Swift still draws, over the shared pages. Of the interactive host: `chat` (`chatId`, and `projectId`, by default the
    /// project the pages show: the native chat, its project's lists read first), `global` (its global orchestrator, started first only with `start`, an explicit Start), `memo`
    /// (`projectId`), `newChat` (`projectId`: the native New terminal sheet at Chat, for chats and orchestrators). Any time:
    /// `settings`, `addHost`, `hostDetails` (`host`), `libraryRecovery`.
    private func present(_ data: [String: Any]) async throws {
        try requireNoNativeScreen()
        let name = try Self.string(data, "name"), hostID = data["host"] as? String
        func requireConnected() throws {
            guard let hostID, hostID == model.selectedDesktopID, model.state == .connected else {
                throw HostCallError("offline", "Connect to this desktop first.")
            }
        }
        func project() throws -> String {
            let id = try Self.string(data, "projectId")
            guard Self.isUUID(id) else { throw HostCallError("not_found", "This project is unavailable.") }
            return id
        }
        switch name {
        case "global":
            try requireConnected()
            try await bridge.openGlobal(start: data["start"] as? Bool == true)
        case "chat":
            try requireConnected()
            let chat = try Self.string(data, "chatId")
            guard Self.isUUID(chat) else { throw HostCallError("not_found", "This chat is unavailable.") }
            let named: String? = try data["projectId"] == nil ? model.projectID : project()
            guard let named else { throw HostCallError("not_found", "Open the chat's project first.") }
            try bridge.openChat(chat, project: named)
        case "memo":
            try requireConnected()
            guard model.desktopFeatures.projectMemo else { throw HostCallError("unsupported", "Update RiWork on the Mac to keep project memos here.") }
            try bridge.openMemo(project: try project())
        case "newChat":
            try requireConnected()
            try await bridge.openNewChat(project: try project())
        default: try bridge.present(named: name, hostID: hostID)
        }
    }

    private static func isUUID(_ value: String) -> Bool { value.utf8.count == 36 && UUID(uuidString: value)?.uuidString.lowercased() == value }

    private func requireNoNativeScreen() throws {
        guard !bridge.nativePresented else { throw HostCallError("conflict", "Close the current native screen first.") }
    }

    // MARK: JSON

    private static func string(_ data: [String: Any], _ key: String) throws -> String {
        guard let text = data[key] as? String, !text.isEmpty, text.utf8.count <= 2048 else { throw HostCallError("failed", "The call is missing its \(key).") }
        return text
    }

    /// A JS value (Foundation JSON types) as `JSONValue`.
    static func json(_ value: Any?) throws -> JSONValue {
        guard let value, !(value is NSNull) else { return .null }
        switch value {
        case let number as NSNumber:
            if CFGetTypeID(number) == CFBooleanGetTypeID() { return .bool(number.boolValue) }
            guard number.doubleValue.isFinite else { throw HostCallError("failed", "A number in the request is not finite.") }
            return .number(number.doubleValue)
        case let text as String: return .string(text)
        case let list as [Any]: return .array(try list.map { try json($0) })
        case let object as [String: Any]: return .object(try object.mapValues { try json($0) })
        default: throw HostCallError("failed", "The request is not plain JSON.")
        }
    }

    /// A `JSONValue` as the Foundation JSON NSH hands to JS.
    static func foundation(_ value: JSONValue) -> Any {
        switch value {
        case .object(let object): object.mapValues { foundation($0) }
        case .array(let list): list.map { foundation($0) }
        case .string(let text): text
        case .number(let number): number
        case .bool(let flag): flag
        case .null: NSNull()
        }
    }

    private static func canonical(_ value: Any) -> Data? { try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) }
}

/// A host call's rejection, as `"<code>: <message>"` (`PortError.from` reads it back).
struct HostCallError: LocalizedError, CustomStringConvertible {
    /// `relay.call` let go of a request whose answer did not come in time.
    static let noAnswer = "The desktop did not answer in time."
    let code: String
    let message: String
    init(_ code: String, _ message: String) { self.code = code; self.message = message }
    var errorDescription: String? { description }
    var description: String { "\(code): \(message)" }

    @MainActor static func from(_ error: any Error, dashboard: Bool = false) -> HostCallError {
        if let known = error as? HostCallError { return known }
        if RemoteError.isUnsupportedMethod(error) { return .init("unsupported", "The RiWork on this Mac does not offer this yet. Update RiWork on the Mac.") }
        if error is CancellationError { return .init("failed", "The request was cancelled.") }
        if let remote = error as? RemoteError {
            switch remote {
            case .rpc(let code, let message):
                switch code {
                case "not_found": return .init("not_found", message)
                case "already_exists", "request_conflict", "viewport_busy": return .init("conflict", message)
                case "pty_limit", "upload_limit", "cache_full", "response_too_large":
                    return .init("limit", dashboard ? RemoteModel.dashboardFailureText(error) : message)
                case "outcome_unknown": return .init("uncertain", remote.localizedDescription)
                default: return .init("failed", message)
                }
            case .uncertainDelivery: return .init("uncertain", remote.localizedDescription)
            case .disconnected, .relayClosed: return .init("offline", remote.localizedDescription)
            case .timeout: return .init("failed", "The desktop did not answer in time.")
            default: return .init("failed", remote.localizedDescription)
            }
        }
        if dashboard { return .init("failed", RemoteModel.dashboardFailureText(error)) }
        return .init("failed", error.localizedDescription)
    }
}

extension RemoteModel {
    /// A `dashboard.get` the shared TSX dashboard read (`relay.call`), installed as `readDashboard` installs its own, so the overview,
    /// the global orchestrator's status and the decision responder see the same read.
    func installDashboard(_ reply: JSONValue, token: UUID) {
        guard generation == token else { return }
        do {
            let snapshot = try DashboardSnapshot(wire: reply)
            if snapshot != dashboard { dashboard = snapshot }
            dashboardReadAt = .now; dashboardFailure = nil; dashboardSupport = .supported
        } catch { dashboardFailure = Self.dashboardFailureText(error) }
    }
    /// A read or write of the shared Projects and project pages (`relay.call`), installed as Swift's own would be: the native terminal,
    /// the native chat and the global screen see the same lists, and the activity loop skips a list the page has just read. Only the
    /// project Swift follows (the one the page shows, `navigate`) takes a project's lists.
    func installSharedRead(_ method: String, params: [String: JSONValue], reply: JSONValue, token: UUID) {
        guard generation == token, state == .connected else { return }
        switch method {
        case "projects.list":
            guard let listed = try? reply["projects"].decode([RemoteProject].self) else { return }
            if listed != projects { projects = listed }
            lastListRead[.projects] = .now
            return
        case "project.create":
            projectCreation = .supported
            guard let project = try? reply["project"].decode(RemoteProject.self) else { return }
            if !projects.contains(where: { $0.id == project.id }) { projects.append(project) }
            // Just made: counts as just active until the desktop has a figure for it, as the native sheet's did.
            touchedProjects[project.id] = project.created_at
            return
        case "shell.create":
            terminalControl = .supported
            guard let session = try? reply["shell"].decode(RemoteSession.self), session.project_id == projectID, loadedProjectID == projectID else { return }
            if let index = shells.firstIndex(where: { $0.id == session.id }) { shells[index] = session } else { shells.append(session) }
            return
        case "shell.close":
            terminalControl = .supported
            if case .string(let id)? = params["shell_id"] { shells.removeAll { $0.id == id } }
            return
        default: break
        }
        guard case .string(let project)? = params["project_id"], scope == .project, project == projectID, loadedProjectID == project else { return }
        switch method {
        case "tabs.list", "tabs.open", "tabs.update":
            guard let tabs = try? reply.decode(SharedTabsReply.self), acceptSharedTabs(tabs) else { return }
            reconcileChatSelection()
            try? reconcileSelectedSession()
        case "shells.list":
            guard let listed = try? reply["shells"].decode([RemoteSession].self) else { return }
            if listed != shells { shells = listed }
            lastListRead[.sessions] = .now
        case "chats.list":
            guard let request = try? ChatListRequest(projectID: project), let listed = try? request.parse(reply) else { return }
            installChats(listed, project: project)
        case "worktrees.list":
            if let listed = try? reply["worktrees"].decode([RemoteWorktree].self), listed != worktrees { worktrees = listed }
        default: break
        }
    }
    func dashboardReadFailed(_ error: any Error, token: UUID) async {
        guard generation == token, !(error is CancellationError) else { return }
        if RemoteError.isUnsupportedMethod(error) { dashboardSupport = .unsupported; dashboardFailure = nil; return }
        dashboardFailure = Self.dashboardFailureText(error)
        await noteLinkLossIfNeeded()
    }

    /// `keepFresh(.dashboard)` without its own `dashboard.get`, while the shared TSX dashboard reads that: the projects and the
    /// orchestrators its rows open with, at the dashboard's pace.
    func keepDashboardListsFresh() async {
        while !Task.isCancelled {
            var pause = dashboardRefreshInterval
            if state == .connected, appActive, !loading {
                if let read = lastListRead[.dashboard], ContinuousClock.now - read < dashboardRefreshInterval {
                    pause = dashboardRefreshInterval - (ContinuousClock.now - read)
                } else {
                    let token = generation
                    async let projectsRead: Void = refreshProjectsQuietly()
                    async let globalRead: Void = refreshGlobalQuietly()
                    _ = await (projectsRead, globalRead)
                    if generation == token { lastListRead[.dashboard] = .now }
                }
            }
            try? await Task.sleep(for: pause)
        }
    }
}
