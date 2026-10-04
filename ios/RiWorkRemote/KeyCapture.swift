import SwiftUI
import UIKit
import Observation
import RiWorkCore

/// Menlo metrics for the terminal grid, measured the same way the text is drawn.
enum TerminalFont {
    static func uiFont(size: Double) -> UIFont { UIFont(name: "Menlo-Regular", size: size) ?? UIFont.monospacedSystemFont(ofSize: size, weight: .regular) }
    /// The face for a style, in the same Menlo as `uiFont` (every Menlo face advances by the same amount, so attributes never move a cell).
    static func uiFont(size: Double, bold: Bool, italic: Bool) -> UIFont {
        let name = switch (bold, italic) { case (false, false): "Menlo-Regular"; case (true, false): "Menlo-Bold"; case (false, true): "Menlo-Italic"; case (true, true): "Menlo-BoldItalic" }
        return UIFont(name: name, size: size) ?? uiFont(size: size)
    }
    /// Device pixels per point, for rounding the line height. Set once by the app at launch (the font cache below can be asked from any
    /// thread, so it cannot ask the screen); 3 is every iPhone since the 6 Plus except the SE.
    nonisolated(unsafe) static var pixelsPerPoint = 3.0
    private static let lock = NSLock()
    private struct CellKey: Hashable { let size: Double, scale: Double }
    nonisolated(unsafe) private static var cells: [CellKey: (width: Double, height: Double)] = [:]
    /// The size of one grid cell. Measured once per size: the terminal asks for it on every line it draws.
    ///
    /// The height is the font's line height rounded to a whole device pixel. Every row of the terminal is then a whole number of pixels
    /// tall and sits on the pixel grid, so text stays crisp at any scroll position and the rows tile without a seam; the desktop
    /// grid (rows that fit) is worked out from the same number, so the two agree to the pixel.
    static func cell(size: Double) -> (width: Double, height: Double) {
        lock.lock()
        let key = CellKey(size: size, scale: max(1, pixelsPerPoint))
        if let known = cells[key] { lock.unlock(); return known }
        lock.unlock()
        let font = uiFont(size: size)
        let height = max(1, (Double(font.lineHeight) * key.scale).rounded() / key.scale)
        let measured: (width: Double, height: Double) = (Double(("M" as NSString).size(withAttributes: [.font: font]).width), height)
        lock.lock()
        if cells.count > 256 { cells.removeAll() }
        cells[key] = measured
        lock.unlock()
        return measured
    }
}

/// A view that becomes first responder to bring up the keyboard, and turns everything typed into `KeyItem`s.
/// It draws nothing: the terminal's own screen shows what the shell echoes.
///
/// It also owns the keyboard shortcuts: ⌘K opens the hotkey menu (⌘, the hotkey settings, ⌘/ the hotkey help), a hotkey with a shortcut
/// runs on it, and while the menu is open every key goes to the menu instead of the shell. The help is not modal: it lists the
/// shortcuts and every key and chord keeps working under it.
@MainActor final class KeyCaptureView: UIView, UITextInput {
    /// Returns false when the input was refused (buffer full).
    var onItems: (([KeyItem]) -> Bool)?
    var onActiveChange: ((Bool) -> Void)?
    /// The person hid the keyboard with the bar's Hide key (not a sheet taking the keyboard, not the view going away).
    var onUserHide: (() -> Void)?
    private(set) var mapper = KeyMapper() { didSet { bar.setLatches(control: mapper.control, alt: mapper.alt, shift: mapper.shift) } }
    /// The modifiers a held bar key started with: every repeat of it keeps them (holding Ctrl+→ moves word after word).
    private var repeatModifiers: ChordModifiers = []
    /// Hardware presses used here rather than by the text system, so their release is not passed on either.
    private var usedPresses: Set<Int> = []
    let bar = KeyBarView()
    /// All the hotkeys the person added; the built-in ones are always there. The bar shows those that want a button.
    var hotkeys: [Hotkey] = [] { didSet { guard hotkeys != oldValue else { return }; bar.hotkeys = hotkeys.filter(\.showsOnBar); refreshShortcuts() } }
    /// The extra shortcuts that open the hotkey menu.
    var shortcutSettings = ShortcutSettings() { didSet { if shortcutSettings != oldValue { refreshShortcuts() } } }
    private(set) var shortcuts = ShortcutMap(hotkeys: [])
    /// The hotkey menu, drawn by SwiftUI and driven from here.
    var palette = PaletteController() { didSet { palette.onOutcome = { [weak self] in self?.paletteOutcome($0) } } }
    /// The hotkey help (⌘/), drawn by SwiftUI. It never takes the keyboard from this view.
    var help = HelpController() { didSet { help.onFire = { [weak self] in self?.helpFire($0) } } }
    var onEditHotkeys: (() -> Void)?
    var onNewHotkey: (() -> Void)?
    var onEditHotkey: ((Hotkey) -> Void)?
    /// Every key event that reaches the view, for the readout.
    var onKeyEvent: ((KeyEventRecord) -> Void)?
    var isEnabled = true { didSet { if !isEnabled, isFirstResponder { _ = resignFirstResponder() } } }
    private var tapDetector = ModifierTapDetector()

