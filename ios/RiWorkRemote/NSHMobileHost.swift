import NativeShell
import Observation
import SwiftUI
import UIKit

/// The application UI starts in NSH. UIKit owns the window and the native
/// surfaces retained during migration, so their keyboard and gesture responders
/// remain in the same view-controller hierarchy.
struct NSHMobileRoot: View {
    let model: RemoteModel
    @State private var pairingLink: MobilePairingLink?

    var body: some View {
        NSHMobileContainer(model: model, pairingLink: pairingLink, style: model.theme.style)
            .ignoresSafeArea()
            .onOpenURL { pairingLink = MobilePairingLink(url: $0) }
    }
}

private struct MobilePairingLink {
    let id = UUID()
    let url: URL
}

private struct NSHMobileContainer: UIViewControllerRepresentable {
    let model: RemoteModel
    let pairingLink: MobilePairingLink?
    let style: DesktopStyle

    func makeCoordinator() -> NSHMobileCoordinator { NSHMobileCoordinator(model: model) }
    func makeUIViewController(context: Context) -> UINavigationController {
        context.coordinator.applyStyle(style)
        return context.coordinator.navigation
    }
    func updateUIViewController(_ controller: UINavigationController, context: Context) {
        context.coordinator.applyStyle(style)
        guard let pairingLink, context.coordinator.handledLink != pairingLink.id else { return }
        context.coordinator.handledLink = pairingLink.id
        context.coordinator.surfaces.handleOpenURL(pairingLink.url)
    }
    static func dismantleUIViewController(_ controller: UINavigationController, coordinator: NSHMobileCoordinator) {
        coordinator.bridge.detach()
    }
}

@MainActor
private final class NSHMobileCoordinator {
    let bridge: NSHMobileBridge
    let surfaces: NSHMobileSurfaces
    let host: NSHMobileHostController
    let navigation: UINavigationController
    var handledLink: UUID?

    func applyStyle(_ style: DesktopStyle) {
        host.applyStyle(style)
        surfaces.applyStyle()
    }

    init(model: RemoteModel) {
        let bridge = NSHMobileBridge(model: model)
        let surfaces = NSHMobileSurfaces(model: model)
        let host = NSHMobileHostController(bridge: bridge)
        self.bridge = bridge
        self.surfaces = surfaces
        self.host = host
        self.navigation = surfaces.makeNavigation(root: host)
        bridge.onPresentation = { [weak surfaces] in surfaces?.present($0) }
        surfaces.onVisibilityChange = { [weak bridge] in bridge?.setNativePresented($0) }
        surfaces.onDismiss = { [weak bridge] in bridge?.nativeDismissed() }
        surfaces.onProjects = { [weak bridge] in bridge?.showProjects() }
        surfaces.onHostAdded = { [weak bridge, weak model] id in
            Task { @MainActor in
                do { try await bridge?.connect(hostID: id) }
                catch { model?.error = error.localizedDescription }
            }
        }
        surfaces.onProjectCreated = { [weak bridge] in bridge?.projectCreated($0) }
        surfaces.onGlobalRequested = { [weak bridge] in bridge?.showGlobal() }
        surfaces.isDecisionAnswered = { [weak bridge] in bridge?.isDecisionAnswered($0) ?? false }
        surfaces.respondToDecision = { [weak bridge] item, command in
            guard let bridge else { throw MobileHostError.unavailable }
            try await bridge.respondToDecision(item, command: command)
        }
    }
}

private enum MobileHostError: LocalizedError {
    case unavailable
    var errorDescription: String? { "The mobile connection is no longer available." }
}

@MainActor
private final class NSHMobileHostController: UIViewController {
    private let app = NshAppView()
    private let bridge: NSHMobileBridge
    private var failureView: UIScrollView?
    private let keyboardHost = KeyCommandHost.HostView()
    private let shortcutOwner = UUID()
    private let guide = UIHostingController(rootView: AnyView(EmptyView()))
    private var shortcutPolicy = false
    private var rootVisible = false
    private var guideObservation = UUID()

    init(bridge: NSHMobileBridge) {
        self.bridge = bridge
        super.init(nibName: nil, bundle: nil)
    }
    required init?(coder: NSCoder) { nil }

