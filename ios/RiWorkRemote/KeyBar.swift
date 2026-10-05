import UIKit
import RiWorkCore

/// A row of keys that scrolls sideways. A finger that lands on a key and then drags scrolls the row instead of pressing the key.
private final class KeyScrollView: UIScrollView {
    override func touchesShouldCancel(in view: UIView) -> Bool { true }
}

/// The key row on the keyboard: special keys, Ctrl, Alt and Shift, symbols that are awkward on the iOS keyboard, and hotkeys.
/// It is the input accessory view, and always 44 pt tall, exactly where iOS puts it:
/// - above the software keyboard, or
/// - alone at the bottom edge of the screen when a hardware keyboard is attached (iOS hides the software keyboard then).
///
/// The row scrolls. Its ends are padded (`KeyBarGeometry`) so that, scrolled fully left or right, the first and the last key are
/// clear of the display's rounded corners. Hide stays put at the trailing end, with the dictation mic just before it, so both are always
/// within reach; everything else, including the "+" that opens the hotkey editor at the very end, is in the scrolling part.
@MainActor final class KeyBarView: UIInputView {
    enum Action: Hashable {
        case key(TerminalKey), control, alt, shift, text(String), paste, attach, hide, hotkey(String), editHotkeys, palette, help, dictate
        /// A choice from the paperclip key's menu (the key itself, `attach`, only opens the menu).
        case attachFrom(AttachmentChoice)
        /// VoiceOver's "Lock" or "Release" on a modifier key, where a double tap cannot be made.
        case latch(ChordModifiers, ModifierLatch)
    }
    private enum Role { case plain, hotkey, muted }

    /// The strip at the standard interface size: a 1 pt rule and the keys.
    static let height = CGFloat(KeyBarGeometry.height)
    /// The strip at the current interface size (`InterfaceScale`); the bar is exactly this tall wherever iOS puts it.
    var barHeight: CGFloat { CGFloat(KeyBarGeometry.height(scale: style.scale)) }
    /// A length of the bar at the current interface size, on whole points.
    private func unit(_ points: CGFloat) -> CGFloat { CGFloat(InterfaceScale.scaled(Double(points), by: style.scale)) }
    /// Symbols that are awkward to reach on the iOS keyboard, sent as text.
    static let symbols: [String] = ["|", "/", "\\", "~", "-", "_", "`", "*", "&", "$", ">", "<", "{", "}", "[", "]", ";", ":", "'", "\""]
    static let symbolNames: [String: String] = [
        "|": "Pipe", "/": "Slash", "\\": "Backslash", "~": "Tilde", "-": "Dash", "_": "Underscore", "`": "Backtick", "*": "Asterisk", "&": "Ampersand",
        "$": "Dollar", ">": "Greater than", "<": "Less than", "{": "Left brace", "}": "Right brace", "[": "Left bracket", "]": "Right bracket",
        ";": "Semicolon", ":": "Colon", "'": "Apostrophe", "\"": "Quote"
    ]
    /// Keys that keep repeating while held.
    private static let repeating: Set<TerminalKey> = [.left, .up, .down, .right, .backspace, .delete, .pageUp, .pageDown]

    var onAction: ((Action) -> Void)?
    private(set) var buttons: [Action: UIButton] = [:]
    /// The hotkeys the person added. They follow the built-in ones.
    var hotkeys: [Hotkey] = [] { didSet { if hotkeys != oldValue { rebuild() } } }
    var style = DesktopStyle.builtIn {
        didSet {
            guard style != oldValue else { return }
            // A new interface size changes every key's size and the bar's height, and Native draws other keys (symbols, another
            // face); colors only need a restyle.
            if style.scale != oldValue.scale || style.native != oldValue.native { applyScale() } else { restyle() }
        }
    }
    /// Where the bar is and the padding at the ends of the row that follows from it.
    private(set) var position = KeyBarPosition.aboveKeyboard
    private(set) var padding = KeyBarPadding.zero
    /// The scrolling row, for tests.
    var scrollView: UIScrollView { scroll }

