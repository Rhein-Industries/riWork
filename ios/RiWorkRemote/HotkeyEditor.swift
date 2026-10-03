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
    /// The keyboard shortcut that runs it without opening the hotkey menu.
    var chord: KeyChord?
    var showsOnBar = true

    init() { id = UUID().uuidString.lowercased(); isNew = true; label = ""; steps = [] }
    init(_ hotkey: Hotkey) {
        id = hotkey.id; isNew = false; label = hotkey.label; chord = hotkey.chord; showsOnBar = hotkey.showsOnBar
        steps = hotkey.steps.map { step in
            switch step {
            case .text(let text): Step(kind: .text, text: text)
            case .key(let key): Step(kind: .key, key: key)
            }
        }
    }
    var hotkey: Hotkey {
        Hotkey(id: id, label: label.trimmingCharacters(in: .whitespaces), steps: steps.map { $0.kind == .text ? .text($0.text) : .key($0.key) }, chord: chord, showsOnBar: showsOnBar)
    }
    /// Why this cannot be saved yet, if it cannot.
    var problem: HotkeyError? {
        do { try hotkey.validate(); return nil } catch { return error as? HotkeyError }
    }
}

/// Where the editor opens: the list, a fresh form, or the form of one hotkey.
enum HotkeyEditorStart: Identifiable {
    case list, new, edit(Hotkey)
    var id: String { switch self { case .list: "list"; case .new: "new"; case .edit(let hotkey): "edit.\(hotkey.id)" } }
    var draft: HotkeyDraft? {
        switch self {
        case .list: nil
        case .new: HotkeyDraft()
        case .edit(let hotkey): HotkeyDraft(hotkey)
        }
    }
}

