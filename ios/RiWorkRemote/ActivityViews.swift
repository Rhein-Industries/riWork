import SwiftUI
import RiWorkCore

// What the agents in the terminals are doing, drawn with the synced desktop colors. One shape for each state, so color is never the
// only difference: working is a pulsing dot in the accent color (with "+N" for the subagents at work), waiting is a filled gold
// disc with an exclamation mark, done is a small muted check. Unknown and exited draw nothing, which is how they looked before.

/// The dot that says an agent is working. It pulses; with Reduce Motion it just sits there.
private struct WorkingDot: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @ScaledMetric(relativeTo: .caption2) private var size = 8.0
    var body: some View {
        Image(systemName: "circle.fill").font(.system(size: size * style.scale)).foregroundStyle(style.accent)
            .symbolEffect(.pulse, options: .repeating, isActive: !reduceMotion)
    }
}

/// The badge that says an agent needs a person.
private struct WaitingDisc: View {
    @Environment(\.desktopStyle) private var style
    @ScaledMetric(relativeTo: .caption2) private var size = 13.0
    var body: some View {
        Image(systemName: "exclamationmark").font(.system(size: size * 0.7 * style.scale, weight: .black)).foregroundStyle(style.background)
            .frame(width: size * style.scale, height: size * style.scale).background(Circle().fill(style.gold))
    }
}

/// The state of one terminal's agent, beside its name in the tab strip.
struct ActivityIndicator: View {
    @Environment(\.desktopStyle) private var style
    let activity: AgentActivity
    var subagents = 0
    var body: some View {
        Group {
            switch activity {
            case .working:
                HStack(spacing: 3) {
                    WorkingDot()
                    if subagents > 0 { Text("+\(subagents)").font(style.mono(10, bold: true, relativeTo: .caption2)).foregroundStyle(style.accent).monospacedDigit() }
                }
            case .waiting:
                WaitingDisc()
            case .done:
                Image(systemName: "checkmark").font(style.system(.caption2, weight: .semibold)).foregroundStyle(style.muted)
            case .unknown, .exited:
                EmptyView()
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(activity.spoken(subagents: subagents) ?? "")
        .accessibilityHidden(!activity.isShown)
    }
}

/// The agents at work in one project, on its row: how many are working and how many are waiting for input.
struct ProjectAgentBadges: View {
    @Environment(\.desktopStyle) private var style
    let agents: ProjectAgents
    var body: some View {
        HStack(spacing: 8) {
            if agents.working > 0 {
                HStack(spacing: 3) {
                    WorkingDot()
                    Text("\(agents.working)").font(style.mono(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent).monospacedDigit()
                }
            }
            if agents.waiting > 0 {
                HStack(spacing: 3) {
                    WaitingDisc()
                    Text("\(agents.waiting)").font(style.mono(11, bold: true, relativeTo: .caption)).foregroundStyle(style.gold).monospacedDigit()
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(agents.spoken ?? "")
        .accessibilityHidden(agents.isIdle)
    }
}

/// The sort choice for the project list: a menu in the search row that names the order in use. The same order is stepped through by ⌘O.
struct ProjectSortMenu: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    var body: some View {
        Menu {
            Picker("Sort projects", selection: Binding(get: { model.projectSort }, set: { model.setProjectSort($0) })) {
                ForEach(ProjectSort.allCases) { sort in Text("\(sort.title) · \(sort.detail)").tag(sort) }
            }
        } label: {
            Label(model.projectSort.title, systemImage: "arrow.up.arrow.down").font(style.mono(11, relativeTo: .caption)).lineLimit(1)
                .frame(minHeight: style.pt(44)).contentShape(Rectangle())
        }
        .fixedSize()
        .accessibilityLabel("Sort projects")
        .accessibilityValue("\(model.projectSort.title), \(model.projectSort.detail)")
        .accessibilityHint("Choose Recent, Name or Date added. On a hardware keyboard, Command O steps to the next order.")
    }
}
