import SwiftUI
import RiWorkCore

/// One of RiWork's own workers, where the chat started it (`WorkerTimeline`). Read-only: the phone neither messages nor stops a worker.
/// Finished, it is one line that opens to the last lines of its output; continued, one muted line that points at the card below; live,
/// its status from the lists the phone reads anyway, and the time since it was noted, counted every second.
struct ChatWorkerCard: View {
    @Environment(\.desktopStyle) private var style
    let card: WorkerCard
    let isOpen: Bool
    let toggle: @MainActor () -> Void
    private var worker: ChatWorker { card.worker }
    private var kind: String {
        switch worker.kind { case .shell: "Worker shell"; case .chat: "Worker chat"; case .unknown: "Worker" }
    }
    var body: some View {
        Group {
            switch card.phase {
            case .continued: continued
            case .finished(let final): finished(final)
            case .listed, .unlisted, .unavailable:
                TimelineView(.periodic(from: .now, by: 1)) { context in live(now: Self.seconds(context.date)) }
            }
        }
        .font(style.face(11, relativeTo: .caption)).frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier("worker-\(worker.sessionID)")
    }
    static func seconds(_ date: Date) -> UInt64 { UInt64(max(0, date.timeIntervalSince1970)) }

    private var continued: some View {
        HStack(spacing: 6) {
            Image(systemName: "arrow.turn.left.down").accessibilityHidden(true)
            Text("\(kind) · \(worker.title)").lineLimit(1)
            Text("· Continued below").fixedSize()
            Spacer(minLength: 0)
        }
        .foregroundStyle(style.muted).padding(.horizontal, 8).padding(.vertical, 4)
        .accessibilityElement(children: .ignore).accessibilityLabel("\(kind), \(worker.title), continued below")
    }

    private func finished(_ final: ChatWorker.Final) -> some View {
        let now = Self.seconds(.now)
        let elapsed = card.elapsed(now: now).map(WorkerCard.duration)
        let status = card.status(now: now)
        return VStack(alignment: .leading, spacing: 4) {
            Button(action: toggle) {
                HStack(spacing: 6) {
                    Image(systemName: isOpen ? "chevron.down" : "chevron.right").accessibilityHidden(true)
                    ActivityIndicator(activity: card.activity(now: now))
                    Text("\(kind) · \(worker.title)").lineLimit(1)
                    Spacer(minLength: 4)
                    Text(([status] + [elapsed].compactMap { $0 }).joined(separator: " · "))
                        .foregroundStyle(final.status == .failed ? style.error : style.muted).fixedSize()
                }
                .frame(minHeight: style.target).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(kind), \(worker.title), \(status)" + (elapsed.map { ", \($0)" } ?? ""))
            .accessibilityValue(isOpen ? "Shown" : "Hidden")
            .accessibilityHint(isOpen ? "Hides its last output" : "Shows its last output")
            flags
            if isOpen {
                if let assignment = worker.assignment { Text(assignment).foregroundStyle(style.muted).fixedSize(horizontal: false, vertical: true) }
                if final.tail.isEmpty {
                    Text("No output reported.").foregroundStyle(style.muted)
                } else {
                    ScrollView(.horizontal, showsIndicators: false) {
                        Text(verbatim: final.tail).font(style.codeSmall).foregroundStyle(style.text).textSelection(.enabled).fixedSize(horizontal: true, vertical: true)
                    }
                }
            }
        }
        .padding(.horizontal, 8).padding(.vertical, 2)
        .background(style.cardHeader, in: RoundedRectangle(cornerRadius: 4))
    }

    private func live(now: UInt64) -> some View {
        let status = card.status(now: now)
        let elapsed = card.elapsed(now: now).map(WorkerCard.duration)
        let meta = [worker.harness?.capitalized, worker.model].compactMap { $0 }.joined(separator: " · ")
        return VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                ActivityIndicator(activity: card.activity(now: now))
                Text("\(kind) · \(worker.title)").lineLimit(1)
                Spacer(minLength: 4)
                Text(([status] + [elapsed].compactMap { $0 }).joined(separator: " · ")).foregroundStyle(style.muted).monospacedDigit().fixedSize()
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(kind), \(worker.title), \(status)" + (elapsed.map { ", \($0)" } ?? ""))
            if let assignment = worker.assignment { Text(assignment).foregroundStyle(style.muted).lineLimit(2) }
            flags
            if !meta.isEmpty { Text(meta).font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1) }
        }
        .padding(8)
        .background(style.cardHeader, in: RoundedRectangle(cornerRadius: 4))
    }

    @ViewBuilder private var flags: some View {
        let notes = [worker.late ? "Started earlier · first seen here" : nil, worker.followUp ? "Follow-up" : nil].compactMap { $0 }
        if !notes.isEmpty {
            Text(notes.joined(separator: " · ")).font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1)
        }
    }
}

/// "2 workers running ↑": over the transcript while a running worker card of this chat is out of view. A tap scrolls to the first.
struct WorkersRunningChip: View {
    @Environment(\.desktopStyle) private var style
    let title: String
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            if style.native {
                Text(title).font(style.face(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent)
                    .padding(.horizontal, 12).frame(minHeight: 44)
                    .background { if !style.glass { Capsule().fill(.ultraThinMaterial) } }
                    .nativeGlass(style, in: Capsule()).contentShape(Capsule())
            } else {
                Text(title).font(style.mono(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent)
                    .padding(.horizontal, 12).frame(minHeight: 44).background(.ultraThinMaterial, in: Capsule())
                    .overlay(Capsule().stroke(style.divider, lineWidth: 1)).contentShape(Capsule())
            }
        }
        .buttonStyle(.plain).padding(.horizontal, 10)
        .accessibilityIdentifier("chat-workers-running")
        .chatLayoutProbe("workers-chip", action: action)
        .accessibilityLabel(title.replacingOccurrences(of: " ↑", with: ""))
        .accessibilityHint("Scrolls to the first one")
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
