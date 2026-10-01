import Foundation
import Network
import RiWorkCore

/// What the model asks of the network path: the conditions to fetch history under, and which kind of link it is (a change of kind
/// makes what was measured about the old one worthless).
@MainActor protocol LinkWatching: AnyObject {
    var conditions: LinkConditions { get }
    /// "wifi", "cellular", "wired", "other" or "none": the link the traffic leaves on.
    var interface: String { get }
    var onChange: (@MainActor () -> Void)? { get set }
    func start()
    func stop()
}

/// The system's view: `NWPathMonitor` (Low Data Mode is `isConstrained`; cellular and personal hotspots are `isExpensive`) and Low
/// Power Mode. The relay is usually reached through Tailscale's tunnel; the path monitor looks through it at the interface under it
/// where the system says so, and the measurements of the pages themselves decide the rest.
///
/// Through a tunnel the path is restless: while the VPN connects, re-keys or roams, the monitor reports several paths within seconds,
/// and the flags and the interface switch with them. The flags are passed on as they come (the model holds a restriction on, see
/// `ConditionHold`); a change of *interface* is passed on only once it has lasted `InterfaceDebounce.settleSeconds`, because it makes
/// the model forget what it measured of the link. The first path, and a path back to the interface it had before a dropout, change
/// nothing at all.
@MainActor final class SystemLinkWatcher: LinkWatching {
    private(set) var conditions = LinkConditions()
    var interface: String { debounce.interface }
    var onChange: (@MainActor () -> Void)?
    private var monitor: NWPathMonitor?
    private var power: (any NSObjectProtocol)?
    private var path: NWPath?
    private var debounce = InterfaceDebounce()
    private var settling: Task<Void, Never>?
    private var now: Double { ProcessInfo.processInfo.systemUptime }

    func start() {
        guard monitor == nil else { return }
        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { [weak self] path in
            Task { @MainActor in self?.apply(path) }
        }
        monitor.start(queue: DispatchQueue(label: "com.riwork.remote.path", qos: .utility))
        self.monitor = monitor
        power = NotificationCenter.default.addObserver(forName: .NSProcessInfoPowerStateDidChange, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
        refresh()
    }
    func stop() {
        monitor?.cancel(); monitor = nil
        if let power { NotificationCenter.default.removeObserver(power) }
        power = nil
        settling?.cancel(); settling = nil
    }

    private func apply(_ path: NWPath) {
        self.path = path
        refresh()
    }
    private func refresh() {
        let lowPower = ProcessInfo.processInfo.isLowPowerModeEnabled
        let next = LinkConditions(constrained: path?.isConstrained ?? false, expensive: path?.isExpensive ?? false, lowPower: lowPower)
        var changed = false
        if next != conditions { conditions = next; changed = true }
        // Before the first path there is nothing to say about the interface.
        if let path {
            let kind: String
            if path.status != .satisfied { kind = "none" }
            else if path.usesInterfaceType(.wifi) { kind = "wifi" }
            else if path.usesInterfaceType(.cellular) { kind = "cellular" }
            else if path.usesInterfaceType(.wiredEthernet) { kind = "wired" }
            else { kind = "other" }
            if debounce.report(kind, at: now) { changed = true }
            scheduleSettle()
        }
        if changed { onChange?() }
    }
    private func scheduleSettle() {
        settling?.cancel(); settling = nil
        guard let delay = debounce.settleDelay(at: now) else { return }
        settling = Task { [weak self] in
            try? await Task.sleep(for: .seconds(delay + 0.01))
            guard !Task.isCancelled, let self else { return }
            self.settling = nil
            if self.debounce.settle(at: self.now) { self.onChange?() }
        }
    }
}

/// A fixed answer, for tests and for a model that has no watcher.
@MainActor final class StaticLinkWatcher: LinkWatching {
    var conditions: LinkConditions { didSet { if conditions != oldValue { onChange?() } } }
    var interface: String { didSet { if interface != oldValue { onChange?() } } }
    var onChange: (@MainActor () -> Void)?
    init(conditions: LinkConditions = LinkConditions(), interface: String = "wifi") { self.conditions = conditions; self.interface = interface }
    func start() {}
    func stop() {}
}