    private let rule = UIView()
    /// Native on iOS 26: the row sits on a capsule of Liquid Glass instead of the panel color.
    private var glassView: UIVisualEffectView?
    private let row = UIView()
    private let scroll = KeyScrollView()
    private let stack = UIStackView()
    private let hideDivider = UIView()
    private var dividers: [UIView] = []
    private var roles: [Action: Role] = [:]
    private var stackLeading: NSLayoutConstraint!, hideTrailing: NSLayoutConstraint!
    private var hideWidth: NSLayoutConstraint!, hideMinWidth: NSLayoutConstraint?, dividerInsets: [NSLayoutConstraint] = [], stackTrailing: NSLayoutConstraint!
    private var latches: [Action: ModifierLatch] = [.control: .off, .alt: .off, .shift: .off]
    private var dictation = Dictation.idle, dictateWidth: NSLayoutConstraint?
    private var scrollBeforeMic: NSLayoutConstraint?, scrollBeforeHide: NSLayoutConstraint?
    private var repeatTask: Task<Void, Never>?
    private var didRepeat = false
    /// True while a held key sends itself again (not for its first press), so that it keeps the modifiers it started with.
    private(set) var isRepeating = false
    private var keyboardObservers: [any NSObjectProtocol] = []

    init() {
        // Style, frame and flexibility as before: a fixed 44 pt height is what keeps the terminal exactly above the bar (a bar that
        // changed height after it appeared is not accounted for by SwiftUI's keyboard avoidance), and the `.keyboard` style is what
        // has iOS put the bar on the bottom edge itself; `.default` lifts it about 17 pt off the edge.
        super.init(frame: CGRect(x: 0, y: 0, width: 320, height: Self.height), inputViewStyle: .keyboard)
        allowsSelfSizing = true
        autoresizingMask = .flexibleWidth
        for view in [rule, row, scroll, stack, hideDivider] { view.translatesAutoresizingMaskIntoConstraints = false }
        addSubview(rule)
        addSubview(row)
        row.addSubview(scroll)
        row.addSubview(hideDivider)
        scroll.addSubview(stack)
        scroll.showsHorizontalScrollIndicator = false
        scroll.showsVerticalScrollIndicator = false
        scroll.alwaysBounceHorizontal = true
        scroll.alwaysBounceVertical = false
        // A key must react the moment a finger lands on it (arrows repeat from touch-down), so touches are not held back.
        scroll.delaysContentTouches = false
        scroll.contentInsetAdjustmentBehavior = .never
        scroll.accessibilityIdentifier = "keybar.scroll"
        stack.axis = .horizontal
        stack.alignment = .fill
        stack.distribution = .fill
        stack.spacing = 0

        stackLeading = stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor)
        stackTrailing = stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -4)
        let hideDividerTop = hideDivider.topAnchor.constraint(equalTo: row.topAnchor, constant: 10)
        let hideDividerBottom = hideDivider.bottomAnchor.constraint(equalTo: row.bottomAnchor, constant: -10)
        dividerInsets = [hideDividerTop, hideDividerBottom]
        NSLayoutConstraint.activate([
            rule.topAnchor.constraint(equalTo: topAnchor), rule.leadingAnchor.constraint(equalTo: leadingAnchor),
            rule.trailingAnchor.constraint(equalTo: trailingAnchor), rule.heightAnchor.constraint(equalToConstant: 1),
            row.leadingAnchor.constraint(equalTo: leadingAnchor), trailingAnchor.constraint(equalTo: row.trailingAnchor),
            row.topAnchor.constraint(equalTo: topAnchor, constant: 1), bottomAnchor.constraint(equalTo: row.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: row.leadingAnchor), scroll.topAnchor.constraint(equalTo: row.topAnchor), scroll.bottomAnchor.constraint(equalTo: row.bottomAnchor),
            hideDividerTop, hideDividerBottom,
            hideDivider.widthAnchor.constraint(equalToConstant: 1),
            stackLeading, stackTrailing,
            stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor), stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor),
            stack.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor)
        ])
        let hide = makeButton(.hide, title: nil, symbol: "keyboard.chevron.compact.down", label: "Hide keyboard", role: .plain)
        hide.translatesAutoresizingMaskIntoConstraints = false
        row.addSubview(hide)
        hideTrailing = row.trailingAnchor.constraint(equalTo: hide.trailingAnchor)
        hideWidth = hide.widthAnchor.constraint(equalToConstant: 44)
        NSLayoutConstraint.activate([
            hideTrailing, hide.leadingAnchor.constraint(equalTo: hideDivider.trailingAnchor), hide.topAnchor.constraint(equalTo: row.topAnchor),
            hide.bottomAnchor.constraint(equalTo: row.bottomAnchor), hideWidth
        ])
        addDictateButton()
        rebuild()
        restyle()
        registerForTraitChanges([UITraitUserInterfaceStyle.self]) { (view: KeyBarView, _) in view.restyle() }
    }
    required init?(coder: NSCoder) { fatalError("KeyBarView is created in code") }
    override var intrinsicContentSize: CGSize { CGSize(width: UIView.noIntrinsicMetric, height: barHeight) }

    /// A new interface size: the bar's height, every key's font, insets and width, and the fixed pieces around them.
    private func applyScale() {
        hideWidth.constant = unit(44)
        for constraint in dividerInsets { constraint.constant = constraint === dividerInsets.first ? unit(10) : -unit(10) }
        stackTrailing.constant = -unit(4)
        if let hide = buttons[.hide] {
            hide.configuration?.image = UIImage(systemName: "keyboard.chevron.compact.down", withConfiguration: UIImage.SymbolConfiguration(pointSize: 14 * CGFloat(style.scale), weight: .regular))
            hideMinWidth?.constant = unit(40)
        }
        dictateWidth?.constant = unit(44)
        setDictation(dictation)
        rebuild()
        invalidateIntrinsicContentSize()
        setNeedsLayout()
    }

    // MARK: Keys

    /// Special keys, then the hotkey menu and help buttons and the built-in and added hotkeys, then symbols, then the hotkey editor's "+" at the very end.
    private func rebuild() {
        for view in stack.arrangedSubviews { stack.removeArrangedSubview(view); view.removeFromSuperview() }
        let hide = buttons[.hide], dictate = buttons[.dictate]
        buttons = [:]; roles = [:]; dividers = []
        if let hide { buttons[.hide] = hide; roles[.hide] = .plain }
        if let dictate { buttons[.dictate] = dictate; roles[.dictate] = .plain }
        // Native draws the named keys as the symbols macOS uses for them; the terminal look spells them out.
        func named(_ title: String, _ symbol: String) -> (String?, String?) { style.native ? (nil, symbol) : (title, nil) }
        func key(_ key: TerminalKey, _ face: (String?, String?), _ label: String) {
            add(.key(key), title: face.0, symbol: face.1, label: label, role: .plain)
        }
        key(.escape, named("Esc", "escape"), "Escape"); key(.tab, named("Tab", "arrow.right.to.line"), "Tab")
        let control = named("Ctrl", "control"), alt = named("Alt", "option"), shift = named("Shift", "shift")
        add(.control, title: control.0, symbol: control.1, label: "Control", role: .plain); add(.alt, title: alt.0, symbol: alt.1, label: "Alt", role: .plain)
        add(.shift, title: shift.0, symbol: shift.1, label: "Shift", role: .plain)
        key(.left, (nil, "arrow.left"), "Left arrow"); key(.up, (nil, "arrow.up"), "Up arrow"); key(.down, (nil, "arrow.down"), "Down arrow"); key(.right, (nil, "arrow.right"), "Right arrow")
        key(.backTab, named("⇧Tab", "arrow.left.to.line"), "Shift Tab"); key(.home, named("Home", "arrow.up.left"), "Home"); key(.end, named("End", "arrow.down.right"), "End")
        key(.pageUp, named("PgUp", "chevron.up.2"), "Page up"); key(.pageDown, named("PgDn", "chevron.down.2"), "Page down")
        key(.delete, named("Del", "delete.right"), "Delete"); key(.backspace, (nil, "delete.left"), "Backspace"); key(.enter, (nil, "return"), "Enter")
        add(.paste, title: nil, symbol: "doc.on.clipboard", label: "Paste", role: .plain)
        add(.attach, title: nil, symbol: "paperclip", label: "Send a photo or file", role: .plain)
        if let paperclip = buttons[.attach] { giveMenu(paperclip) }
        addDivider()
        // The hotkey menu first: it reaches every hotkey, shortcut and key from the keyboard (also ⌘K).
        add(.palette, title: nil, symbol: "command", label: "Hotkey menu", role: .hotkey)
        // And the hotkey help (⌘/): every hotkey with its shortcut, to look at before pressing one.
        let help = named("?", "questionmark.circle")
        add(.help, title: help.0, symbol: help.1, label: "Hotkey help", role: .hotkey)
        for hotkey in Hotkey.builtIn + hotkeys {
            add(.hotkey(hotkey.id), title: hotkey.label, symbol: nil, label: "Hotkey \(hotkey.label)", role: .hotkey)
        }
        addDivider()
        for symbol in Self.symbols { add(.text(symbol), title: symbol, symbol: nil, label: Self.symbolNames[symbol] ?? symbol, role: .plain, wide: false) }
        addDivider()
        add(.editHotkeys, title: nil, symbol: "plus", label: "Add or edit hotkeys", role: .muted)
        restyle()
    }
    private func add(_ action: Action, title: String?, symbol: String?, label: String, role: Role, wide: Bool = true) {
        let button = makeButton(action, title: title, symbol: symbol, label: label, role: role, wide: wide)
        stack.addArrangedSubview(button)
    }
    /// The paperclip opens a menu of where the photo or file comes from, from the key itself (above the keyboard, not over the
    /// screen); the keyboard stays up until a choice is made. In the look's casing, and with the camera only where there is one.
    private func giveMenu(_ paperclip: UIButton) {
        paperclip.menu = UIMenu(children: AttachmentChoice.available.map { choice in
            UIAction(title: style.cased(choice.title), image: UIImage(systemName: choice.symbol)) { [weak self] _ in self?.tapped(.attachFrom(choice)) }
        })
        paperclip.showsMenuAsPrimaryAction = true
        paperclip.preferredMenuElementOrder = .fixed
        paperclip.accessibilityHint = "Sends a photo or a file to the Mac and pastes its path"
    }
    private func addDivider() {
        let holder = UIView()
        let line = UIView()
        line.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(line)
        NSLayoutConstraint.activate([
            holder.widthAnchor.constraint(equalToConstant: unit(9)), line.widthAnchor.constraint(equalToConstant: 1), line.heightAnchor.constraint(equalToConstant: unit(20)),
            line.centerXAnchor.constraint(equalTo: holder.centerXAnchor), line.centerYAnchor.constraint(equalTo: holder.centerYAnchor)
        ])
        dividers.append(line)
        stack.addArrangedSubview(holder)
    }
    private func makeButton(_ action: Action, title: String?, symbol: String?, label: String, role: Role, wide: Bool = true) -> UIButton {
        let button = UIButton(type: .system)
        var configuration = UIButton.Configuration.plain()
        let inset: CGFloat = unit(wide ? 9 : 5)
        configuration.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: inset, bottom: 0, trailing: inset)
        if let title {
            let size: CGFloat = wide ? 13 : 16
            configuration.attributedTitle = AttributedString(title, attributes: AttributeContainer([.font: style.uiFace(size: size)]))
        }
        if let symbol { configuration.image = UIImage(systemName: symbol, withConfiguration: UIImage.SymbolConfiguration(pointSize: 14 * CGFloat(style.scale), weight: .regular)) }
        configuration.titleLineBreakMode = .byClipping
        button.configuration = configuration
        button.accessibilityLabel = label
        button.accessibilityIdentifier = "keybar.\(label)"
        button.setContentHuggingPriority(.required, for: .horizontal)
        button.setContentCompressionResistancePriority(.required, for: .horizontal)
        let minimumWidth = button.widthAnchor.constraint(greaterThanOrEqualToConstant: unit(wide ? 40 : 30))
        minimumWidth.isActive = true
        if action == .hide { hideMinWidth = minimumWidth }
        button.addAction(UIAction { [weak self] _ in self?.tapped(action) }, for: .touchUpInside)
        if case .key(let key) = action, Self.repeating.contains(key) {
            button.addAction(UIAction { [weak self] _ in self?.beginRepeat(action) }, for: .touchDown)
            button.addAction(UIAction { [weak self] _ in self?.endRepeat() }, for: [.touchUpInside, .touchUpOutside, .touchCancel, .touchDragExit])
        }
        buttons[action] = button
        roles[action] = role
        return button
    }

    /// Ctrl, Alt and Shift are sticky. Armed for the next key, a modifier is tinted (the accent on the highlight color); locked,
    /// it is filled with the accent. The same in the terminal look and under Native.
    func setLatches(control: ModifierLatch, alt: ModifierLatch, shift: ModifierLatch) {
        latches = [.control: control, .alt: alt, .shift: shift]
        for (action, modifier) in [(Action.control, ChordModifiers.control), (.alt, .alt), (.shift, .shift)] {
            guard let button = buttons[action] else { continue }
            let state = latch(action)
            button.configuration?.baseForegroundColor = state == .locked ? style.backgroundUI : state == .once ? style.accentUI : style.textUI
            button.configuration?.background.backgroundColor = state == .locked ? style.accentUI : state == .once ? style.activeUI : .clear
            // Inset from the bar's edges, so that on glass the highlight stays inside the capsule; round under Native, square-ish in the terminal look.
            button.configuration?.background.backgroundInsets = NSDirectionalEdgeInsets(top: unit(6), leading: unit(1), bottom: unit(6), trailing: unit(1))
            button.configuration?.background.cornerRadius = style.native ? unit(15) : unit(5)
            button.isSelected = state != .off
            button.accessibilityValue = state == .locked ? "locked" : state == .once ? "armed" : "not armed"
            button.accessibilityTraits = state == .off ? .button : [.button, .selected]
            button.accessibilityHint = "Applies to the next key. Double-tap to lock."
            let lock = state != .locked
            button.accessibilityCustomActions = [UIAccessibilityCustomAction(name: lock ? "Lock" : "Release") { [weak self] _ in
                self?.onAction?(.latch(modifier, lock ? .locked : .off)); return true
            }]
        }
    }
    func latch(_ action: Action) -> ModifierLatch { latches[action] ?? .off }
    /// Programmatic press, used by the buttons and by tests.
    func tapped(_ action: Action) {
        // A key that already repeated while held is not sent once more on release.
        if didRepeat { didRepeat = false; return }
        onAction?(action)
    }
    private func beginRepeat(_ action: Action) {
        didRepeat = false
        repeatTask?.cancel()
        repeatTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(400))
            while !Task.isCancelled, let self {
                self.isRepeating = self.didRepeat
                self.didRepeat = true
                self.onAction?(action)
                self.isRepeating = false
                try? await Task.sleep(for: .milliseconds(70))
            }
        }
    }
    private func endRepeat() { repeatTask?.cancel(); repeatTask = nil; isRepeating = false }

    // MARK: Dictation

    /// The mic's state: listening is drawn like an armed modifier, getting ready and settling are dimmed.
    enum Dictation { case idle, busy, listening }
    /// The mic sits between the scrolling row and Hide, fixed like Hide, while the desktop's mic setting is on (`DesktopStyle.mic`).
    /// Off, it is hidden and the scrolling row reaches the divider before Hide instead: one of the two trailing constraints is active.
    private func addDictateButton() {
        let mic = makeButton(.dictate, title: nil, symbol: "mic", label: "Dictate", role: .plain)
        mic.translatesAutoresizingMaskIntoConstraints = false
        row.addSubview(mic)
        dictateWidth = mic.widthAnchor.constraint(equalToConstant: 44)
        scrollBeforeMic = scroll.trailingAnchor.constraint(equalTo: mic.leadingAnchor)
        scrollBeforeHide = scroll.trailingAnchor.constraint(equalTo: hideDivider.leadingAnchor)
        NSLayoutConstraint.activate([
            mic.trailingAnchor.constraint(equalTo: hideDivider.leadingAnchor),
            mic.topAnchor.constraint(equalTo: row.topAnchor), mic.bottomAnchor.constraint(equalTo: row.bottomAnchor), dictateWidth!
        ])
        applyMic()
    }
    /// Shows or takes away the mic as the desktop's setting says, in place: the row is not rebuilt.
    private func applyMic() {
        guard let mic = buttons[.dictate], let scrollBeforeMic, let scrollBeforeHide else { return }
        let shown = style.mic
        guard mic.isHidden == shown || scrollBeforeMic.isActive != shown || scrollBeforeHide.isActive == shown else { return }
        mic.isHidden = !shown
        // Deactivate first, so the two never hold at once.
        (shown ? scrollBeforeHide : scrollBeforeMic).isActive = false
        (shown ? scrollBeforeMic : scrollBeforeHide).isActive = true
        setNeedsLayout()
    }
    /// The mic is in the bar (the desktop's mic setting is on).
    var showsMic: Bool { buttons[.dictate].map { !$0.isHidden } ?? false }
    func setDictation(_ state: Dictation) {
        dictation = state
        guard let mic = buttons[.dictate] else { return }
        let symbol = state == .idle ? "mic" : "mic.fill"
        mic.configuration?.image = UIImage(systemName: symbol, withConfiguration: UIImage.SymbolConfiguration(pointSize: 14 * CGFloat(style.scale), weight: .regular))
        mic.configuration?.baseForegroundColor = state == .listening ? style.accentUI : (state == .busy ? style.mutedUI : style.textUI)
        mic.configuration?.background.backgroundColor = state == .listening ? style.activeUI : .clear
        mic.accessibilityLabel = state == .idle ? "Dictate" : "Stop dictation"
        mic.accessibilityValue = state == .listening ? "Listening" : nil
    }

    // MARK: Look

    private func restyle() {
        applyGlass()
        backgroundColor = glassView == nil ? style.panelUI : .clear
        rule.backgroundColor = style.dividerUI
        rule.isHidden = glassView != nil
        hideDivider.backgroundColor = style.dividerUI
        for line in dividers { line.backgroundColor = style.dividerUI }
        // The row spans the bar, in focus mode too, and its ends are padded clear of the display corners.
        // On glass the end keys also keep clear of the capsule's rounded ends.
        let capsuleInset: CGFloat = glassView == nil ? 0 : 12
        stackLeading.constant = CGFloat(padding.left) + capsuleInset
        hideTrailing.constant = CGFloat(padding.right) + capsuleInset
        for (action, button) in buttons {
            switch roles[action] ?? .plain {
            case .plain: button.configuration?.baseForegroundColor = style.textUI
            case .hotkey: button.configuration?.baseForegroundColor = style.magentaUI
            case .muted: button.configuration?.baseForegroundColor = style.mutedUI
            }
        }
        setLatches(control: latch(.control), alt: latch(.alt), shift: latch(.shift))
        setDictation(dictation)
        applyMic()
    }
    /// Native on iOS 26 puts the row on glass, inset from the edges like the system's own bars; anything else takes it away.
    private func applyGlass() {
        guard #available(iOS 26, *), style.glass else {
            glassView?.removeFromSuperview(); glassView = nil
            return
        }
        guard glassView == nil else { return }
        let glass = UIVisualEffectView(effect: UIGlassEffect())
        glass.translatesAutoresizingMaskIntoConstraints = false
        glass.isUserInteractionEnabled = false
        glass.cornerConfiguration = .capsule()
        insertSubview(glass, at: 0)
        NSLayoutConstraint.activate([
            glass.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6), trailingAnchor.constraint(equalTo: glass.trailingAnchor, constant: 6),
            glass.topAnchor.constraint(equalTo: topAnchor, constant: 2), bottomAnchor.constraint(equalTo: glass.bottomAnchor, constant: 2)
        ])
        glassView = glass
    }
    override func layoutSubviews() {
        super.layoutSubviews()
        refreshPlacement()
    }

    // MARK: Placement

    /// Measures where the bar is and pads the ends of the row to match. It never changes the bar's size or place, so this cannot
    /// feed back into itself: it runs on every layout and keyboard change and settles at once.
    func refreshPlacement() {
        guard let window else { return }
        let frame = convert(bounds, to: window)
        let own = safeAreaInsets, outer = window.safeAreaInsets
        // At the bottom edge the bar's own safe area has the home indicator in it; that and the frame agree.
        let measured = KeyBarGeometry.position(barMaxY: frame.maxY, screenHeight: window.bounds.height, tolerance: max(1, outer.bottom + 1))
        let position: KeyBarPosition = own.bottom > 0 ? .screenBottom : measured
        let safe = KeyBarInsets(left: max(own.left, outer.left), bottom: max(own.bottom, outer.bottom), right: max(own.right, outer.right))
        applyPlacement(safeArea: safe, position: position)
    }
    /// Applies a placement. Also the entry point for tests, which cannot attach a hardware keyboard.
    func applyPlacement(safeArea: KeyBarInsets, position: KeyBarPosition) {
        let padding = KeyBarGeometry.padding(safeArea: safeArea, position: position)
        guard position != self.position || padding != self.padding else { return }
        self.position = position
        self.padding = padding
        restyle()
    }
    override func safeAreaInsetsDidChange() { super.safeAreaInsetsDidChange(); refreshPlacement() }
    /// The bar going away (keyboard hidden) while a finger is down must not leave an arrow repeating.
    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window == nil { endRepeat(); stopObservingKeyboard() } else { observeKeyboard(); refreshPlacement() }
    }
    /// The bar moves when the keyboard appears, hides or gives way to a hardware one, and none of that relayouts the bar itself.
    /// Observed only while the bar is on screen.
    private func observeKeyboard() {
        guard keyboardObservers.isEmpty else { return }
        for name in [UIResponder.keyboardDidShowNotification, UIResponder.keyboardDidChangeFrameNotification] {
            keyboardObservers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.refreshPlacement() }
            })
        }
    }
    private func stopObservingKeyboard() {
        keyboardObservers.forEach(NotificationCenter.default.removeObserver)
        keyboardObservers = []
    }
}
