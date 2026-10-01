import UIKit
import RiWorkCore

/// A row of keys that scrolls sideways. A finger that lands on a key and then drags scrolls the row instead of pressing the key.
private final class KeyScrollView: UIScrollView {
    override func touchesShouldCancel(in view: UIView) -> Bool { true }
}

/// The key row on the keyboard: special keys, Ctrl and Alt, symbols that are awkward on the iOS keyboard, and hotkeys.
/// It is the input accessory view, and always 44 pt tall, exactly where iOS puts it:
/// - above the software keyboard, or
/// - alone at the bottom edge of the screen when a hardware keyboard is attached (iOS hides the software keyboard then).
///
/// The row scrolls. Its ends are padded (`KeyBarGeometry`) so that, scrolled fully left or right, the first and the last key are
/// clear of the display's rounded corners. Hide stays put at the trailing end, so it is always within reach; everything else,
/// including the "+" that opens the hotkey editor at the very end, is in the scrolling part.
@MainActor final class KeyBarView: UIInputView {
    enum Action: Hashable {
        case key(TerminalKey), control, alt, text(String), paste, hide, hotkey(String), editHotkeys, palette
    }
    /// How the bar is drawn when it is alone at the bottom edge. Above the keyboard it is always a strip.
    enum Presentation: Equatable { case strip, pill }
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
    var presentation = Presentation.strip { didSet { if presentation != oldValue { restyle() } } }
    var style = DesktopStyle.builtIn {
        didSet {
            guard style != oldValue else { return }
            // A new interface size changes every key's size and the bar's height; colors only need a restyle.
            if style.scale != oldValue.scale { applyScale() } else { restyle() }
        }
    }
    /// Where the bar is and the padding at the ends of the row that follows from it.
    private(set) var position = KeyBarPosition.aboveKeyboard
    private(set) var padding = KeyBarPadding.zero
    /// The pill is used only when the bar is alone at the bottom edge and the caller asked for it.
    var isPill: Bool { presentation == .pill && position == .screenBottom }
    /// The scrolling row, for tests.
    var scrollView: UIScrollView { scroll }

    private let rule = UIView()
    private let row = UIView()
    private let scroll = KeyScrollView()
    private let stack = UIStackView()
    private let hideDivider = UIView()
    private var dividers: [UIView] = []
    private var roles: [Action: Role] = [:]
    private var rowLeading: NSLayoutConstraint!, rowTrailing: NSLayoutConstraint!, rowTop: NSLayoutConstraint!, rowBottom: NSLayoutConstraint!
    private var stackLeading: NSLayoutConstraint!, hideTrailing: NSLayoutConstraint!
    private var hideWidth: NSLayoutConstraint!, hideMinWidth: NSLayoutConstraint?, dividerInsets: [NSLayoutConstraint] = [], stackTrailing: NSLayoutConstraint!
    private var controlArmed = false, altArmed = false
    private var repeatTask: Task<Void, Never>?
    private var didRepeat = false
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

