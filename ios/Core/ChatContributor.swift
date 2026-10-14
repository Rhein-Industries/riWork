import Foundation

/// The Contributor control uses only what the host reports. A host that leaves Muse to its
/// settings has an unknown mode; the first tap asks for off, as the desktop control does.
public struct ChatContributorChoice: Sendable, Equatable {
    public let mode: ChatContributorMode?

    public init?(info: ChatInfo) {
        guard info.provider == .muse else { return nil }
        mode = info.reportedContributorMode
    }

    public var title: String {
        switch mode {
        case .on: "Contributor on"
        case .off: "Contributor off"
        case nil: "Contributor unknown"
        }
    }
    public var detail: String {
        switch mode {
        case .on: "Turn contributor mode off for this chat. Muse restarts on the same conversation."
        case .off: "Turn contributor mode on for this chat. Muse restarts on the same conversation."
        case nil: "This Mac has not reported an on or off mode. Turn it off; update RiWork on the Mac if it is refused."
        }
    }
    public var nextMode: ChatContributorMode { mode == .off ? .on : .off }
    public var command: ChatCommand { .configure(contributorMode: nextMode) }
}

/// A fresh snapshot can arrive before an older in-flight event page. Keep its Contributor
/// report until the event stream catches up, without replacing or skipping transcript history.
/// Nothing here records the mode requested by the phone, and nothing is persisted locally.
public struct ChatContributorState: Sendable, Equatable {
    public private(set) var info: ChatInfo?
    public private(set) var through: UInt64 = 0

    public init() {}
    public mutating func observe(_ info: ChatInfo, through sequence: UInt64) {
        guard sequence >= through else { return }
        self.info = info; through = sequence
    }
    public func choice(fallback: ChatInfo) -> ChatContributorChoice? {
        ChatContributorChoice(info: info ?? fallback)
    }
}
