import SwiftUI
import RiWorkCore

/// What the hotkey form edits: a hotkey whose steps can be rearranged and whose text may not be valid yet.
struct HotkeyDraft: Identifiable, Equatable {
    struct Step: Identifiable, Equatable {
        enum Kind: Equatable { case text, key }
        let id = UUID()
        var kind: Kind
        var text = ""
        var key = TerminalKey.enter
    }
    var id: String
    var isNew: Bool
    var label: String
    var steps: [Step]

    init() { id = UUID().uuidString.lowercased(); isNew = true; label = ""; steps = [] }
    init(_ hotkey: Hotkey) {
        id = hotkey.id; isNew = false; label = hotkey.label
        steps = hotkey.steps.map { step in
            switch step {
            case .text(let text): Step(kind: .text, text: text)
            case .key(let key): Step(kind: .key, key: key)
            }
        }
    }
    var hotkey: Hotkey {
        Hotkey(id: id, label: label.trimmingCharacters(in: .whitespaces), steps: steps.map { $0.kind == .text ? .text($0.text) : .key($0.key) })
    }
    /// Why this cannot be saved yet, if it cannot.
    var problem: HotkeyError? {
        do { try hotkey.validate(); return nil } catch { return error as? HotkeyError }
    }
}

/// Lists the hotkeys: add, edit, delete and reorder your own; the built-in ones are shown for reference.
struct HotkeyEditorSheet: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.dismiss) private var dismiss
    let store: HotkeyStore
    @State private var draft: HotkeyDraft?
    @State private var reordering = false

    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: "HOTKEYS") {
                if store.custom.count > 1 { Button(reordering ? "Finish" : "Reorder") { reordering.toggle() } }
                Button("Done") { dismiss() }
            }
            List {
                Section("Yours") {
                    ForEach(store.custom) { hotkey in
                        Button { draft = HotkeyDraft(hotkey) } label: { row(hotkey, tint: style.magenta) }.buttonStyle(.plain)
                            .accessibilityHint("Edit this hotkey")
                    }
                    .onDelete { store.remove(atOffsets: $0) }
                    .onMove { store.move(fromOffsets: $0, toOffset: $1) }
                    .listRowBackground(style.background)
                    Button("Add a hotkey", systemImage: "plus.circle") { draft = HotkeyDraft() }
                        .disabled(store.custom.count >= HotkeyLibrary.maxHotkeys).listRowBackground(style.background)
                }
                Section("Built in") {
                    ForEach(Hotkey.builtIn) { row($0, tint: style.muted) }.listRowBackground(style.background)
                }
                Section {
                    Text("A hotkey sends its steps to the terminal in order, like typing them. Text is typed as it is; use the Enter key step for a new line.")
                        .font(style.system(.caption)).foregroundStyle(style.muted).listRowBackground(style.background)
                }
            }
            .listStyle(.plain).scrollContentBackground(.hidden)
            .environment(\.editMode, .constant(reordering ? .active : .inactive))
        }
        .background(style.background).foregroundStyle(style.text)
        .font(style.mono(13, relativeTo: .body)).tint(style.accent).buttonStyle(DesktopButtonStyle())
        .sheet(item: $draft) { HotkeyForm(store: store, draft: $0).desktopThemed(style) }
        .presentationDetents([.medium, .large]).presentationCornerRadius(8)
    }
    private func row(_ hotkey: Hotkey, tint: Color) -> some View {
        HStack(spacing: 10) {
            Text(hotkey.label).font(style.mono(13, bold: true, relativeTo: .body)).foregroundStyle(tint).frame(minWidth: 64, alignment: .leading)
            Text(hotkey.summary).font(style.mono(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(1)
            Spacer(minLength: 0)
        }.frame(minHeight: style.pt(40)).contentShape(Rectangle())
    }
}

/// Adds or edits one hotkey: a name and a sequence of steps, each text or a special key.
struct HotkeyForm: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.dismiss) private var dismiss
    let store: HotkeyStore
    @State var draft: HotkeyDraft
    @State private var failure: String?

    init(store: HotkeyStore, draft: HotkeyDraft) { self.store = store; _draft = State(initialValue: draft) }

    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: draft.isNew ? "NEW HOTKEY" : "EDIT HOTKEY") {
                Button("Cancel") { dismiss() }
                Button("Save") { save() }.disabled(draft.problem != nil)
            }
            List {
                Section("Name on the key bar") {
                    TextField("e.g. Clear", text: $draft.label).modifier(DesktopField()).autocorrectionDisabled().textInputAutocapitalization(.never)
                        .onChange(of: draft.label) { _, text in if text.count > Hotkey.maxLabelLength { draft.label = String(text.prefix(Hotkey.maxLabelLength)) } }
                        .listRowBackground(style.background)
                }
                Section("Steps, in order") {
                    ForEach($draft.steps) { $step in stepRow($step) }
                        .onDelete { draft.steps.remove(atOffsets: $0) }
                        .onMove { draft.steps.move(fromOffsets: $0, toOffset: $1) }
                        .listRowBackground(style.background)
                    HStack(spacing: 16) {
                        Button("Add text", systemImage: "textformat") { draft.steps.append(.init(kind: .text)) }
                        Menu {
                            ForEach(TerminalKey.choices.filter { if case .control = $0 { false } else { true } }, id: \.self) { key in
                                Button(key.title) { draft.steps.append(.init(kind: .key, key: key)) }
                            }
                            Menu("Ctrl + letter") {
                                ForEach(TerminalKey.choices.filter { if case .control = $0 { true } else { false } }, id: \.self) { key in
                                    Button(key.title) { draft.steps.append(.init(kind: .key, key: key)) }
                                }
                            }
                        } label: { Label("Add key", systemImage: "command") }
                        Spacer(minLength: 0)
                    }.disabled(draft.steps.count >= Hotkey.maxSteps).listRowBackground(style.background)
                }
                Section("Sends") {
                    Text(draft.hotkey.summary.isEmpty ? "Nothing yet" : draft.hotkey.summary).font(style.mono(12, relativeTo: .caption)).foregroundStyle(style.muted)
                    if let problem = draft.problem, showsProblem {
                        Label(problem.localizedDescription, systemImage: "exclamationmark.circle").font(style.system(.caption)).foregroundStyle(style.warning)
                    }
                    if let failure { Label(failure, systemImage: "exclamationmark.circle").font(style.system(.caption)).foregroundStyle(style.error) }
                }.listRowBackground(style.background)
                if !draft.isNew {
                    Section {
                        Button("Delete this hotkey", systemImage: "trash", role: .destructive) { store.remove(id: draft.id); dismiss() }.listRowBackground(style.background)
                    }
                }
            }
            .listStyle(.plain).scrollContentBackground(.hidden)
            // Drag handles and delete controls stay visible: this list is only ever edited.
            .environment(\.editMode, .constant(.active))
        }
        .background(style.background).foregroundStyle(style.text)
        .font(style.mono(13, relativeTo: .body)).tint(style.accent).buttonStyle(DesktopButtonStyle())
        .presentationDetents([.large]).presentationCornerRadius(8)
    }
    /// The first thing a new form says is not a complaint.
    private var showsProblem: Bool { !(draft.isNew && draft.label.isEmpty && draft.steps.isEmpty) }
    @ViewBuilder private func stepRow(_ step: Binding<HotkeyDraft.Step>) -> some View {
        HStack(spacing: 8) {
            Image(systemName: step.wrappedValue.kind == .text ? "textformat" : "command").foregroundStyle(style.muted).frame(width: 20)
            if step.wrappedValue.kind == .text {
                TextField("Text to type", text: step.text).textFieldStyle(.plain).autocorrectionDisabled().textInputAutocapitalization(.never)
            } else {
                Picker("Key", selection: step.key) { ForEach(TerminalKey.choices, id: \.self) { Text($0.title).tag($0) } }.pickerStyle(.menu).labelsHidden()
                Spacer(minLength: 0)
            }
        }.frame(minHeight: style.pt(40))
    }
    private func save() {
        let hotkey = draft.hotkey
        do {
            if draft.isNew { try store.add(hotkey) } else { try store.update(hotkey) }
            dismiss()
        } catch { failure = error.localizedDescription }
    }
}