    // Text input traits: every one of these would change what the shell receives.
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var smartInsertDeleteType: UITextSmartInsertDeleteType = .no
    var inlinePredictionType: UITextInlinePredictionType = .no
    var mathExpressionCompletionType: UITextMathExpressionCompletionType = .no
    var writingToolsBehavior: UIWritingToolsBehavior = .none
    var keyboardType: UIKeyboardType = .asciiCapable
    var keyboardAppearance: UIKeyboardAppearance = .default
    var returnKeyType: UIReturnKeyType = .default
    var enablesReturnKeyAutomatically = false
    var isSecureTextEntry = false

    override init(frame: CGRect) {
        super.init(frame: frame)
        isAccessibilityElement = false
        bar.onAction = { [weak self] in self?.barAction($0) }
        palette.onOutcome = { [weak self] in self?.paletteOutcome($0) }
        help.onFire = { [weak self] in self?.helpFire($0) }
    }
    required init?(coder: NSCoder) { fatalError("KeyCaptureView is created in code") }

    override var canBecomeFirstResponder: Bool { isEnabled }
    override var inputAccessoryView: UIView? { bar }
    /// Replaces the software keyboard. Tests set an empty view: that is how iOS lays out a hardware keyboard (the bar alone
    /// at the bottom edge), which a simulator cannot attach.
    var inputViewStandIn: UIView?
    override var inputView: UIView? { inputViewStandIn }
    override func becomeFirstResponder() -> Bool {
        let became = super.becomeFirstResponder()
        if became { onActiveChange?(true) }
        return became
    }
    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { mapper.disarmModifiers(); palette.close(); help.close(); tapDetector.reset(); onActiveChange?(false) }
        return resigned
    }

    // MARK: UIKeyInput
    /// Always true, or the delete key stops reporting once the (nonexistent) text is empty.
    var hasText: Bool { true }
    func insertText(_ text: String) {
        if palette.isOpen { paletteText(text); return }
        emit(mapper.insert(text))
    }
    func deleteBackward() {
        if palette.isOpen { palette.apply(.backspace); return }
        emit(mapper.deleteBackward())
    }
    private func emit(_ items: [KeyItem]) {
        guard !items.isEmpty else { return }
        _ = onItems?(items)
    }

    // MARK: UITextInput
    // The software keyboard repeats a held delete key only for a `UITextInput` with text before the caret: a bare `UIKeyInput`
    // gets one `deleteBackward` per press. So the view shows the keyboard a document of one space with the caret after it, and
    // nothing ever changes it. Every delete, held or not, still arrives as one `deleteBackward` (the keyboard selects the space
    // first; that selection is ignored), and text still arrives through `insertText`.
    private final class Position: UITextPosition {
        let offset: Int
        init(_ offset: Int) { self.offset = offset }
    }
    private final class Range: UITextRange {
        let from: Int, to: Int
        init(_ a: Int, _ b: Int) { from = min(a, b); to = max(a, b) }
        override var start: UITextPosition { Position(from) }
        override var end: UITextPosition { Position(to) }
        override var isEmpty: Bool { from == to }
    }
    private static let document = " "
    private static let length = (document as NSString).length
    private static func offset(_ position: UITextPosition) -> Int { min(max((position as? Position)?.offset ?? length, 0), length) }

    func text(in range: UITextRange) -> String? {
        let from = Self.offset(range.start), to = Self.offset(range.end)
        return (Self.document as NSString).substring(with: NSRange(location: from, length: to - from))
    }
    /// Only the system's own edits come here (autocorrection and the rest are off): new text is typed text, nothing is replaced.
    func replace(_ range: UITextRange, withText text: String) { if !text.isEmpty { insertText(text) } }
    var selectedTextRange: UITextRange? { get { Range(Self.length, Self.length) } set {} }
    // No marked text: what is typed goes to the shell as it is typed.
    var markedTextRange: UITextRange? { nil }
    var markedTextStyle: [NSAttributedString.Key: Any]? { get { nil } set {} }
    func setMarkedText(_ markedText: String?, selectedRange: NSRange) {}
    func unmarkText() {}
    var beginningOfDocument: UITextPosition { Position(0) }
    var endOfDocument: UITextPosition { Position(Self.length) }
    func textRange(from fromPosition: UITextPosition, to toPosition: UITextPosition) -> UITextRange? { Range(Self.offset(fromPosition), Self.offset(toPosition)) }
    func position(from position: UITextPosition, offset: Int) -> UITextPosition? {
        let moved = Self.offset(position) + offset
        return (0...Self.length).contains(moved) ? Position(moved) : nil
    }
    func position(from position: UITextPosition, in direction: UITextLayoutDirection, offset: Int) -> UITextPosition? {
        self.position(from: position, offset: direction == .left || direction == .up ? -offset : offset)
    }
    func compare(_ position: UITextPosition, to other: UITextPosition) -> ComparisonResult {
        let a = Self.offset(position), b = Self.offset(other)
        return a < b ? .orderedAscending : a > b ? .orderedDescending : .orderedSame
    }
    func offset(from: UITextPosition, to toPosition: UITextPosition) -> Int { Self.offset(toPosition) - Self.offset(from) }
    weak var inputDelegate: UITextInputDelegate?
    lazy var tokenizer: UITextInputTokenizer = UITextInputStringTokenizer(textInput: self)
    func position(within range: UITextRange, farthestIn direction: UITextLayoutDirection) -> UITextPosition? { direction == .left || direction == .up ? range.start : range.end }
    func characterRange(byExtending position: UITextPosition, in direction: UITextLayoutDirection) -> UITextRange? { Range(Self.offset(position), Self.offset(position)) }
    func baseWritingDirection(for position: UITextPosition, in direction: UITextStorageDirection) -> NSWritingDirection { .leftToRight }
    func setBaseWritingDirection(_ writingDirection: NSWritingDirection, for range: UITextRange) {}
    // Nothing is drawn: no caret, no selection, no rectangles for the system to put a menu or a loupe on.
    func firstRect(for range: UITextRange) -> CGRect { .zero }
    func caretRect(for position: UITextPosition) -> CGRect { .zero }
    func selectionRects(for range: UITextRange) -> [UITextSelectionRect] { [] }
    func closestPosition(to point: CGPoint) -> UITextPosition? { endOfDocument }
    func closestPosition(to point: CGPoint, within range: UITextRange) -> UITextPosition? { range.end }
    func characterRange(at point: CGPoint) -> UITextRange? { nil }

    // MARK: Paste
    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        action == #selector(paste(_:)) ? UIPasteboard.general.hasStrings : super.canPerformAction(action, withSender: sender)
    }
    override func paste(_ sender: Any?) {
        guard let text = UIPasteboard.general.string else { return }
        if palette.isOpen { paletteText(text) } else { pasteText(text) }
    }
    /// Multi-line paste becomes text plus Enter items; other control characters are dropped.
    func pasteText(_ text: String) { emit(KeyMapper.items(for: text)) }

    // MARK: Key bar
    private func barAction(_ action: KeyBarView.Action) {
        if palette.isOpen {
            switch action {
            case .palette: palette.close(); return
            case .key(let key): if let move = Self.paletteMove(for: key) { movePalette(move); return }; palette.close()
            case .text(let symbol): palette.apply(.text(symbol)); return
            case .hide: break
            default: palette.close()
            }
        }
        switch action {
        case .key(let key) where bar.isRepeating: emit(KeyMapper.encode(key, modifiers: repeatModifiers))
        case .key(let key): repeatModifiers = mapper.armed; emit(mapper.press(key))
        case .control: mapper.tap(.control)
        case .alt: mapper.tap(.alt)
        case .shift: mapper.tap(.shift)
        case .latch(let modifier, let latch): mapper.setLatch(modifier, latch)
        case .text(let symbol): emit(mapper.insert(symbol))
        case .paste: paste(nil)
        case .hide: onUserHide?(); _ = resignFirstResponder()
        case .hotkey(let id): if let hotkey = (Hotkey.builtIn + hotkeys).first(where: { $0.id == id }) { emit(mapper.run(hotkey)) }
        case .editHotkeys: onEditHotkeys?()
        case .palette: togglePalette()
        case .help: toggleHelp()
        }
    }

    // MARK: Hotkey menu
    private enum PaletteMove { case input(HotkeyPalette.Input), close }
    /// How a terminal key moves the menu while it is open: the keys a keyboard lacks stand in for themselves, so a shortcut that
    /// sends ↑ moves up and one that sends Esc closes, and Ctrl-N, Ctrl-P and friends work as in a shell.
    private static func paletteMove(for key: TerminalKey) -> PaletteMove? {
        switch key {
        case .up, .backTab: return .input(.up)
        case .down, .tab: return .input(.down)
        case .pageUp: return .input(.pageUp)
        case .pageDown: return .input(.pageDown)
        case .enter: return .input(.activate)
        case .backspace: return .input(.backspace)
        case .escape: return .close
        case .control(let letter):
            switch letter {
            case "n", "j": return .input(.down)
            case "p", "k": return .input(.up)
            case "u": return .input(.clearQuery)
            case "m": return .input(.activate)   // Ctrl-M is Return
            case "c", "g": return .close
            default: return nil
            }
        default: return nil
        }
    }
    private func movePalette(_ move: PaletteMove) {
        switch move {
        case .input(let input): palette.apply(input)
        case .close: palette.close()
        }
    }
    func togglePalette() {
        if palette.isOpen { palette.close(); return }
        // A sheet or alert over the terminal: the menu would open out of sight.
        guard window?.rootViewController?.presentedViewController == nil else { return }
        mapper.disarmModifiers()
        help.close()
        palette.open(hotkeys: hotkeys)
    }
    /// ⌘/ or the key bar's ? button. The help does not take the keyboard, so there is nothing to give back when it closes.
    func toggleHelp() {
        if help.isOpen { help.close(); return }
        guard window?.rootViewController?.presentedViewController == nil else { return }
        palette.close()
        help.open(hotkeys: hotkeys, shortcuts: shortcutSettings)
    }
    /// A tap on a row of the help: the same as its chord. The help stays up.
    private func helpFire(_ hotkey: Hotkey) { emit(mapper.run(hotkey)) }
    /// Typed text goes to the filter; Return chooses, Tab moves on (a software keyboard has no key commands for them).
    private func paletteText(_ text: String) {
        var run = ""
        func flush() { if !run.isEmpty { palette.apply(.text(run)); run = "" } }
        for character in text {
            switch character {
            case "\n", "\r", "\r\n": flush(); palette.apply(.activate)
            case "\t": flush(); palette.apply(.down)
            default: run.append(character)
            }
        }
        flush()
    }
    private func paletteOutcome(_ outcome: HotkeyPalette.Outcome) {
        switch outcome {
        case .none: break
        case .hotkey(let hotkey): palette.close(); emit(mapper.run(hotkey))
        case .key(let key): palette.close(); emit(mapper.press(key))
        case .configure: palette.close(); onEditHotkeys?()
        case .newHotkey: palette.close(); onNewHotkey?()
        case .edit(let hotkey): palette.close(); onEditHotkey?(hotkey)
        }
    }

    // MARK: Shortcuts
    private func perform(_ action: ShortcutAction) {
        switch action {
        case .openPalette: togglePalette()
        case .openSettings: palette.close(); help.close(); onEditHotkeys?()
        case .openHelp: toggleHelp()
        case .hotkey(let hotkey):
            if palette.isOpen {
                if hotkey.items.count == 1, case .key(let key) = hotkey.items[0], let move = Self.paletteMove(for: key) { movePalette(move) }
                return
            }
            emit(mapper.run(hotkey))
        }
    }

    static func modifiers(_ flags: UIKeyModifierFlags) -> ChordModifiers {
        var out: ChordModifiers = []
        if flags.contains(.shift) { out.insert(.shift) }
        if flags.contains(.control) { out.insert(.control) }
        if flags.contains(.alternate) { out.insert(.alt) }
        if flags.contains(.command) { out.insert(.command) }
        return out
    }
    static func flags(_ modifiers: ChordModifiers) -> UIKeyModifierFlags {
        var out: UIKeyModifierFlags = []
        if modifiers.contains(.shift) { out.insert(.shift) }
        if modifiers.contains(.control) { out.insert(.control) }
        if modifiers.contains(.alt) { out.insert(.alternate) }
        if modifiers.contains(.command) { out.insert(.command) }
        return out
    }

    /// The `UIKeyCommand` input for keys that are not letters, digits or punctuation (those come from `HIDKey.character`).
    private static let specialInputs: [(code: Int, input: String)] = [
        (HIDKey.up, UIKeyCommand.inputUpArrow), (HIDKey.down, UIKeyCommand.inputDownArrow),
        (HIDKey.left, UIKeyCommand.inputLeftArrow), (HIDKey.right, UIKeyCommand.inputRightArrow),
        (HIDKey.escape, UIKeyCommand.inputEscape), (HIDKey.tab, "\t"), (HIDKey.returnKey, "\r"), (HIDKey.space, " "),
        (HIDKey.home, UIKeyCommand.inputHome), (HIDKey.end, UIKeyCommand.inputEnd),
        (HIDKey.pageUp, UIKeyCommand.inputPageUp), (HIDKey.pageDown, UIKeyCommand.inputPageDown),
        (HIDKey.f1, UIKeyCommand.f1), (HIDKey.f1 + 1, UIKeyCommand.f2), (HIDKey.f1 + 2, UIKeyCommand.f3),
        (HIDKey.f1 + 3, UIKeyCommand.f4), (HIDKey.f1 + 4, UIKeyCommand.f5), (HIDKey.f1 + 5, UIKeyCommand.f6),
        (HIDKey.f1 + 6, UIKeyCommand.f7), (HIDKey.f1 + 7, UIKeyCommand.f8), (HIDKey.f1 + 8, UIKeyCommand.f9),
        (HIDKey.f1 + 9, UIKeyCommand.f10), (HIDKey.f1 + 10, UIKeyCommand.f11), (HIDKey.f12, UIKeyCommand.f12)
    ]
    /// Every input a `UIKeyCommand` can name except Escape (which cancels learning): letters, digits, punctuation, and the special keys.
    static var learnableInputs: [String] {
        (0x04...0x38).compactMap { commandInput(forKeyCode: $0) } + specialInputs.filter { $0.code != HIDKey.escape }.map(\.input)
    }
    static func commandInput(forKeyCode code: Int) -> String? {
        specialInputs.first { $0.code == code }?.input ?? HIDKey.character(for: code)
    }
    static func keyCode(forCommandInput input: String) -> Int? {
        specialInputs.first { $0.input == input }?.code ?? HIDKey.code(forCharacter: input)
    }

    // MARK: Hardware keyboard
    private static let namedCommands: [(input: String, flags: UIKeyModifierFlags, key: TerminalKey)] = [
        (UIKeyCommand.inputUpArrow, [], .up), (UIKeyCommand.inputDownArrow, [], .down),
        (UIKeyCommand.inputLeftArrow, [], .left), (UIKeyCommand.inputRightArrow, [], .right),
        (UIKeyCommand.inputEscape, [], .escape), ("\t", [], .tab), ("\t", .shift, .backTab),
        (UIKeyCommand.inputHome, [], .home), (UIKeyCommand.inputEnd, [], .end),
        (UIKeyCommand.inputPageUp, [], .pageUp), (UIKeyCommand.inputPageDown, [], .pageDown),
        // Ctrl-[ is Escape in every terminal: the way to Esc on a keyboard whose Ctrl is the only modifier it has.
        ("[", .control, .escape)
    ]
    private static func command(_ input: String, _ flags: UIKeyModifierFlags, _ title: String? = nil, selector: Selector = #selector(keyCommandFired(_:))) -> UIKeyCommand {
        let command = UIKeyCommand(input: input, modifierFlags: flags, action: selector)
        // Tab, arrows and Escape otherwise move focus or do nothing in the terminal, and Command shortcuts are the system's first.
        command.wantsPriorityOverSystemBehavior = true
        if let title { command.discoverabilityTitle = title }
        return command
    }
    /// The keys with a terminal meaning of their own that a modifier changes (Ctrl+→, Alt+Return, Shift+Home …).
    private static let modifiableInputs: [(input: String, key: TerminalKey)] = [
        (UIKeyCommand.inputUpArrow, .up), (UIKeyCommand.inputDownArrow, .down), (UIKeyCommand.inputLeftArrow, .left), (UIKeyCommand.inputRightArrow, .right),
        (UIKeyCommand.inputHome, .home), (UIKeyCommand.inputEnd, .end), (UIKeyCommand.inputPageUp, .pageUp), (UIKeyCommand.inputPageDown, .pageDown),
        (UIKeyCommand.inputEscape, .escape), ("\t", .tab), ("\r", .enter)
    ]
    /// Shift, Ctrl and Alt in every combination, one at least. Command is the system's and the app's, never the terminal's.
    private static let terminalFlags: [UIKeyModifierFlags] = (1...7).map { bits in
        var flags: UIKeyModifierFlags = []
        if bits & 1 != 0 { flags.insert(.shift) }
        if bits & 2 != 0 { flags.insert(.control) }
        if bits & 4 != 0 { flags.insert(.alternate) }
        return flags
    }
    private lazy var commands: [UIKeyCommand] = {
        var list = Self.namedCommands.map { Self.command($0.input, $0.flags) }
        var seen = Set(Self.namedCommands.map { "\($0.input)|\($0.flags.rawValue)" })
        // The same keys with modifiers held. Their own command each, so that they repeat while held like the plain ones.
        for (input, _) in Self.modifiableInputs {
            for flags in Self.terminalFlags where seen.insert("\(input)|\(flags.rawValue)").inserted { list.append(Self.command(input, flags)) }
        }
        // Ctrl plus a letter, alone or with Shift and Alt (Option alone types the layout's own characters, as on the Mac).
        for flags in Self.terminalFlags where flags.contains(.control) {
            for value in 97...122 { list.append(Self.command(String(UnicodeScalar(UInt8(value))), flags)) }
        }
        list.append(Self.command("k", .command, "Hotkey menu"))
        list.append(Self.command(",", .command, "Hotkey settings"))
        list.append(Self.command("/", .command, "Hotkey help"))
        // Shift-Return is among the modified keys above: inside the menu it edits the chosen hotkey (outside it, it is Enter).
        return list
    }()
    /// Commands that mean something only while the hotkey menu is open, and are claimed only then: with the menu closed, ⌘N belongs
    /// to the app (a new terminal) and must reach the hosting controller.
    private lazy var paletteCommands: [UIKeyCommand] = [
        Self.command("n", .command, "New hotkey (in the hotkey menu)"),
        // The system's Cancel (a keyboard without Esc has no other way to put the menu away).
        Self.command(".", .command, "Close the hotkey menu"),
    ]
    /// The same for the help: ⌘. closes it (Esc does too, and is a command already). Claimed only while it is open.
    private lazy var helpCommands: [UIKeyCommand] = [Self.command(".", .command, "Close the hotkey help")]
    /// Commands for the shortcuts of hotkeys, the menu and the help: the keys a `UIKeyCommand` can name, with priority over the system.
    private var chordCommands: [UIKeyCommand] = []
    override var keyCommands: [UIKeyCommand]? { commands + (palette.isOpen ? paletteCommands : []) + (help.isOpen ? helpCommands : []) + chordCommands }

    private func refreshShortcuts() {
        shortcuts = ShortcutMap(hotkeys: hotkeys, settings: shortcutSettings)
        var seen = Set((commands + paletteCommands + helpCommands).map { "\($0.input ?? "")|\($0.modifierFlags.rawValue)" })
        chordCommands = shortcuts.chords.compactMap { chord in
            guard !chord.isTap, let input = Self.commandInput(forKeyCode: chord.keyCode) else { return nil }
            let flags = Self.flags(chord.modifiers)
            guard seen.insert("\(input)|\(flags.rawValue)").inserted else { return nil }
            return Self.command(input, flags)
        }
    }

    @objc func keyCommandFired(_ command: UIKeyCommand) {
        guard let input = command.input else { return }
        // A key command may take the key without a press reaching us: a Ctrl held for it was used, not tapped.
        tapDetector.spoil()
        let flags = command.modifierFlags
        let code = Self.keyCode(forCommandInput: input)
        onKeyEvent?(KeyEventRecord(phase: .command, keyCode: code, modifiers: Self.modifiers(flags), rawModifiers: Int(flags.rawValue), characters: input, charactersIgnoringModifiers: input))
        // A shortcut of the person's comes before the built-in meaning of the same keys (Ctrl-E, say).
        if let code, let action = shortcuts.action(for: KeyChord(keyCode: code, modifiers: Self.modifiers(flags))) { perform(action); return }
        // The help closes on Esc and ⌘.; every other key (arrows included) is the shell's as ever.
        if help.isOpen, (input == UIKeyCommand.inputEscape && flags.isEmpty) || (input == "." && flags == .command) { help.close(); return }
        let named = Self.namedCommands.first { $0.input == input && $0.flags == flags }?.key
        let control: TerminalKey? = flags == .control ? input.unicodeScalars.first.flatMap { input.unicodeScalars.count == 1 ? TerminalKey.control(forLetter: Character($0)) : nil } : nil
        if palette.isOpen {
            if let key = named ?? control, let move = Self.paletteMove(for: key) { movePalette(move) }
            else if input == "\r", flags == .shift { palette.apply(.editSelected) }
            else if input == "n", flags == .command { palette.apply(.newHotkey) }
            else if input == ".", flags == .command { palette.close() }
            return
        }
        guard !flags.contains(.command) else { return }
        let held = Self.modifiers(flags)
        if let key = Self.modifiableInputs.first(where: { $0.input == input })?.key { emit(mapper.press(key, modifiers: held)); return }
        // Ctrl with a letter or `[`, and whatever Shift and Alt are held with it. Armed bar modifiers add to them.
        if flags.contains(.control), input.count == 1, let character = input.first { emit(mapper.press(character, modifiers: held)) }
    }

    // MARK: Key presses
    // Every physical key reaches here first, so the readout sees them all and shortcuts that no `UIKeyCommand` can name (function
    // keys, a lone modifier such as the Clicks button) still work. Whatever is not used is passed on to the text system.

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let rest = presses.filter { !consume($0, .down) }
        if !rest.isEmpty { super.pressesBegan(Set(rest), with: event) }
    }
    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let rest = presses.filter { !consume($0, .up) }
        if !rest.isEmpty { super.pressesEnded(Set(rest), with: event) }
    }
    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        tapDetector.reset()
        for press in presses { if let event = record(press, .cancelled) { _ = handle(event) } }
        super.pressesCancelled(presses, with: event)
    }
    private func record(_ press: UIPress, _ phase: KeyEventRecord.Phase) -> KeyEventRecord? {
        guard let key = press.key else { return nil }
        return KeyEventRecord(phase: phase, keyCode: Int(key.keyCode.rawValue), modifiers: Self.modifiers(key.modifierFlags), rawModifiers: Int(key.modifierFlags.rawValue),
                              characters: key.characters, charactersIgnoringModifiers: key.charactersIgnoringModifiers)
    }
    /// True when the press was used and must not go on.
    private func consume(_ press: UIPress, _ phase: KeyEventRecord.Phase) -> Bool {
        guard let event = record(press, phase) else { return false }
        return handle(event)
    }
    /// One key event: reported to the readout, then used if it is a shortcut. True when it was used up.
    func handle(_ event: KeyEventRecord) -> Bool {
        onKeyEvent?(event)
        guard let code = event.keyCode else { return false }
        switch event.phase {
        case .down:
            tapDetector.keyDown(code)
            // A modifier going down is never used up: it is the start of a chord, or of a tap.
            guard !HIDKey.isModifier(code) else { return false }
            guard let action = shortcuts.action(for: KeyChord(keyCode: code, modifiers: event.modifiers)) else { return deleteKey(code, event.modifiers) }
            // Keys a `UIKeyCommand` can name are handled by their command, once.
            guard Self.commandInput(forKeyCode: code) == nil else { return false }
            perform(action)
            return true
        case .up:
            if let tap = tapDetector.keyUp(code), let action = shortcuts.action(for: tap) { perform(action) }
            return usedPresses.remove(code) != nil
        case .cancelled, .command:
            if event.phase == .cancelled { usedPresses.remove(code) }
            return false
        }
    }
    /// Forward Delete, and Backspace with Ctrl or Alt held, which the text system would turn into a plain delete or nothing at all
    /// (the view's document is one space with the caret at its end). Matched by their key codes: no `UIKeyCommand` input names
    /// Backspace for certain. Plain Backspace stays with the text system, which repeats it while held.
    private func deleteKey(_ code: Int, _ modifiers: ChordModifiers) -> Bool {
        guard !palette.isOpen, !modifiers.contains(.command) else { return false }
        let key: TerminalKey
        switch code {
        case HIDKey.deleteForward: key = .delete
        case HIDKey.backspace where !modifiers.isDisjoint(with: [.control, .alt]): key = .backspace
        default: return false
        }
        usedPresses.insert(code)
        emit(mapper.press(key, modifiers: modifiers))
        return true
    }
}