/// A shortcut as a small outlined tag.
struct ChordTag: View {
    @Environment(\.desktopStyle) private var style
    let chord: KeyChord
    var body: some View {
        Text(chord.title).font(style.mono(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent)
            .padding(.horizontal, 5).padding(.vertical, 1)
            .overlay(RoundedRectangle(cornerRadius: 3).stroke(style.divider, lineWidth: 1))
            .accessibilityLabel("Shortcut \(chord.title)")
    }
}

/// Lists the hotkeys: add, edit, delete and reorder your own, install a keyboard template, set shortcuts for the hotkey menu and try
/// keys; the built-in ones are shown for reference.
///
/// Everything is reachable from the keyboard: ↑ ↓ (or Ctrl-P, Ctrl-N, Tab) select a row, Return opens or runs it, ⌘N adds a hotkey,
/// ⌘⌫ deletes the selected one, ⌘↑ ⌘↓ reorder it, Esc (or ⌘.) closes.
struct HotkeyEditorSheet: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.dismiss) private var dismiss
    let store: HotkeyStore
    let keyboard: KeyboardPrefs
    @State private var draft: HotkeyDraft?
    @State private var reordering = false
    @State private var selection: Row = .add
    @State private var notice: String?
    @State private var learning: ShortcutTarget?
    @State private var learnProblem: String?
    @State private var testing = false
    /// The hotkey the form was last open for, so the list selects it again when the form closes.
    @State private var lastDraftID: String?

    /// The rows the keyboard walks through, top to bottom.
    enum Row: Hashable { case hotkey(String), add, template(String), menuShortcut, helpShortcut, tester }
    private var rows: [Row] { store.custom.map { Row.hotkey($0.id) } + [.add] + HotkeyTemplate.all.map { Row.template($0.id) } + [.menuShortcut, .helpShortcut, .tester] }
    /// What a learned shortcut opens: the hotkey menu (⌘K) or the hotkey help (⌘/).
    enum ShortcutTarget {
        case menu, help
        var row: Row { self == .menu ? .menuShortcut : .helpShortcut }
        var title: String { self == .menu ? "Hotkey menu" : "Hotkey help" }
        var fixed: KeyChord { self == .menu ? .paletteDefault : .helpDefault }
        var noun: String { self == .menu ? "menu" : "help" }
    }

    init(store: HotkeyStore, keyboard: KeyboardPrefs, start: HotkeyEditorStart = .list) {
        self.store = store; self.keyboard = keyboard
        _draft = State(initialValue: start.draft)
        if case .edit(let hotkey) = start { _selection = State(initialValue: .hotkey(hotkey.id)) }
        else if let first = store.custom.first { _selection = State(initialValue: .hotkey(first.id)) }
    }

    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: "HOTKEYS") {
                if store.custom.count > 1 { Button(reordering ? "Finish" : "Reorder") { reordering.toggle() } }
                Button("Done") { dismiss() }
            }
            ScrollViewReader { proxy in
                List {
                    Section("Yours") {
                        ForEach(store.custom) { hotkey in
                            Button { selection = .hotkey(hotkey.id); draft = HotkeyDraft(hotkey) } label: { row(hotkey, tint: style.magenta) }.buttonStyle(.plain)
                                .accessibilityHint("Edit this hotkey")
                                .listRowBackground(rowBackground(.hotkey(hotkey.id))).id(Row.hotkey(hotkey.id))
                        }
                        .onDelete { store.remove(atOffsets: $0) }
                        .onMove { store.move(fromOffsets: $0, toOffset: $1) }
                        Button("Add a hotkey", systemImage: "plus.circle") { selection = .add; draft = HotkeyDraft() }
                            .disabled(store.custom.count >= HotkeyLibrary.maxHotkeys).listRowBackground(rowBackground(.add)).id(Row.add)
                    }
                    Section("Keyboard templates") {
                        ForEach(HotkeyTemplate.all) { template in templateRow(template).id(Row.template(template.id)) }
                        hint("Adds to your hotkeys; nothing of yours is removed or changed. The shortcuts need a hardware keyboard.")
                    }
                    Section("Keyboard") {
                        shortcutRow(.menu).id(Row.menuShortcut)
                        shortcutRow(.help).id(Row.helpShortcut)
                        testerRow.id(Row.tester)
                        hint("⌘K opens the hotkey menu from any shell, ⌘/ the hotkey help, which lists every hotkey with its shortcut. The tester shows what a key sends, for example the Clicks button.")
                    }
                    Section("Built in") {
                        ForEach(Hotkey.builtIn) { row($0, tint: style.muted) }.listRowBackground(style.background)
                    }
                    Section {
                        hint("A hotkey sends its steps to the terminal in order, like typing them. Text is typed as it is; use the Enter key step for a new line. A shortcut runs a hotkey without opening the menu.")
                    }
                }
                .listStyle(.plain).scrollContentBackground(.hidden)
                .environment(\.editMode, .constant(reordering ? .active : .inactive))
                .onChange(of: selection) { _, row in withAnimation(.easeInOut(duration: 0.15)) { proxy.scrollTo(row) } }
            }
        }
        .background(style.background).foregroundStyle(style.text)
        .font(style.mono(13, relativeTo: .body)).tint(style.accent).buttonStyle(DesktopButtonStyle())
        .background { KeyCommandHost(active: draft == nil && learning == nil && !testing, actions: listActions) }
        .background {
            KeyLearnView(active: learning != nil || testing, onEvent: { keyboard.events.record($0) },
                         onChord: learning != nil ? { learnShortcut($0) } : nil, onCancel: { learning = nil; learnProblem = nil })
        }
        .onChange(of: draft?.id) { _, id in if let id { lastDraftID = id } }
        .sheet(item: $draft, onDismiss: { if let id = lastDraftID, store.custom.contains(where: { $0.id == id }) { selection = .hotkey(id) } }) {
            HotkeyForm(store: store, keyboard: keyboard, draft: $0).desktopThemed(style)
        }
        .presentationDetents([.medium, .large]).presentationCornerRadius(8)
    }

    // MARK: Rows

    private func hint(_ text: String) -> some View {
        Text(text).font(style.system(.caption)).foregroundStyle(style.muted).listRowBackground(style.background)
    }
    private func rowBackground(_ row: Row) -> Color { selection == row ? style.active : style.background }
    private func row(_ hotkey: Hotkey, tint: Color) -> some View {
        HStack(spacing: 10) {
            Text(hotkey.label).font(style.mono(13, bold: true, relativeTo: .body)).foregroundStyle(tint).frame(minWidth: 64, alignment: .leading)
            Text(hotkey.summary).font(style.mono(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(1)
            Spacer(minLength: 0)
            if !hotkey.showsOnBar { Image(systemName: "eye.slash").font(style.system(.caption)).foregroundStyle(style.muted).accessibilityLabel("Not on the key bar") }
            if let chord = hotkey.chord { ChordTag(chord: chord) }
        }.frame(minHeight: style.pt(40)).contentShape(Rectangle())
    }
    private func templateRow(_ template: HotkeyTemplate) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(template.name).font(style.mono(13, bold: true, relativeTo: .body))
                Spacer(minLength: 4)
                Button("Install") { selection = .template(template.id); install(template) }.buttonStyle(DesktopButtonStyle(prominent: true, compact: true))
            }
            Text(template.summary).font(style.system(.caption)).foregroundStyle(style.muted)
            Text(template.hotkeys.compactMap { hotkey in hotkey.chord.map { "\($0.title) \(hotkey.label)" } }.joined(separator: "  "))
                .font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted)
            if let notice, selection == .template(template.id) { Text(notice).font(style.system(.caption)).foregroundStyle(style.accent) }
        }
        .padding(.vertical, 4).contentShape(Rectangle()).listRowBackground(rowBackground(.template(template.id)))
    }
    /// The hotkey menu or the hotkey help: its fixed shortcut, the person's extra ones, and learning another.
    private func shortcutRow(_ target: ShortcutTarget) -> some View {
        let extra = target == .menu ? store.shortcuts.paletteChords : store.shortcuts.helpChords
        return VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(target.title).font(style.mono(13, bold: true, relativeTo: .body))
                Spacer(minLength: 4)
                ChordTag(chord: target.fixed)
                ForEach(extra, id: \.self) { chord in
                    Button { if target == .menu { store.removePaletteChord(chord) } else { store.removeHelpChord(chord) } } label: { HStack(spacing: 3) { ChordTag(chord: chord); Image(systemName: "xmark.circle.fill").font(style.system(.caption)).foregroundStyle(style.muted) } }
                        .buttonStyle(.plain).accessibilityLabel("Remove shortcut \(chord.title)")
                }
            }
            if learning == target {
                Label("Press the key or chord that should open the \(target.noun). Esc cancels.", systemImage: "keyboard").font(style.system(.caption)).foregroundStyle(style.accent)
                lastEvent
            } else {
                Button("Add a shortcut for the \(target.noun)…", systemImage: "plus") { startLearning(target) }.buttonStyle(DesktopButtonStyle(compact: true))
            }
            if let learnProblem, learning == target { Label(learnProblem, systemImage: "exclamationmark.circle").font(style.system(.caption)).foregroundStyle(style.warning) }
        }
        .padding(.vertical, 4).contentShape(Rectangle()).listRowBackground(rowBackground(target.row))
    }
    private var testerRow: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("Key tester").font(style.mono(13, bold: true, relativeTo: .body))
                Spacer(minLength: 4)
                Button(testing ? "Stop" : "Test keys") { selection = .tester; testing.toggle() }.buttonStyle(DesktopButtonStyle(prominent: testing, compact: true))
            }
            if testing {
                Label("Press any key, or the Clicks button. Each press is shown here.", systemImage: "keyboard").font(style.system(.caption)).foregroundStyle(style.accent)
                lastEvent
            }
            Toggle("Show key events over the terminal", isOn: Binding(get: { keyboard.showKeyEvents }, set: { keyboard.setShowKeyEvents($0) }))
                .font(style.system(.caption))
        }
        .padding(.vertical, 4).contentShape(Rectangle()).listRowBackground(rowBackground(.tester))
    }
    @ViewBuilder private var lastEvent: some View {
        if let event = keyboard.events.last {
            VStack(alignment: .leading, spacing: 1) { ForEach(Array(event.lines.enumerated()), id: \.offset) { _, line in Text(line) } }
                .font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.text).monospacedDigit()
        } else {
            Text("No key yet").font(style.system(.caption)).foregroundStyle(style.muted)
        }
    }

    // MARK: Actions

    private func install(_ template: HotkeyTemplate) {
        let result = store.install(template)
        var text = result.summary
        if !result.paletteChordsAdded.isEmpty { text += " Tapping Control also opens the hotkey menu." }
        let taken = result.skipped.compactMap { skip -> String? in if case .shortcutTaken(let owner) = skip.reason { "\(skip.hotkey.label) (\(owner) has its shortcut)" } else { nil } }
        if !taken.isEmpty { text += " Skipped: " + taken.joined(separator: ", ") + "." }
        if result.skipped.contains(where: { $0.reason == .libraryFull }) { text += " The hotkey limit is \(HotkeyLibrary.maxHotkeys)." }
        notice = text
    }
    private func startLearning(_ target: ShortcutTarget) { selection = target.row; learnProblem = nil; learning = target }
    private func learnShortcut(_ chord: KeyChord) {
        guard let target = learning else { return }
        do {
            if target == .menu { try store.addPaletteChord(chord) } else { try store.addHelpChord(chord) }
            learning = nil; learnProblem = nil
        } catch { learnProblem = error.localizedDescription }
    }

    /// The keys that drive the list.
    private var listActions: [KeyAction] {
        let up: @MainActor () -> Void = { moveSelection(-1) }, down: @MainActor () -> Void = { moveSelection(1) }
        return [
            KeyAction(input: UIKeyCommand.inputUpArrow, title: "Previous row", run: up),
            KeyAction(input: UIKeyCommand.inputDownArrow, title: "Next row", run: down),
            KeyAction(input: "p", modifiers: .control, run: up), KeyAction(input: "n", modifiers: .control, run: down),
            KeyAction(input: "\t", modifiers: .shift, run: up), KeyAction(input: "\t", run: down),
            KeyAction(input: "\r", title: "Open or run the selected row", run: { activateSelection() }),
            KeyAction(input: "m", modifiers: .control, run: { activateSelection() }),
            KeyAction(input: "n", modifiers: .command, title: "New hotkey", run: { selection = .add; draft = HotkeyDraft() }),
            KeyAction(input: "\u{8}", modifiers: .command, title: "Delete the selected hotkey", run: { deleteSelected() }),
            KeyAction(input: UIKeyCommand.inputUpArrow, modifiers: .command, title: "Move the selected hotkey up", run: { moveSelected(-1) }),
            KeyAction(input: UIKeyCommand.inputDownArrow, modifiers: .command, title: "Move the selected hotkey down", run: { moveSelected(1) }),
            KeyAction(input: UIKeyCommand.inputEscape, title: "Done", run: { dismiss() }),
            KeyAction(input: ".", modifiers: .command, run: { dismiss() }), KeyAction(input: "w", modifiers: .command, run: { dismiss() })
        ]
    }
    private func moveSelection(_ step: Int) {
        let all = rows
        guard let index = all.firstIndex(of: selection) else { selection = all.first ?? .add; return }
        selection = all[(index + step + all.count) % all.count]
    }
    private func activateSelection() {
        switch selection {
        case .hotkey(let id): if let hotkey = store.custom.first(where: { $0.id == id }) { draft = HotkeyDraft(hotkey) }
        case .add: if store.custom.count < HotkeyLibrary.maxHotkeys { draft = HotkeyDraft() }
        case .template(let id): if let template = HotkeyTemplate.all.first(where: { $0.id == id }) { install(template) }
        case .menuShortcut: startLearning(.menu)
        case .helpShortcut: startLearning(.help)
        case .tester: testing.toggle()
        }
    }
    private func deleteSelected() {
        guard case .hotkey(let id) = selection, let index = store.custom.firstIndex(where: { $0.id == id }) else { return }
        store.remove(id: id)
        let remaining = store.custom
        selection = remaining.isEmpty ? .add : .hotkey(remaining[min(index, remaining.count - 1)].id)
    }
    private func moveSelected(_ step: Int) {
        guard case .hotkey(let id) = selection, let index = store.custom.firstIndex(where: { $0.id == id }) else { return }
        let target = index + step
        guard store.custom.indices.contains(target) else { return }
        store.move(fromOffsets: IndexSet(integer: index), toOffset: step < 0 ? target : target + 1)
    }
}