    func applyStyle(_ style: DesktopStyle) {
        let appearance: UIUserInterfaceStyle = style.theme.followsSystemAppearance ? .unspecified : style.theme.dark.map { $0 ? .dark : .light } ?? .unspecified
        if overrideUserInterfaceStyle != appearance { overrideUserInterfaceStyle = appearance }
        viewIfLoaded?.backgroundColor = style.backgroundUI
        app.backgroundColor = style.backgroundUI
        app.tintColor = style.accentUI
        setNeedsStatusBarAppearanceUpdate()
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        applyStyle(bridge.model.theme.style)
        // This 1-point responder lives beneath the app. It can receive Clicks
        // prefix keys without putting a transparent touch target over NSH.
        keyboardHost.frame = CGRect(x: 0, y: 0, width: 1, height: 1)
        keyboardHost.isAccessibilityElement = false
        keyboardHost.avoidTextInput = true
        keyboardHost.tabShortcuts = bridge.model.keyboard.tabShortcuts
        keyboardHost.table.actions = [
            KeyAction(input: "r", modifiers: .command, title: "Refresh", run: { [weak self] in
                self?.app.send("mobile.command", ["type": "refresh"])
            }),
            KeyAction(input: ",", modifiers: .command, title: "Settings", run: { [weak self] in
                self?.app.send("mobile.command", ["type": "present", "name": "settings"])
            }),
        ]
        view.addSubview(keyboardHost)
        app.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(app)
        NSLayoutConstraint.activate([
            app.topAnchor.constraint(equalTo: view.topAnchor),
            app.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            app.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            app.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        app.onError = { [weak self] in self?.showFailure($0) }
        registerForTraitChanges([UITraitUserInterfaceStyle.self, UITraitAccessibilityContrast.self]) { (controller: NSHMobileHostController, _: UITraitCollection) in
            controller.bridge.traitsChanged()
        }
        addChild(guide)
        guide.view.translatesAutoresizingMaskIntoConstraints = false
        guide.view.backgroundColor = .clear
        guide.view.isHidden = true
        view.addSubview(guide.view)
        let guideWidth = guide.view.widthAnchor.constraint(equalToConstant: 484)
        let guideHeight = guide.view.heightAnchor.constraint(equalToConstant: 536)
        guideWidth.priority = .defaultHigh
        guideHeight.priority = .defaultHigh
        NSLayoutConstraint.activate([
            guideWidth, guideHeight,
            guide.view.widthAnchor.constraint(lessThanOrEqualTo: view.safeAreaLayoutGuide.widthAnchor, constant: -8),
            guide.view.heightAnchor.constraint(lessThanOrEqualTo: view.safeAreaLayoutGuide.heightAnchor, constant: -8),
            guide.view.centerXAnchor.constraint(equalTo: view.safeAreaLayoutGuide.centerXAnchor),
            guide.view.centerYAnchor.constraint(equalTo: view.safeAreaLayoutGuide.centerYAnchor),
        ])
        guide.didMove(toParent: self)
        bridge.onRootShortcutPolicyChange = { [weak self] eligible in
            self?.shortcutPolicy = eligible
            self?.updateShortcuts()
        }
        NSHMobileSurfaces.registerSymbols(on: app)
        loadUI()
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        rootVisible = true
        updateShortcuts()
    }
    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        rootVisible = false
        updateShortcuts()
    }

    private func updateShortcuts() {
        let active = rootVisible && shortcutPolicy && failureView == nil
        keyboardHost.active = active
        if active {
            bridge.model.keyboard.tabShortcuts.setAppHandler(owner: shortcutOwner) { [weak bridge] in bridge?.rootShortcut($0) }
            keyboardHost.takeFocus()
        } else {
            bridge.model.keyboard.tabShortcuts.clearAppHandler(owner: shortcutOwner)
            if keyboardHost.isFirstResponder { _ = keyboardHost.resignFirstResponder() }
        }
        updateGuide()
    }

    private func updateGuide() {
        let token = UUID()
        guideObservation = token
        withObservationTracking {
            let model = bridge.model
            let visible = rootVisible && shortcutPolicy && failureView == nil && model.keyboard.tabShortcuts.guideVisible
            // The guide owns only this bounded panel while visible; its Close
            // button stays usable and the rest of the screen receives touches.
            guide.view.isHidden = !visible
            guide.rootView = AnyView(TabShortcutGuide(controller: model.keyboard.tabShortcuts).desktopThemed(model.theme.style))
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, self.guideObservation == token else { return }
                self.updateGuide()
            }
        }
    }