/// Lets SwiftUI focus the capture view (tap on the terminal) and see whether the keyboard is up.
///
/// There are three ways the keyboard comes or goes, and they are kept apart:
/// - the person's own: a tap on the terminal or Show keyboard (`focus`), and Hide (`userDismiss`). Hiding is remembered for that
///   shell, so the terminal does not take the keyboard back by itself;
/// - the app's: a sheet takes the keyboard and gives it back (`suspendForModal` / `restoreAfterModal`), a closed shell hands over
///   to another one (`shellReplacedWithoutTap`);
/// - automatic: a shell becomes ready and the focus policy decides (`shellReady` / `autoFocus`).
@MainActor @Observable final class KeyFocus {
    var isActive = false
    @ObservationIgnored weak var view: KeyCaptureView?
    /// Which shell is on screen, set by the view that owns this.
    @ObservationIgnored var shellID: String?
    /// The setting, whether a hardware keyboard is attached, and whether the shell can take direct typing; asked at the moment of
    /// deciding, so a retry never works from old values.
    @ObservationIgnored var policy: () -> (setting: KeyboardFocusSetting, hardware: Bool, canType: Bool) = { (.never, false, false) }
    @ObservationIgnored private(set) var tracker = ShellFocusTracker()
    @ObservationIgnored private var restoreAfterModal = false
    @ObservationIgnored private var retry: Task<Void, Never>?

