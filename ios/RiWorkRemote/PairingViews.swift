import SwiftUI
import UIKit
import RiWorkCore

/// Pairing JSON is a credential. SwiftUI's TextEditor cannot turn off smart punctuation, inline
/// prediction or writing tools, any of which can rewrite `"`, `--` or base64url hyphens on device.
struct PairingCodeField: UIViewRepresentable {
    @Binding var text: String
    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.delegate = context.coordinator
        view.backgroundColor = .clear
        view.textColor = DesktopStyle.textUI
        view.tintColor = DesktopStyle.accentUI
        view.font = UIFontMetrics(forTextStyle: .caption1).scaledFont(for: UIFont(name: "Menlo", size: 11) ?? .monospacedSystemFont(ofSize: 11, weight: .regular))
        view.adjustsFontForContentSizeCategory = true
        view.autocorrectionType = .no
        view.autocapitalizationType = .none
        view.spellCheckingType = .no
        view.smartQuotesType = .no
        view.smartDashesType = .no
        view.smartInsertDeleteType = .no
        view.inlinePredictionType = .no
        view.mathExpressionCompletionType = .no
        view.writingToolsBehavior = .none
        view.dataDetectorTypes = []
        view.textContainerInset = UIEdgeInsets(top: 4, left: 4, bottom: 4, right: 4)
        view.accessibilityLabel = "Pairing JSON or deep link"
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return view
    }
    func updateUIView(_ view: UITextView, context: Context) {
        context.coordinator.parent = self
        if view.text != text { view.text = text }
    }
    @MainActor final class Coordinator: NSObject, UITextViewDelegate {
        var parent: PairingCodeField
        init(_ parent: PairingCodeField) { self.parent = parent }
        func textViewDidChange(_ textView: UITextView) { parent.text = textView.text }
    }
}

struct PairDesktopSheet: View {
    @Bindable var model: RemoteModel
    var initialText: String
    var fromLink = false
    var onAdded: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var code = ""
    @State private var allowLocal = false
    @State private var scanning = false
    @State private var confirming = false
    @State private var error: String?
    private var trimmedCode: String { code.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var parsed: Result<Pairing, any Error>? {
        trimmedCode.isEmpty ? nil : Result { try Pairing.parse(code, allowLocalDevelopment: allowLocal) }
    }
    private var pairing: Pairing? { if case .success(let pairing)? = parsed { pairing } else { nil } }
    private func save() {
        do { let id = try model.add(pairingText: code, name: name, allowLocal: allowLocal); code = ""; onAdded(id); dismiss() }
        catch { self.error = error.localizedDescription }
    }
    @ViewBuilder private var codeEntry: some View {
        Text("PAIRING CODE").font(.caption).foregroundStyle(DesktopStyle.muted)
        PairingCodeField(text: $code)
            .frame(height: 140)
            .background(DesktopStyle.background).overlay(Rectangle().stroke(DesktopStyle.divider, lineWidth: 1))
            .privacySensitive()
        HStack {
            PasteButton(payloadType: String.self) { values in if let value = values.first { code = value } }.buttonBorderShape(.roundedRectangle(radius: 3)).controlSize(.small)
            Spacer()
            Button("Scan QR", systemImage: "qrcode.viewfinder") { scanning = true }
        }
    }
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: "PAIR DESKTOP") { Button("Cancel") { code = ""; dismiss() } }
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    if fromLink {
                        Label("A link asked to pair this device", systemImage: "link.badge.plus").font(.custom("Menlo-Bold", size: 12, relativeTo: .subheadline))
                        Text("Any app or web page can open pairing links. Continue only if you just created this pairing on your own desktop.").foregroundStyle(DesktopStyle.warning)
                    } else {
                        Text("Paste the desktop’s pairing JSON or link. Treat this code like a password.").foregroundStyle(DesktopStyle.muted)
                    }
                    Text("DESKTOP NAME").font(.caption).foregroundStyle(DesktopStyle.muted)
                    TextField("e.g. Studio Mac", text: $name).textContentType(.name).modifier(DesktopField())
                    if let pairing {
                        Text("PAIRING DETAILS").font(.caption).foregroundStyle(DesktopStyle.muted)
                        PairingDetails(pairing: pairing)
                    }
                    // A link's raw base64 says nothing useful; show it only when it does not parse, so it can be fixed or dismissed.
                    if !(fromLink && pairing != nil) { codeEntry }
                    DesktopRule()
                    // ATS only allows cleartext loopback in Debug builds (see project.yml), so Release has no switch.
                    #if DEBUG
                    Toggle("Allow local development relay", isOn: $allowLocal).font(.caption).toggleStyle(.switch)
                    if allowLocal { Text("Loopback ws:// only. Use wss:// on real devices.").font(.caption).foregroundStyle(DesktopStyle.muted) }
                    #endif
                    if case .failure(let failure)? = parsed { Label(failure.localizedDescription, systemImage: "exclamationmark.circle").font(.caption).foregroundStyle(DesktopStyle.error) }
                    if let error { Label(error, systemImage: "exclamationmark.circle").font(.caption).foregroundStyle(DesktopStyle.error) }
                    Button(pairing.map { "Pair with \($0.relayHost)" } ?? "Save pairing & connect", systemImage: "link") {
                        if fromLink { confirming = true } else { save() }
                    }.buttonStyle(DesktopButtonStyle(prominent: true)).disabled(pairing == nil)
                    Text("End-to-end encrypted. Pairing keys stay in this device’s Keychain.").font(.caption).foregroundStyle(DesktopStyle.muted)
                }.padding(16).frame(maxWidth: 560, alignment: .leading).frame(maxWidth: .infinity)
            }
        }.background(DesktopStyle.background.ignoresSafeArea()).foregroundStyle(DesktopStyle.text)
            .font(.custom("Menlo", size: 13, relativeTo: .body)).tint(DesktopStyle.accent).buttonStyle(DesktopButtonStyle())
            .onAppear { code = initialText }
            .onChange(of: initialText) { _, text in code = text; error = nil }
            .alert("Pair with \(pairing?.relayHost ?? "this relay")?", isPresented: $confirming) {
                Button("Pair") { save() }
                Button("Cancel", role: .cancel) {}
            } message: {
                if let pairing { Text("Device “\(pairing.displayDeviceName)” for desktop \(pairing.desktopShortID) would connect through \(pairing.relayHost). Pair only if you created this yourself.") }
            }
            .sheet(isPresented: $scanning) { QRScannerSheet { value in code = value } }
            .presentationCornerRadius(8)
    }
}

