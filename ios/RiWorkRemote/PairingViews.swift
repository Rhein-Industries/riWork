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
        NavigationStack {
            Form {
                Section {
                    Label("Pair your desktop", systemImage: "lock.shield").font(.headline)
                    Text("Create a device pairing on the RiWork desktop, then paste its JSON or pairing link here. Treat the code like a password.").font(.subheadline).foregroundStyle(.secondary)
                }
                Section("Desktop name") { TextField("e.g. Studio Mac", text: $name).textContentType(.name) }
                Section("Pairing code") {
                    TextEditor(text: $code).font(.caption.monospaced()).frame(minHeight: 140).autocorrectionDisabled().textInputAutocapitalization(.never).privacySensitive().accessibilityLabel("Pairing JSON or deep link")
                    HStack {
                        PasteButton(payloadType: String.self) { values in if let value = values.first { code = value } }
                        Spacer()
                        Button("Scan QR", systemImage: "qrcode.viewfinder") { scanning = true }
                    }
                }
                Section {
                    Toggle("Allow local development relay", isOn: $allowLocal)
                    if allowLocal { Text("Allows ws:// only on localhost or a loopback address. Use wss:// for real devices and networks.").font(.caption).foregroundStyle(.secondary) }
                }
                if let error { Section { Label(error, systemImage: "exclamationmark.circle").font(.subheadline).foregroundStyle(.red) } }
                Section {
                    Button("Save pairing & connect", systemImage: "link") {
                        do { let id = try model.add(pairingText: code, name: name, allowLocal: allowLocal); code = ""; onAdded(id); dismiss() }
                        catch { self.error = error.localizedDescription }
                    }.disabled(code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                Section { Text("Encrypted end to end between this device and your desktop. Pairing keys are stored in Keychain on this device.").font(.footnote).foregroundStyle(.secondary) }
            }
            .navigationTitle("Add desktop").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { code = ""; dismiss() } } }
            .onAppear { code = initialText }
            .onChange(of: initialText) { _, text in code = text }
            .sheet(isPresented: $scanning) { QRScannerSheet { value in code = value } }
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
        NavigationStack {
            Form {
                Section("Desktop name") { TextField("Name", text: $name) }
                if let error { Text(error).foregroundStyle(.red) }
            }.navigationTitle("Rename desktop").navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                    ToolbarItem(placement: .confirmationAction) { Button("Save") { do { try model.rename(id: desktop.id, name: name); dismiss() } catch { self.error = error.localizedDescription } }.disabled(name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) }
                }.onAppear { name = desktop.name }
        }.presentationDetents([.medium])
    }
}