    // The person's.
    func focus() { tracker.userFocused(shellID); _ = view?.becomeFirstResponder() }
    func userDismiss() { noteUserHide(); _ = view?.resignFirstResponder() }
    /// The capture view's own Hide key resigns by itself; this only remembers that the person did it.
    func noteUserHide() { tracker.userDismissed(shellID) }

    // The app's.
    func dismiss() { _ = view?.resignFirstResponder() }
    /// A sheet is about to cover the terminal: put the keyboard away and remember to bring it back.
    func suspendForModal() {
        restoreAfterModal = view?.isFirstResponder ?? false
        _ = view?.resignFirstResponder()
    }
    /// The sheet is gone: the shell gets the keyboard back if it had it, or else as the policy says.
    func restoreAfterModalDismissal() {
        let restore = restoreAfterModal
        restoreAfterModal = false
        if restore, !tracker.isDismissed(shellID), let view, !Self.isObscured(view) { _ = view.becomeFirstResponder() }
        else { autoFocus() }
    }
    func shellReplacedWithoutTap() { tracker.shellReplacedWithoutTap(shellID); dismiss() }

    // Automatic.
    /// Feed the ready shell (nil when none is). A shell that has just become ready may take the keyboard.
    func shellReady(_ shell: String?) {
        if tracker.readiness(shell) { autoFocus() }
    }
    /// Takes the keyboard if the policy says so. A shell that is not ready (no live screen yet) never does. While a sheet is up it
    /// looks again every quarter second for ten seconds, to catch the moment the sheet goes.
    @discardableResult
    func autoFocus(retries: Int = 40) -> KeyboardFocusDecision {
        retry?.cancel()
        let current = policy()
        let context = KeyboardFocusContext(hardwareKeyboard: current.hardware, obscured: Self.isObscured(view),
                                           alreadyFocused: view?.isFirstResponder ?? false, dismissedHere: tracker.isDismissed(shellID),
                                           canType: current.canType && tracker.readyShell != nil && tracker.readyShell == shellID)
        let decision = KeyboardFocusPolicy.decide(setting: current.setting, context: context)
        if decision.shouldFocus {
            _ = view?.becomeFirstResponder()
        } else if decision == .skip(.obscured), retries > 0 {
            retry = Task { [weak self] in
                try? await Task.sleep(for: .milliseconds(250))
                guard !Task.isCancelled else { return }
                self?.autoFocus(retries: retries - 1)
            }
        }
        return decision
    }

