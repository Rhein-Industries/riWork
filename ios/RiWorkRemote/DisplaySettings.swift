import SwiftUI
import RiWorkCore

/// The Display section of the app's settings: how large the app's chrome is, how large the terminal text is, and the latency
/// overlay. Reachable from the terminal menu and from the desktop list. Every change applies at once and is remembered.
struct DisplaySettingsSheet: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.dismiss) private var dismiss
    @Bindable var model: RemoteModel
    /// The interface size under the finger. It is applied when the drag ends, so the sheet does not resize under it.
    @State private var draftScale = InterfaceScale.standard

    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: "DISPLAY") { Button("Done") { dismiss() } }
            List {
                Section {
                    row(title: "Interface size", value: "\(InterfaceScale.percent(draftScale))%") {
                        stepButton("Smaller interface", "minus", disabled: draftScale <= InterfaceScale.range.lowerBound) { commit(InterfaceScale.stepped(draftScale, by: -1)) }
                        Slider(value: $draftScale, in: InterfaceScale.range, step: InterfaceScale.step) { editing in
                            if !editing { commit(draftScale) }
                        }.accessibilityLabel("Interface size").accessibilityValue("\(InterfaceScale.percent(draftScale)) percent")
                        stepButton("Larger interface", "plus", disabled: draftScale >= InterfaceScale.range.upperBound) { commit(InterfaceScale.stepped(draftScale, by: 1)) }
                    }
                    resetRow("Reset to 100%", disabled: model.interfaceScale == InterfaceScale.standard) { commit(InterfaceScale.standard) }
                } header: { Text("APP").font(style.system(.caption)) } footer: {
                    Text("Headers, lists, the key bar and buttons. The terminal text has its own size.").font(style.system(.caption))
                }
                .listRowBackground(style.background).listRowSeparatorTint(style.divider)

                Section {
                    row(title: "Terminal text size", value: "\(Int(model.terminalFontSize)) pt") {
                        stepButton("Smaller text", "minus", disabled: model.terminalFontSize <= TerminalFontSize.range.lowerBound) { model.stepTerminalFontSize(-1) }
                        Slider(value: Binding(get: { model.terminalFontSize }, set: { model.setTerminalFontSize($0) }),
                               in: TerminalFontSize.range, step: TerminalFontSize.step)
                            .accessibilityLabel("Terminal text size").accessibilityValue("\(Int(model.terminalFontSize)) points")
                        stepButton("Larger text", "plus", disabled: model.terminalFontSize >= TerminalFontSize.range.upperBound) { model.stepTerminalFontSize(1) }
                    }
                    Text(TerminalText.textPresentation("$ ls  ⏺ ✻ ⚠ ✓ ● ⎿ Aa 0O"))
                        .font(.custom("Menlo", fixedSize: model.terminalFontSize)).foregroundStyle(style.terminalForeground)
                        .padding(.vertical, 6).padding(.horizontal, 8).frame(maxWidth: .infinity, alignment: .leading)
                        .background(style.terminalBackground).accessibilityHidden(true)
                    resetRow("Reset to \(Int(TerminalFontSize.standard)) pt", disabled: model.terminalFontSize == TerminalFontSize.standard) {
                        model.setTerminalFontSize(TerminalFontSize.standard)
                    }
                } header: { Text("TERMINAL").font(style.system(.caption)) } footer: {
                    Text("Pinching the terminal changes this too. The desktop pane follows the new grid.").font(style.system(.caption))
                }
                .listRowBackground(style.background).listRowSeparatorTint(style.divider)

                Section {
                    Toggle("Show latency", isOn: Binding(get: { model.showLatency }, set: { model.setShowLatency($0) }))
                        .frame(minHeight: style.pt(44))
                } header: { Text("DEBUG").font(style.system(.caption)) } footer: {
                    Text("A small overlay in the terminal: round trips of keys and screen reads, echo latency, the age of the last change, live or poll, and the last payload size.")
                        .font(style.system(.caption))
                }
                .listRowBackground(style.background).listRowSeparatorTint(style.divider)
            }
            .listStyle(.plain).scrollContentBackground(.hidden)
            .environment(\.defaultMinListRowHeight, style.pt(44))
        }
        .background(style.background).foregroundStyle(style.text)
        .font(style.mono(13)).tint(style.accent).buttonStyle(DesktopButtonStyle())
        .presentationDetents([.medium, .large]).presentationCornerRadius(8)
        .onAppear { draftScale = model.interfaceScale }
        .onChange(of: model.interfaceScale) { _, value in draftScale = value }
    }

    private func commit(_ value: Double) {
        draftScale = InterfaceScale.clamped(value)
        model.setInterfaceScale(value)
    }
    private func row<Controls: View>(title: String, value: String, @ViewBuilder controls: () -> Controls) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(title)
                Spacer(minLength: 8)
                Text(value).foregroundStyle(style.muted).monospacedDigit()
            }
            HStack(spacing: 0) { controls() }
        }.padding(.vertical, 4)
    }
    private func stepButton(_ label: String, _ symbol: String, disabled: Bool, action: @escaping () -> Void) -> some View {
        Button(label, systemImage: symbol, action: action).labelStyle(.iconOnly).disabled(disabled)
    }
    private func resetRow(_ title: String, disabled: Bool, action: @escaping () -> Void) -> some View {
        Button(title, systemImage: "arrow.counterclockwise", action: action).disabled(disabled).frame(maxWidth: .infinity, alignment: .leading)
    }
}