    @objc private func loadUI() {
        failureView?.removeFromSuperview()
        failureView = nil
        app.isHidden = false
        setNeedsStatusBarAppearanceUpdate()
        do {
            bridge.attach(to: app)
            // Remote operations go through the existing model. Pairing secrets
            // and the encrypted transport never enter the JavaScript VM.
            try app.grantCapabilities(#"{"host":["mobile.snapshot","mobile.action"],"clipboard":true,"openUrl":true}"#)
            guard let url = Bundle.main.url(forResource: "bundle", withExtension: "nshc", subdirectory: "nsh") else {
                throw NSError(domain: "RiWorkMobile", code: 1, userInfo: [NSLocalizedDescriptionKey: "The NSH mobile interface is missing from this build."])
            }
            try app.loadBundle(at: url)
        } catch {
            showFailure(error.localizedDescription)
        }
    }

    private func showFailure(_ message: String) {
        bridge.detach()
        // A hidden NSH input can remain first responder after its engine
        // fails. Recovery must not inherit its keyboard or focused draft.
        view.endEditing(true)
        failureView?.removeFromSuperview()
        app.isHidden = true
        let title = UILabel()
        title.text = "RiWork couldn’t open its interface"
        title.font = .preferredFont(forTextStyle: .headline)
        title.adjustsFontForContentSizeCategory = true
        title.numberOfLines = 0
        let detail = UILabel()
        detail.text = message
        detail.font = .preferredFont(forTextStyle: .body)
        detail.adjustsFontForContentSizeCategory = true
        detail.numberOfLines = 0
        let retry = UIButton(type: .system)
        retry.setTitle("Try again", for: .normal)
        retry.titleLabel?.font = .preferredFont(forTextStyle: .body)
        retry.titleLabel?.adjustsFontForContentSizeCategory = true
        retry.heightAnchor.constraint(greaterThanOrEqualToConstant: 44).isActive = true
        retry.addTarget(self, action: #selector(loadUI), for: .touchUpInside)
        let stack = UIStackView(arrangedSubviews: [title, detail, retry])
        stack.axis = .vertical
        stack.spacing = 16
        stack.translatesAutoresizingMaskIntoConstraints = false
        let recovery = UIScrollView()
        recovery.translatesAutoresizingMaskIntoConstraints = false
        recovery.alwaysBounceVertical = false
        recovery.keyboardDismissMode = .interactive
        recovery.addSubview(stack)
        view.addSubview(recovery)
        NSLayoutConstraint.activate([
            recovery.leadingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.leadingAnchor),
            recovery.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor),
            recovery.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            recovery.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: recovery.contentLayoutGuide.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(equalTo: recovery.contentLayoutGuide.trailingAnchor, constant: -24),
            stack.topAnchor.constraint(equalTo: recovery.contentLayoutGuide.topAnchor, constant: 24),
            stack.bottomAnchor.constraint(equalTo: recovery.contentLayoutGuide.bottomAnchor, constant: -24),
            stack.widthAnchor.constraint(equalTo: recovery.frameLayoutGuide.widthAnchor, constant: -48),
        ])
        failureView = recovery
        updateShortcuts()
        setNeedsStatusBarAppearanceUpdate()
        UIAccessibility.post(notification: .screenChanged, argument: title)
    }

    override var preferredStatusBarStyle: UIStatusBarStyle {
        if let dark = bridge.model.theme.style.theme.dark { return dark ? .lightContent : .darkContent }
        return failureView == nil ? NshAppView.preferredStatusBarStyle : .default
    }
    override var prefersStatusBarHidden: Bool { failureView == nil && NshAppView.prefersStatusBarHidden }
    override var preferredStatusBarUpdateAnimation: UIStatusBarAnimation { NshAppView.preferredStatusBarUpdateAnimation }
}