    /// A sheet, alert or popover covers the terminal, or another text field has the keyboard: the terminal must not take it.
    static func isObscured(_ view: UIView?) -> Bool {
        guard let window = view?.window else { return true }
        if window.rootViewController?.presentedViewController != nil { return true }
        if let responder = firstResponder(in: window), responder !== view, responder is UITextInput { return true }
        return false
    }
    /// The view in `root`'s hierarchy that has the keyboard, if any.
    private static func firstResponder(in root: UIView) -> UIView? {
        if root.isFirstResponder { return root }
        for subview in root.subviews { if let found = firstResponder(in: subview) { return found } }
        return nil
    }
}

struct KeyCapture: UIViewRepresentable {
    let focus: KeyFocus
    var isEnabled: Bool
    var label: String
    /// Focus mode draws the bar as a pill when it is alone at the bottom edge (hardware keyboard).
    var hotkeys: [Hotkey] = []
    var shortcuts = ShortcutSettings()
    var palette: PaletteController?
    var help: HelpController?
    var onEditHotkeys: () -> Void = {}
    var onNewHotkey: () -> Void = {}
    var onEditHotkey: (Hotkey) -> Void = { _ in }
    var onKeyEvent: (KeyEventRecord) -> Void = { _ in }
    var onItems: ([KeyItem]) -> Bool