/// Adds or edits one hotkey: a name, a sequence of steps (text or a special key), a shortcut and whether the key bar shows it.
///
/// From the keyboard: Tab and Shift-Tab move between the name and the text steps, Return saves, Esc (or ⌘.) cancels, ⌘T adds a text
/// step, ⌘R records a key step by pressing it, ⌘L learns the shortcut by pressing it.
struct HotkeyForm: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.dismiss) private var dismiss
    let store: HotkeyStore
    let keyboard: KeyboardPrefs
    @State var draft: HotkeyDraft
    @State private var failure: String?
    @State private var focus: Field?
    @State private var mode: Mode = .none
    @State private var learnProblem: String?

    enum Field: Hashable { case name, step(UUID) }
    /// What a press of the keyboard means right now: nothing, a shortcut to learn, or a key to record as a step.
    enum Mode { case none, shortcut, recordKey }

    init(store: HotkeyStore, keyboard: KeyboardPrefs, draft: HotkeyDraft) {
        self.store = store; self.keyboard = keyboard; _draft = State(initialValue: draft)
    }

    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: draft.isNew ? "NEW HOTKEY" : "EDIT HOTKEY") {
                Button("Cancel") { dismiss() }
                Button("Save") { save() }.disabled(draft.problem != nil)
            }
            List {
                Section("Name on the key bar") {
                    field(.name, text: $draft.label, placeholder: "e.g. Clear", label: "Name", maxLength: Hotkey.maxLabelLength)
                        .listRowBackground(style.background)
                }
                Section("Steps, in order") {
                    ForEach($draft.steps) { $step in stepRow($step) }
                        .onDelete { draft.steps.remove(atOffsets: $0) }
                        .onMove { draft.steps.move(fromOffsets: $0, toOffset: $1) }
                        .listRowBackground(style.background)
                    HStack(spacing: 16) {
                        Button("Add text", systemImage: "textformat") { addTextStep() }
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
                        Button(mode == .recordKey ? "Stop" : "Press a key", systemImage: "keyboard") { mode = mode == .recordKey ? .none : .recordKey; learnProblem = nil }
                        Spacer(minLength: 0)
                    }.disabled(draft.steps.count >= Hotkey.maxSteps).listRowBackground(style.background)
                    if mode == .recordKey {
                        Label("Press the key to add as a step (Esc is recorded too). ⌘. stops.", systemImage: "keyboard").font(style.system(.caption)).foregroundStyle(style.accent)
                            .listRowBackground(style.background)
                    }
                }
                Section {
                    HStack(spacing: 8) {
                        if let chord = draft.chord { ChordTag(chord: chord) } else { Text("None").foregroundStyle(style.muted) }
                        Spacer(minLength: 4)
                        Button(mode == .shortcut ? "Stop" : "Learn key", systemImage: "keyboard") { mode = mode == .shortcut ? .none : .shortcut; learnProblem = nil }
                        if draft.chord != nil { Button("Clear", systemImage: "xmark.circle") { draft.chord = nil }.labelStyle(.iconOnly) }
                    }.frame(minHeight: style.pt(40))
                    if mode == .shortcut {
                        Label("Press the key or chord. A lone modifier (Control, say) is a tap. Esc cancels.", systemImage: "keyboard").font(style.system(.caption)).foregroundStyle(style.accent)
                        if let event = keyboard.events.last {
                            Text(event.summary).font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                        }
                    }
                    if let learnProblem { Label(learnProblem, systemImage: "exclamationmark.circle").font(style.system(.caption)).foregroundStyle(style.warning) }
                    Toggle("Show on the key bar", isOn: $draft.showsOnBar).frame(minHeight: style.pt(40))
                } header: { Text("Shortcut") } footer: {
                    Text("Runs this hotkey from the keyboard without opening the menu. Use ⌘, Ctrl or Alt with a letter, or a function key.").font(style.system(.caption))
                }.listRowBackground(style.background)
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
        // Keys work even when no field has the keyboard (after a tap on a button, say).
        .background { KeyCommandHost(active: focus == nil && mode == .none, actions: formActions) }
        .background {
            KeyLearnView(active: mode != .none, onEvent: { keyboard.events.record($0) },
                         onChord: mode == .none ? nil : { learned($0) }, escapeCancels: mode == .shortcut,
                         onCancel: { if mode == .shortcut { mode = .none; focus = .name } })
        }
        .onAppear { if focus == nil { focus = .name } }
        .presentationDetents([.large]).presentationCornerRadius(8)
    }
    /// The first thing a new form says is not a complaint.
    private var showsProblem: Bool { !(draft.isNew && draft.label.isEmpty && draft.steps.isEmpty) }

    // MARK: Fields

    private var order: [Field] { [.name] + draft.steps.filter { $0.kind == .text }.map { Field.step($0.id) } }
    private func field(_ id: Field, text: Binding<String>, placeholder: String, label: String, maxLength: Int? = nil) -> some View {
        NavTextField(text: maxLength.map { limit in
            Binding(get: { text.wrappedValue }, set: { text.wrappedValue = String($0.prefix(limit)) })
        } ?? text, placeholder: placeholder, label: label, isFocused: focus == id,
                     onFocus: { if focus != id { focus = id } }, onBlur: { if focus == id { focus = nil } },
                     actions: formActions, onReturn: { returned() })
            .frame(minHeight: style.pt(36))
    }
    @ViewBuilder private func stepRow(_ step: Binding<HotkeyDraft.Step>) -> some View {
        HStack(spacing: 8) {
            Image(systemName: step.wrappedValue.kind == .text ? "textformat" : "command").foregroundStyle(style.muted).frame(width: 20)
            if step.wrappedValue.kind == .text {
                field(.step(step.wrappedValue.id), text: step.text, placeholder: "Text to type", label: "Text step")
            } else {
                Picker("Key", selection: step.key) { ForEach(TerminalKey.choices, id: \.self) { Text($0.title).tag($0) } }.pickerStyle(.menu).labelsHidden()
                Spacer(minLength: 0)
            }
        }.frame(minHeight: style.pt(40))
    }
    private func addTextStep() {
        let step = HotkeyDraft.Step(kind: .text)
        draft.steps.append(step)
        focus = .step(step.id)
    }

    // MARK: Keys

    private var formActions: [KeyAction] {
        [
            KeyAction(input: "\t", title: "Next field", run: { moveFocus(1) }),
            KeyAction(input: "\t", modifiers: .shift, title: "Previous field", run: { moveFocus(-1) }),
            KeyAction(input: UIKeyCommand.inputEscape, title: "Cancel", run: { cancel() }),
            KeyAction(input: ".", modifiers: .command, run: { cancel() }),
            KeyAction(input: "s", modifiers: .command, title: "Save", run: { if draft.problem == nil { save() } }),
            KeyAction(input: "\r", modifiers: .command, run: { if draft.problem == nil { save() } }),
            KeyAction(input: "l", modifiers: .command, title: "Learn the shortcut", run: { mode = .shortcut; learnProblem = nil }),
            KeyAction(input: "r", modifiers: .command, title: "Record a key as a step", run: { mode = .recordKey; learnProblem = nil }),
            KeyAction(input: "t", modifiers: .command, title: "Add a text step", run: { addTextStep() })
        ]
    }
    /// Escape or ⌘.: leaves a learning mode first, and only then the form.
    private func cancel() {
        if mode != .none { mode = .none; focus = .name } else { dismiss() }
    }
    private func moveFocus(_ step: Int) {
        let fields = order
        guard let current = focus, let index = fields.firstIndex(of: current) else { focus = fields.first; return }
        focus = fields[(index + step + fields.count) % fields.count]
    }
    /// Return saves; if the hotkey is not complete yet it moves on to the next field instead.
    private func returned() {
        if draft.problem == nil { save() } else { moveFocus(1) }
    }

    /// A press while learning a shortcut or recording a key step.
    private func learned(_ chord: KeyChord) {
        switch mode {
        case .none:
            break
        case .shortcut:
            do {
                try chord.validate()
                if let owner = store.custom.first(where: { $0.id != draft.id && $0.chord == chord }) { throw HotkeyError.chordInUse(owner.label) }
                if store.shortcuts.paletteChords.contains(chord) { throw HotkeyError.chordInUse("the hotkey menu") }
                if store.shortcuts.helpChords.contains(chord) { throw HotkeyError.chordInUse("the hotkey help") }
                draft.chord = chord; learnProblem = nil; mode = .none; focus = .name
            } catch { learnProblem = error.localizedDescription }
        case .recordKey:
            if chord == KeyChord(keyCode: 0x37, modifiers: .command) { mode = .none; focus = .name; return }
            if let key = TerminalKey(chord: chord) {
                if draft.steps.count < Hotkey.maxSteps { draft.steps.append(.init(kind: .key, key: key)) }
                learnProblem = nil
            } else {
                learnProblem = "\(chord.title) is not a key a terminal receives. Pick it with Add key."
            }
        }
    }
    private func save() {
        let hotkey = draft.hotkey
        do {
            if draft.isNew { try store.add(hotkey) } else { try store.update(hotkey) }
            dismiss()
        } catch { failure = error.localizedDescription }
    }
}
