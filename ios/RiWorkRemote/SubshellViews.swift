import SwiftUI
import RiWorkCore

/// Uses the phone's existing list subscription. No shell input/stop commands are added.
struct ChatWorkerShells: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    let project: String?
    @State private var collapsed: Set<String> = []
    private var cards: [WorkerShellCard] { WorkerShellCard.project(project, sessions: model.shells, tasks: model.workerTasks) }
    var body: some View {
        if !cards.isEmpty {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                ScrollView {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Worker shells · " + freshness(context.date)).font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                        ForEach(cards) { card in
                            let open = !collapsed.contains(card.id)
                            VStack(alignment: .leading, spacing: 4) {
                                Button {
                                    if collapsed.contains(card.id) { collapsed.remove(card.id) } else { collapsed.insert(card.id) }
                                } label: {
                                    HStack {
                                        Image(systemName: open ? "chevron.down" : "chevron.right")
                                        Text(card.title).lineLimit(1)
                                        Spacer(minLength: 4)
                                        Text(card.status).foregroundStyle(style.muted)
                                    }
                                }.buttonStyle(.plain).accessibilityLabel("\(card.title), \(card.status), \(open ? "expanded" : "collapsed")")
                                if open {
                                    let elapsed = card.elapsed(now: UInt64(max(0, context.date.timeIntervalSince1970)))
                                    Text("\(card.harness) · \(card.model ?? "Model unavailable") · \(elapsed / 60)m \(elapsed % 60)s")
                                        .font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                                    ForEach(card.tasks.dropFirst()) { task in Text(task.title).lineLimit(2) }
                                    Text("Output in terminal").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                                }
                            }.font(style.face(11, relativeTo: .caption)).padding(6).background(style.cardHeader, in: RoundedRectangle(cornerRadius: 4))
                                .accessibilityIdentifier("worker-\(card.id)")
                        }
                    }.padding(.horizontal, 16).padding(.vertical, 6)
                }.frame(maxHeight: style.pt(180))
            }
            .onAppear { collapseFinished() }
            .onChange(of: cards.map { "\($0.id):\($0.finished)" }) { old, _ in
                for card in cards {
                    if card.finished, !old.contains("\(card.id):true") { collapsed.insert(card.id) }
                    if !card.finished, old.contains("\(card.id):true") { collapsed.remove(card.id) }
                }
            }
        }
    }
    private func collapseFinished() { for card in cards where card.finished { collapsed.insert(card.id) } }
    private func freshness(_ now: Date) -> String {
        let age = model.workerShellsCheckedAt.map { "last checked \(Int(max(0, now.timeIntervalSince($0))))s ago" } ?? "not checked yet"
        if !model.appActive || model.state != .connected { return "Paused · " + age }
        if model.workerShellListFailed { return "Check failed · " + age }
        return age
    }
}

struct ChatSubagentCard: View {
    @Environment(\.desktopStyle) private var style
    let run: ChatSubagent
    let isOpen: Bool
    let toggle: @MainActor () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button(action: toggle) {
                HStack {
                    Image(systemName: isOpen ? "chevron.down" : "chevron.right")
                    Text("Subagent · " + run.name).lineLimit(1)
                    Spacer(minLength: 4)
                    Text(run.status.title).foregroundStyle(run.status == .failed ? style.error : style.muted)
                }
            }.buttonStyle(.plain).disabled(run.result.isEmpty)
                .accessibilityLabel("Subagent, \(run.name), \(run.status.title), result summary")
            if let purpose = run.purpose { Text(purpose).foregroundStyle(style.muted).lineLimit(2) }
            if isOpen, !run.result.isEmpty {
                Text(verbatim: run.result).font(style.face(11, relativeTo: .caption)).textSelection(.enabled)
            }
        }.font(style.face(11, relativeTo: .caption)).padding(8).frame(maxWidth: .infinity, alignment: .leading)
            .background(style.cardHeader, in: RoundedRectangle(cornerRadius: 4))
    }
}