private struct PairingDetails: View {
    let pairing: Pairing
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            row("Relay", pairing.relayHost + (pairing.usesLocalDevelopmentRelay ? " (local, unencrypted)" : ""))
            row("Device", pairing.displayDeviceName)
            row("Desktop", pairing.desktopShortID)
            if pairing.v == 2, pairing.invite_state != "established" {
                row("Invite", "Single-use until \(inviteDeadline(pairing.expires_at))")
            }
        }.padding(8).frame(maxWidth: .infinity, alignment: .leading)
            .background(DesktopStyle.panel).overlay(Rectangle().stroke(DesktopStyle.divider, lineWidth: 1))
            .accessibilityElement(children: .combine)
    }
    private func inviteDeadline(_ expires: UInt64?) -> String {
        guard let expires else { return "expiry missing" }
        return Date(timeIntervalSince1970: TimeInterval(expires)).formatted(date: .abbreviated, time: .shortened)
    }
    private func row(_ title: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(title.uppercased()).font(.caption).foregroundStyle(DesktopStyle.muted).frame(width: 72, alignment: .leading)
            Text(value).lineLimit(2).textSelection(.enabled)
        }
    }
}

struct RenameDesktopSheet: View {
    @Bindable var model: RemoteModel
    var desktop: SavedDesktop
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var error: String?
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: "RENAME DESKTOP") { Button("Cancel") { dismiss() } }
            VStack(alignment: .leading, spacing: 12) {
                Text("DESKTOP NAME").font(.caption).foregroundStyle(DesktopStyle.muted)
                TextField("Name", text: $name).modifier(DesktopField())
                if let error { Text(error).foregroundStyle(DesktopStyle.error) }
                Button("Save") { do { try model.rename(id: desktop.id, name: name); dismiss() } catch { self.error = error.localizedDescription } }
                    .buttonStyle(DesktopButtonStyle(prominent: true)).disabled(name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }.padding(16).frame(maxWidth: 560, alignment: .leading).frame(maxWidth: .infinity)
            Spacer(minLength: 0)
        }.background(DesktopStyle.background).foregroundStyle(DesktopStyle.text)
            .font(.custom("Menlo", size: 13, relativeTo: .body)).tint(DesktopStyle.accent).buttonStyle(DesktopButtonStyle())
            .onAppear { name = desktop.name }.presentationDetents([.medium]).presentationCornerRadius(8)
    }
}
