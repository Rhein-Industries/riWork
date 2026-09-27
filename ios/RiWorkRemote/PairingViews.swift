import SwiftUI
import RiWorkCore

struct PairDesktopSheet: View {
    @Bindable var model: RemoteModel
    var initialText: String
    var onAdded: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var code = ""
    @State private var allowLocal = false
    @State private var scanning = false
    @State private var error: String?
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: "PAIR DESKTOP") { Button("Cancel") { code = ""; dismiss() } }
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text("Paste the desktop’s pairing JSON or link. Treat this code like a password.").foregroundStyle(DesktopStyle.muted)
                    Text("DESKTOP NAME").font(.caption).foregroundStyle(DesktopStyle.muted)
                    TextField("e.g. Studio Mac", text: $name).textContentType(.name).modifier(DesktopField())
                    Text("PAIRING CODE").font(.caption).foregroundStyle(DesktopStyle.muted)
                    TextEditor(text: $code).font(.custom("Menlo", size: 11, relativeTo: .caption))
                        .frame(height: 140).scrollContentBackground(.hidden).padding(4)
                        .background(DesktopStyle.background).overlay(Rectangle().stroke(DesktopStyle.divider, lineWidth: 1))
                        .autocorrectionDisabled().textInputAutocapitalization(.never).privacySensitive().accessibilityLabel("Pairing JSON or deep link")
                    HStack {
                        PasteButton(payloadType: String.self) { values in if let value = values.first { code = value } }.buttonBorderShape(.roundedRectangle(radius: 3)).controlSize(.small)
                        Spacer()
                        Button("Scan QR", systemImage: "qrcode.viewfinder") { scanning = true }
                    }
                    DesktopRule()
                    Toggle("Allow local development relay", isOn: $allowLocal).font(.caption).toggleStyle(.switch)
                    if allowLocal { Text("Loopback ws:// only. Use wss:// on real devices.").font(.caption).foregroundStyle(DesktopStyle.muted) }
                    if let error { Label(error, systemImage: "exclamationmark.circle").font(.caption).foregroundStyle(DesktopStyle.error) }
                    Button("Save pairing & connect", systemImage: "link") {
                        do { let id = try model.add(pairingText: code, name: name, allowLocal: allowLocal); code = ""; onAdded(id); dismiss() }
                        catch { self.error = error.localizedDescription }
                    }.buttonStyle(DesktopButtonStyle(prominent: true)).disabled(code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    Text("End-to-end encrypted. Pairing keys stay in this device’s Keychain.").font(.caption).foregroundStyle(DesktopStyle.muted)
                }.padding(16).frame(maxWidth: 560, alignment: .leading).frame(maxWidth: .infinity)
            }
        }.background(DesktopStyle.background.ignoresSafeArea()).foregroundStyle(DesktopStyle.text)
            .font(.custom("Menlo", size: 13, relativeTo: .body)).tint(DesktopStyle.accent).buttonStyle(DesktopButtonStyle())
            .onAppear { code = initialText }
            .onChange(of: initialText) { _, text in code = text }
            .sheet(isPresented: $scanning) { QRScannerSheet { value in code = value } }
            .presentationCornerRadius(8)
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