        rowLeading = row.leadingAnchor.constraint(equalTo: leadingAnchor)
        rowTrailing = trailingAnchor.constraint(equalTo: row.trailingAnchor)
        rowTop = row.topAnchor.constraint(equalTo: topAnchor, constant: 1)
        rowBottom = bottomAnchor.constraint(equalTo: row.bottomAnchor)
        stackLeading = stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor)
        stackTrailing = stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -4)
        let hideDividerTop = hideDivider.topAnchor.constraint(equalTo: row.topAnchor, constant: 10)
        let hideDividerBottom = hideDivider.bottomAnchor.constraint(equalTo: row.bottomAnchor, constant: -10)
        dividerInsets = [hideDividerTop, hideDividerBottom]
        NSLayoutConstraint.activate([
            rule.topAnchor.constraint(equalTo: topAnchor), rule.leadingAnchor.constraint(equalTo: leadingAnchor),
            rule.trailingAnchor.constraint(equalTo: trailingAnchor), rule.heightAnchor.constraint(equalToConstant: 1),
            rowLeading, rowTrailing, rowTop, rowBottom,
            scroll.leadingAnchor.constraint(equalTo: row.leadingAnchor), scroll.topAnchor.constraint(equalTo: row.topAnchor), scroll.bottomAnchor.constraint(equalTo: row.bottomAnchor),
            hideDividerTop, hideDividerBottom,
            hideDivider.widthAnchor.constraint(equalToConstant: 1), scroll.trailingAnchor.constraint(equalTo: hideDivider.leadingAnchor),
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
        rebuild()
        invalidateIntrinsicContentSize()
        setNeedsLayout()
    }

    // MARK: Keys

    /// Special keys, then the hotkey menu button and the built-in and added hotkeys, then symbols, then the hotkey editor's "+" at the very end.
    private func rebuild() {
        for view in stack.arrangedSubviews { stack.removeArrangedSubview(view); view.removeFromSuperview() }
        let hide = buttons[.hide]
        buttons = [:]; roles = [:]; dividers = []
        if let hide { buttons[.hide] = hide; roles[.hide] = .plain }
        func key(_ key: TerminalKey, _ title: String?, _ symbol: String? = nil, _ label: String) {
            add(.key(key), title: title, symbol: symbol, label: label, role: .plain)
        }
        key(.escape, "Esc", nil, "Escape"); key(.tab, "Tab", nil, "Tab")
        add(.control, title: "Ctrl", symbol: nil, label: "Control", role: .plain); add(.alt, title: "Alt", symbol: nil, label: "Alt", role: .plain)
        key(.left, nil, "arrow.left", "Left arrow"); key(.up, nil, "arrow.up", "Up arrow"); key(.down, nil, "arrow.down", "Down arrow"); key(.right, nil, "arrow.right", "Right arrow")
        key(.backTab, "⇧Tab", nil, "Shift Tab"); key(.home, "Home", nil, "Home"); key(.end, "End", nil, "End")
        key(.pageUp, "PgUp", nil, "Page up"); key(.pageDown, "PgDn", nil, "Page down")
        key(.delete, "Del", nil, "Delete"); key(.backspace, nil, "delete.left", "Backspace"); key(.enter, nil, "return", "Enter")
        add(.paste, title: nil, symbol: "doc.on.clipboard", label: "Paste", role: .plain)
        addDivider()
        // The hotkey menu first: it reaches every hotkey, shortcut and key from the keyboard (also ⌘K).
        add(.palette, title: nil, symbol: "command", label: "Hotkey menu", role: .hotkey)
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
            configuration.attributedTitle = AttributedString(title, attributes: AttributeContainer([.font: style.uiFont("Menlo", size: size)]))
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

    /// Ctrl and Alt are sticky: they stay highlighted until the next key uses them.
    func setArmed(control: Bool, alt: Bool) {
        controlArmed = control; altArmed = alt
        for (action, armed) in [(Action.control, control), (Action.alt, alt)] {
            guard let button = buttons[action] else { continue }
            button.configuration?.baseForegroundColor = armed ? style.accentUI : style.textUI
            button.configuration?.background.backgroundColor = armed ? style.activeUI : .clear
            button.isSelected = armed
            button.accessibilityValue = armed ? "armed" : "not armed"
            button.accessibilityTraits = armed ? [.button, .selected] : .button
        }
    }
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
                self.didRepeat = true
                self.onAction?(action)
                try? await Task.sleep(for: .milliseconds(70))
            }
        }
    }
    private func endRepeat() { repeatTask?.cancel(); repeatTask = nil }

    // MARK: Look

    private func restyle() {
        let pill = isPill
        backgroundColor = pill ? .clear : style.panelUI
        rule.backgroundColor = style.dividerUI
        rule.isHidden = pill
        hideDivider.backgroundColor = style.dividerUI
        for line in dividers { line.backgroundColor = style.dividerUI }
        row.backgroundColor = pill ? style.panelUI : .clear
        row.layer.cornerRadius = pill ? (barHeight - 4) / 2 : 0
        row.layer.cornerCurve = .continuous
        row.layer.borderWidth = pill ? 1 : 0
        row.layer.borderColor = style.dividerUI.resolvedColor(with: traitCollection).cgColor
        row.clipsToBounds = pill
        // Strip: the row spans the bar and its ends are padded. Pill: the pill itself is inset by the padding, so the keys need little.
        rowLeading.constant = pill ? CGFloat(padding.left) : 0
        rowTrailing.constant = pill ? CGFloat(padding.right) : 0
        rowTop.constant = pill ? 2 : 1
        rowBottom.constant = pill ? 2 : 0
        stackLeading.constant = pill ? 10 : CGFloat(padding.left)
        hideTrailing.constant = pill ? 4 : CGFloat(padding.right)
        for (action, button) in buttons {
            switch roles[action] ?? .plain {
            case .plain: button.configuration?.baseForegroundColor = style.textUI
            case .hotkey: button.configuration?.baseForegroundColor = style.magentaUI
            case .muted: button.configuration?.baseForegroundColor = style.mutedUI
            }
        }
        setArmed(control: controlArmed, alt: altArmed)
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