    func makeUIView(context: Context) -> KeyCaptureView {
        let view = KeyCaptureView()
        view.onActiveChange = { [focus] active in focus.isActive = active }
        view.onUserHide = { [focus] in focus.noteUserHide() }
        focus.view = view
        return view
    }
    func updateUIView(_ view: KeyCaptureView, context: Context) {
        view.onItems = onItems
        view.isEnabled = isEnabled
        view.accessibilityLabel = label
        // New colors reach the bar in place: the view, its focus and the keyboard are not rebuilt, so typing goes on.
        let previousScale = view.bar.style.scale
        view.bar.style = context.environment.desktopStyle
        // A bar that is on screen tells iOS its new height; the keyboard is asked to lay it out again.
        if view.bar.style.scale != previousScale, view.isFirstResponder { view.reloadInputViews() }
        if let palette, view.palette !== palette { view.palette = palette }
        if let help, view.help !== help { view.help = help }
        view.hotkeys = hotkeys
        view.shortcutSettings = shortcuts
        view.onEditHotkeys = onEditHotkeys
        view.onNewHotkey = onNewHotkey
        view.onEditHotkey = onEditHotkey
        view.onKeyEvent = onKeyEvent
        focus.view = view
    }
    static func dismantleUIView(_ view: KeyCaptureView, coordinator: ()) {
        // Resign first (reports "inactive"), then detach: the keyboard must not outlive the capture view.
        _ = view.resignFirstResponder()
        view.onItems = nil
        view.onActiveChange = nil
        view.onUserHide = nil
        view.onKeyEvent = nil
    }
}
