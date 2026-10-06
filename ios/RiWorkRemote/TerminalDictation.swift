import SwiftUI
import RiWorkCore

/// Dictation for a terminal, floating over the bottom of the pane (right above the key bar while the keyboard is up).
///
/// - **Listening:** the mic, what is heard so far, Cancel and Done.
/// - **Review** (direct typing): the text in a command field that has the keyboard, so it can be corrected before anything reaches the
///   shell. Return or Send types it with Return; Insert types it without, to finish it in the program's own prompt; Discard drops it.
///   A spoken line never goes to the shell unseen: a full-screen program would take its letters as commands.
///
/// With the line composer the text goes straight into the composer's field instead, which is a review step of its own.
struct TerminalDictationPanel: View {
    @Environment(\.desktopStyle) private var style
    let controller: DictationController
    /// The dictated line under review; nil when there is none.
    @Binding var review: String?
    let canType: Bool
    /// Types the line into the shell, with Return when `submit`.
    let type: (_ line: String, _ submit: Bool) -> Void
    /// The review is over (typed or discarded): the keyboard goes back to the terminal.
    var done: () -> Void = {}

    var body: some View {
        let phase = controller.phase(for: .terminal)
        if phase.isActive {
            card { listening(phase) }
        } else if let line = review {
            card { reviewing(line) }
        }
    }

    private func listening(_ phase: DictationPhase) -> some View {
        HStack(spacing: 6) {
            DictationGlyph(phase: phase, level: controller.level, size: 20)
            Group {
                if phase.text.isEmpty { Text(Self.status(phase)).foregroundStyle(style.muted) }
                else { Text(verbatim: phase.text).foregroundStyle(style.text) }
            }
            .font(style.mono(13, relativeTo: .body)).lineLimit(3).frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier("dictation.live")
            Button("Cancel dictation", systemImage: "xmark") { controller.cancel() }
                .labelStyle(.iconOnly).buttonStyle(DesktopButtonStyle(compact: true))
            Button(style.cased("Done")) { controller.stop() }
                .buttonStyle(DesktopButtonStyle(prominent: true, compact: true)).disabled(phase != .listening(text: phase.text))
                .accessibilityHint("Stops listening and shows the text to check")
        }
    }
    private func reviewing(_ line: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            CommandField(text: Binding(get: { review ?? "" }, set: { review = $0 }), placeholder: "Dictated command", isEnabled: true,
                         label: "Dictated command", onSubmit: { submit(true) }, onRejectedInput: {}, takesFocus: true)
                .modifier(DesktopField())
            HStack(spacing: 6) {
                Button("Discard", systemImage: "xmark") { review = nil; done() }
                    .labelStyle(.iconOnly).buttonStyle(DesktopButtonStyle(compact: true))
                    .accessibilityLabel("Discard dictated command")
                Text("Return sends").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1)
                Spacer(minLength: 4)
                Button(style.cased("Insert")) { submit(false) }
                    .buttonStyle(DesktopButtonStyle(compact: true)).disabled(!canSubmit)
                    .accessibilityHint("Types it into the terminal without Return")
                Button(style.cased("Send"), systemImage: "arrow.up") { submit(true) }
                    .labelStyle(.titleAndIcon).buttonStyle(DesktopButtonStyle(prominent: true, compact: true)).disabled(!canSubmit)
                    .accessibilityHint("Types it into the terminal followed by Return")
            }
        }
    }
    private var canSubmit: Bool { canType && !(review ?? "").trimmingCharacters(in: .whitespaces).isEmpty }
    private func submit(_ withReturn: Bool) {
        guard canSubmit, let line = review else { return }
        review = nil
        type(line, withReturn)
        done()
    }

    @ViewBuilder private func card<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        if style.native {
            content().padding(10).frame(maxWidth: .infinity, alignment: .leading)
                .background(style.glass ? AnyShapeStyle(.clear) : AnyShapeStyle(style.panel), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                .nativeGlass(style, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                .padding(.horizontal, 8).padding(.bottom, 6)
        } else {
            content().padding(8).frame(maxWidth: .infinity, alignment: .leading)
                .background(style.panel).overlay(alignment: .top) { DesktopRule() }
        }
    }

    static func status(_ phase: DictationPhase) -> String {
        switch phase {
        case .preparing(let note): note ?? "Starting…"
        case .finishing: "Finishing…"
        default: "Listening…"
        }
    }
}

/// The mic beside the line composer's Send. A view of its own, so the words heard rebuild only it.
struct TerminalMicButton: View {
    @Environment(\.desktopStyle) private var style
    var controller = DictationController.shared
    let isEnabled: Bool
    let action: () -> Void
    var body: some View {
        let phase = controller.phase(for: .terminal)
        Button(action: action) { DictationGlyph(phase: phase, level: controller.level, size: 20) }
            .buttonStyle(TargetButtonStyle(dims: false))
            .disabled(!isEnabled && !phase.isActive)
            .accessibilityLabel(phase.isActive ? "Stop dictation" : "Dictate").accessibilityIdentifier("dictation.terminal")
            .dictationAlert(controller, owner: .terminal)
    }
}

extension DictationController {
    /// What the key bar's mic shows for `owner`. A stored stage, not the phase: reading it does not rebuild a view for every word heard.
    func barState(for owner: DictationOwner) -> KeyBarView.Dictation {
        guard self.owner == owner else { return .idle }
        return stage
    }
}
