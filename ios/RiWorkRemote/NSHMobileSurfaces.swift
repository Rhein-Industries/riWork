import SwiftUI
import UIKit
import NativeShell
import RiWorkCore

/// NSH owns the hosts, dashboard and project navigation. These are the existing
/// native workspaces and forms, presented by the same UIKit navigation owner as
/// the NSH root. There is no nested SwiftUI navigation stack around a terminal.
@MainActor final class NSHMobileSurfaces: NSObject, UINavigationControllerDelegate, UIGestureRecognizerDelegate, UIAdaptivePresentationControllerDelegate {
    let model: RemoteModel
    var onDismiss: (() -> Void)?
    var onVisibilityChange: ((Bool) -> Void)?
    var onProjects: (() -> Void)?
    var onHostAdded: ((String) -> Void)?
    var onHostRemoved: ((String) -> Void)?
    var onGlobalRequested: (() -> Void)?
    var onProjectCreated: ((RemoteProject) -> Void)?
    var isDecisionAnswered: ((HostedDashboardDecision) -> Bool)?
    var respondToDecision: ((HostedDashboardDecision, ChatCommand) async throws -> Void)?

    private weak var navigation: UINavigationController?
    private weak var root: UIViewController?
    private weak var workspace: UIViewController?
    private weak var sheet: NSHMobileSheetController?
    private var sheetHostID: String?
    private var workspaceKey: String?
    private var lastVisible = false
    private var pendingLink: URL?
    private var afterSheet: (() -> Void)?
    private var canDismissSheet: (() -> Bool)?
    private var newProject: NewProjectSheetModel?
    private var createdProject: RemoteProject?

    init(model: RemoteModel) { self.model = model }

    /// Recolor existing controllers in place: changing a palette must not
    /// replace the navigation stack, keyboard responder or an open sheet.
    func applyStyle() {
        guard let navigation else { return }
        func paint(_ controller: UIViewController, _ style: DesktopStyle) {
            let appearance: UIUserInterfaceStyle = style.theme.followsSystemAppearance ? .unspecified : style.theme.dark.map { $0 ? .dark : .light } ?? .unspecified
            if controller.overrideUserInterfaceStyle != appearance { controller.overrideUserInterfaceStyle = appearance }
            controller.viewIfLoaded?.backgroundColor = style.backgroundUI
            controller.viewIfLoaded?.tintColor = style.accentUI
            controller.setNeedsStatusBarAppearanceUpdate()
        }
        let style = model.theme.style
        paint(navigation, style)
        for controller in navigation.viewControllers { paint(controller, style) }
        if let sheet { paint(sheet, sheetHostID.map { model.theme.style(for: $0) } ?? style) }
    }

    func makeNavigation(root: UIViewController) -> UINavigationController {
        let navigation = NSHMobileNavigationController(rootViewController: root)
        self.navigation = navigation
        self.root = root
        navigation.delegate = self
        navigation.setNavigationBarHidden(true, animated: false)
        navigation.view.backgroundColor = model.theme.style.backgroundUI
        navigation.interactivePopGestureRecognizer?.delegate = self
        return navigation
    }

    func present(_ request: NSHMobilePresentation) {
        // A guarded presentation can be refused (a host was removed, another
        // sheet still owns the presenter). Release NSH's optimistic input lock
        // to the hierarchy that actually exists, even if it did not change.
        defer { publishVisibility(force: true) }
        switch request {
        case .project(let project): openProject(project)
        case .global: openGlobal()
        case .addHost: pairDesktop()
        case .hostDetails(let id):
            guard let desktop = model.desktops.first(where: { $0.id == id }) else { return }
            presentSheet(NSHMobileHostDetails(model: model, desktop: desktop, removed: { [weak self] in self?.onHostRemoved?(id) }), hostID: id)
        case .settings: presentSheet(DisplaySettingsSheet(model: model))
        case .newProject: openNewProject()
        case .decision(let hostID, let decision):
            guard let desktop = model.desktops.first(where: { $0.id == hostID }) else { return }
            let item = HostedDashboardDecision(hostID: hostID, hostName: desktop.name, decision: decision)
            presentSheet(DashboardDecisionSheet(item: item, answered: isDecisionAnswered?(item) ?? false) { [weak self] command in
                guard let respond = self?.respondToDecision else { throw DashboardResponseError.hostUnavailable }
                try await respond(item, command)
            }, hostID: hostID)
        case .libraryRecovery:
            presentSheet(NSHMobileLibraryRecovery(model: model))
        case .dismiss: dismissNative()
        }
    }

    /// Pairing material stays in Swift. The existing details/confirmation flow
    /// also applies when a URL arrives while a native workspace is on screen.
    func handleOpenURL(_ url: URL) {
        guard url.scheme?.lowercased() == "riwork" else { return }
        guard sheet == nil, navigation?.transitionCoordinator == nil, navigation?.viewIfLoaded?.window != nil else {
            pendingLink = url
            return
        }
        pairDesktop(text: url.absoluteString, fromLink: true, overPresented: true)
    }

    func dismissNative() {
        if sheet != nil {
            dismissSheet { [weak self] in self?.returnToRoot() }
        } else { returnToRoot() }
    }

    private func openProject(_ project: RemoteProject) {
        let key = "project:\(model.selectedDesktopID ?? ""):\(project.id)"
        guard key != workspaceKey else { return }
        let content = TerminalTabsView(model: model, project: project, onBack: { [weak self] in self?.pop() }, onProjects: { [weak self] in
            self?.onProjects?()
            self?.returnToRoot()
        })
        showWorkspace(content, key: key)
    }

    private func openGlobal() {
        let key = "global:\(model.selectedDesktopID ?? "")"
        guard key != workspaceKey else { return }
        showWorkspace(NSHMobileGlobalWorkspace(model: model, back: { [weak self] in self?.pop() }, projects: { [weak self] in
            self?.onProjects?()
            self?.returnToRoot()
        }, newTerminal: { [weak self] project in
            self?.model.newTerminalRequestedProject = project.id
            self?.openProject(project)
        }), key: key)
    }

    private func showWorkspace<Content: View>(_ content: Content, key: String) {
        guard let navigation, let root else { return }
        if sheet != nil {
            dismissSheet { [weak self] in self?.showWorkspace(content, key: key) }
            return
        }
        if let presenter = navigation.topViewController, presenter.presentedViewController != nil {
            presenter.dismiss(animated: true) { [weak self] in self?.showWorkspace(content, key: key) }
            return
        }
        model.keyboard.tabShortcuts.cancel()
        let view = NSHMobileThemed(model: model, content: content)
            .environment(\.chatPresentFiles, { [weak self] files, selected in self?.openFiles(files, selected: selected) })
            .onChange(of: model.globalScreenRequests) { [weak self] _, _ in
                guard let self, self.workspaceKey?.hasPrefix("project:") == true else { return }
                self.onGlobalRequested?()
            }
        let controller = NSHMobileWorkspaceController(rootView: AnyView(view))
        controller.didAppear = { [weak self] in self?.showPendingLink() }
        controller.view.backgroundColor = model.theme.style.backgroundUI
        workspace = controller
        workspaceKey = key
        navigation.setViewControllers([root, controller], animated: navigation.view.window != nil)
        publishVisibility()
    }

    private func openFiles(_ files: [ChatFileEntry], selected: ChatFileEntry) {
        guard let navigation, navigation.topViewController === workspace,
              navigation.presentedViewController == nil,
              navigation.topViewController?.presentedViewController == nil,
              navigation.transitionCoordinator == nil else { return }
        model.keyboard.tabShortcuts.cancel()
        navigation.view.endEditing(true)
        let viewer = ChatFileViewer(files: files, initialID: selected.id, initialLine: selected.line, onBack: { [weak self] in self?.pop() })
        let controller = UIHostingController(rootView: NSHMobileThemed(model: model, content: viewer))
        controller.view.backgroundColor = model.theme.style.backgroundUI
        navigation.pushViewController(controller, animated: true)
    }

    private func pop() {
        guard let navigation, navigation.viewControllers.count > 1, navigation.transitionCoordinator == nil else { return }
        model.keyboard.tabShortcuts.cancel()
        navigation.view.endEditing(true)
        navigation.popViewController(animated: true)
    }

    private func returnToRoot() {
        guard let navigation else { return }
        model.keyboard.tabShortcuts.cancel()
        navigation.view.endEditing(true)
        if navigation.viewControllers.count > 1 { navigation.popToRootViewController(animated: navigation.view.window != nil) }
        else { workspace = nil; workspaceKey = nil; publishVisibility() }
    }

    private func pairDesktop(text: String = "", fromLink: Bool = false, overPresented: Bool = false) {
        presentSheet(PairDesktopSheet(model: model, initialText: text, fromLink: fromLink) { [weak self] id in
            // PairDesktopSheet dismisses after saving. Activate only after its
            // dismissal, when the root may safely navigate or push a workspace.
            self?.afterSheet = { [weak self] in self?.finishPairing(id) }
        }, overPresented: overPresented)
    }

    private func finishPairing(_ id: String) {
        if let presenter = navigation?.topViewController, presenter.presentedViewController != nil {
            presenter.dismiss(animated: true) { [weak self] in self?.finishPairing(id) }
            return
        }
        returnToRoot()
        onHostAdded?(id)
    }

    private func openNewProject() {
        guard newProject == nil, model.canOpenNewProject else { return }
        let form = NewProjectSheetModel(model: model)
        form.onCreated = { [weak self] project in self?.createdProject = project }
        form.dismiss = { [weak self] in
            guard let self else { return }
            let project = self.createdProject
            self.dismissSheet { [weak self] in
                guard let self, let project else { return }
                self.model.newTerminalRequestedProject = project.id
                self.onProjectCreated?(project)
                self.openProject(project)
            }
        }
        newProject = form
        if !presentSheet(NewProjectSheet(sheet: form), canDismiss: { !form.busy }, detents: [.medium(), .large()]) { newProject = nil }
    }

    @discardableResult private func presentSheet<Content: View>(_ content: Content, hostID: String? = nil, canDismiss: (() -> Bool)? = nil,
                                                             overPresented: Bool = false, detents: [UISheetPresentationController.Detent] = [.large()]) -> Bool {
        guard let navigation, var presenter = navigation.topViewController, sheet == nil else { return false }
        if overPresented {
            while let presented = presenter.presentedViewController { presenter = presented }
        }
        guard presenter.presentedViewController == nil else { return false }
        model.keyboard.tabShortcuts.cancel()
        presenter.view.endEditing(true)
        let controller = NSHMobileSheetController(rootView: AnyView(NSHMobileThemed(model: model, hostID: hostID, content: content)))
        controller.modalPresentationStyle = .pageSheet
        controller.view.backgroundColor = (hostID.map { model.theme.style(for: $0) } ?? model.theme.style).backgroundUI
        controller.didDismiss = { [weak self, weak controller] in
            guard let self, let controller, self.sheet === controller else { return }
            self.sheetEnded()
        }
        sheet = controller
        sheetHostID = hostID
        applyStyle()
        canDismissSheet = canDismiss
        controller.presentationController?.delegate = self
        controller.sheetPresentationController?.detents = detents
        controller.sheetPresentationController?.prefersGrabberVisible = true
        presenter.present(controller, animated: true)
        publishVisibility()
        return true
    }

    private func dismissSheet(_ completion: (() -> Void)? = nil) {
        guard let sheet else { completion?(); return }
        guard canDismissSheet?() != false else { return }
        afterSheet = completion
        sheet.dismiss(animated: true) { [weak self, weak sheet] in
            guard let self, let sheet, self.sheet === sheet else { return }
            self.sheetEnded()
        }
    }

    private func sheetEnded() {
        guard sheet != nil else { return }
        sheet = nil
        sheetHostID = nil
        canDismissSheet = nil
        newProject = nil
        createdProject = nil
        let completion = afterSheet
        afterSheet = nil
        publishVisibility()
        completion?()
        showPendingLink()
    }

    private func showPendingLink() {
        guard let url = pendingLink, sheet == nil, navigation?.transitionCoordinator == nil, navigation?.viewIfLoaded?.window != nil else { return }
        pendingLink = nil
        handleOpenURL(url)
    }

    private func publishVisibility(force: Bool = false) {
        let visible = sheet != nil || (navigation?.viewControllers.count ?? 1) > 1
        guard force || visible != lastVisible else { return }
        lastVisible = visible
        onVisibilityChange?(visible)
        if !visible { onDismiss?() }
    }

    func navigationController(_ navigationController: UINavigationController, willShow viewController: UIViewController, animated: Bool) {
        navigationController.setNavigationBarHidden(true, animated: false)
        navigationController.interactivePopGestureRecognizer?.delegate = self
        navigationController.setNeedsStatusBarAppearanceUpdate()
    }

    func navigationController(_ navigationController: UINavigationController, didShow viewController: UIViewController, animated: Bool) {
        navigationController.interactivePopGestureRecognizer?.isEnabled = navigationController.viewControllers.count > 1
        if viewController === root {
            workspace = nil
            workspaceKey = nil
            model.keyboard.tabShortcuts.cancel()
        }
        publishVisibility()
        showPendingLink()
    }

    func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
        guard let navigation else { return false }
        return navigation.viewControllers.count > 1 && navigation.transitionCoordinator == nil
            && navigation.presentedViewController == nil && navigation.topViewController?.presentedViewController == nil
    }

    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer, shouldBeRequiredToFailBy other: UIGestureRecognizer) -> Bool {
        other.view is UIScrollView
    }

    func presentationControllerShouldDismiss(_ presentationController: UIPresentationController) -> Bool { canDismissSheet?() != false }
    func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
        if presentationController.presentedViewController === sheet { sheetEnded() }
    }

    static func registerSymbols(on app: NshAppView) {
        app.register(component: "riwork.mobile.symbol") { NSHMobileSymbol(context: $0) }
    }
}

/// The top native/NSH controller owns status-bar decisions, including terminal
/// focus mode. UIKit continues to calculate each child's safe area and forwards
/// appearances, including cancelled interactive pops, to its hosting controller.
private final class NSHMobileNavigationController: UINavigationController {
    override var childForStatusBarStyle: UIViewController? { topViewController }
    override var childForStatusBarHidden: UIViewController? { topViewController }
    override var childForHomeIndicatorAutoHidden: UIViewController? { topViewController }
    override var childForScreenEdgesDeferringSystemGestures: UIViewController? { topViewController }
}

private final class NSHMobileSheetController: UIHostingController<AnyView> {
    var didDismiss: (() -> Void)?
    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        if isBeingDismissed || presentingViewController == nil { didDismiss?() }
    }
}

private final class NSHMobileWorkspaceController: UIHostingController<AnyView> {
    var didAppear: (() -> Void)?
    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        didAppear?()
    }
}

private struct NSHMobileThemed<Content: View>: View {
    @Bindable var model: RemoteModel
    var hostID: String? = nil
    let content: Content
    var body: some View {
        content
            .desktopThemed(hostID.map { model.theme.style(for: $0) } ?? model.theme.style)
            .environment(\.tabShortcuts, model.keyboard.tabShortcuts)
            .onChange(of: model.appActive) { _, active in if !active { model.keyboard.tabShortcuts.cancel() } }
            .onChange(of: model.keyboard.hardware.isAttached) { _, _ in model.keyboard.tabShortcuts.cancel() }
            .onChange(of: model.keyboard.software.isShown) { _, _ in model.keyboard.tabShortcuts.cancel() }
            .onChange(of: model.keyboard.layout) { _, _ in model.keyboard.tabShortcuts.cancel() }
    }
}

private struct NSHMobileGlobalWorkspace: View {
    @Bindable var model: RemoteModel
    let back: () -> Void
    let projects: () -> Void
    let newTerminal: (RemoteProject) -> Void
    @State private var shortcutOwner = UUID()

    var body: some View {
        GlobalOrchestratorScreen(model: model, onBack: back)
            .overlay { TabShortcutGuide(controller: model.keyboard.tabShortcuts) }
            .onAppear {
                model.keyboard.tabShortcuts.setAppHandler(owner: shortcutOwner) { action in
                    switch action {
                    case .projects: projects()
                    case .newTerminal:
                        if let id = model.projectID, let project = model.project(withID: id) { newTerminal(project) }
                    case .modelPicker: if model.selectedChat != nil { model.keyboard.requestModelPicker() }
                    default: break
                    }
                }
            }
            .onDisappear { model.keyboard.tabShortcuts.clearAppHandler(owner: shortcutOwner) }
    }
}

private struct NSHMobileLibraryRecovery: View {
    @Bindable var model: RemoteModel
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: "Saved pairings") { Button("Done") { dismiss() } }
            LibraryFailureView(model: model)
        }
    }
}

private struct NSHMobileHostDetails: View {
    @Bindable var model: RemoteModel
    let desktop: SavedDesktop
    let removed: () -> Void
    @Environment(\.desktopStyle) private var style
    @Environment(\.dismiss) private var dismiss
    @State private var renaming = false
    @State private var confirmingRemoval = false
    @State private var busy = false
    @State private var failure: String?

    private var current: SavedDesktop { model.desktops.first(where: { $0.id == desktop.id }) ?? desktop }
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: current.name) { Button("Done") { dismiss() }.disabled(busy) }
            List {
                Section {
                    Label(current.pairing.relayHost, systemImage: "network").foregroundStyle(style.muted)
                    Button("Rename desktop", systemImage: "pencil") { renaming = true }.disabled(busy)
                    Button("Remove from this device", systemImage: "trash", role: .destructive) { confirmingRemoval = true }.disabled(busy)
                }.listRowBackground(style.panel)
                if let failure { Section { Text(failure).foregroundStyle(style.warning) }.listRowBackground(style.panel) }
                Section { Text("Pairing keys stay in Keychain. Removing this pairing leaves desktop sessions running.").font(style.system(.caption)).foregroundStyle(style.muted) }
                    .listRowBackground(style.panel)
            }
            .scrollContentBackground(.hidden)
            .environment(\.defaultMinListRowHeight, max(44, style.pt(44)))
        }
        .desktopSheetSurface(style)
        .interactiveDismissDisabled(busy)
        .sheet(isPresented: $renaming) { RenameDesktopSheet(model: model, desktop: current).desktopThemed(model.theme.style(for: desktop.id)) }
        .alert("Remove pairing?", isPresented: $confirmingRemoval) {
            Button("Remove pairing", role: .destructive) {
                busy = true
                Task {
                    do { try await model.remove(id: desktop.id); removed(); dismiss() }
                    catch { failure = error.localizedDescription }
                    busy = false
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Removes \(current.name)’s local pairing keys. Desktop sessions keep running. Revoke this device on the desktop to deny future access.")
        }
    }
}

@MainActor private final class NSHMobileSymbol: NshComponent {
    let imageView = UIImageView()
    let context: NshComponentContext
    var view: UIView { imageView }
    private var name = ""
    private var size: CGFloat = 20

    init(context: NshComponentContext) {
        self.context = context
        imageView.contentMode = .scaleAspectFit
        imageView.tintColor = .label
        imageView.isAccessibilityElement = false
        context.setSize(CGSize(width: size, height: size))
    }

    func setProp(_ name: String, _ value: Any?) {
        switch name {
        case "name":
            guard let value = value as? String, self.name != value else { return }
            self.name = value
            updateImage()
        case "size":
            guard let value = value as? NSNumber else { return }
            let next = min(96, max(8, CGFloat(value.doubleValue)))
            guard next != size else { return }
            size = next
            context.setSize(CGSize(width: size, height: size))
            updateImage()
        case "color": imageView.tintColor = Self.color(value) ?? .label
        default: break
        }
    }

    private func updateImage() {
        imageView.image = UIImage(systemName: name, withConfiguration: UIImage.SymbolConfiguration(pointSize: size, weight: .regular))?.withRenderingMode(.alwaysTemplate)
    }

    private static func color(_ value: Any?) -> UIColor? {
        if let number = value as? NSNumber {
            let argb = number.uint32Value
            return UIColor(red: CGFloat((argb >> 16) & 255) / 255, green: CGFloat((argb >> 8) & 255) / 255,
                           blue: CGFloat(argb & 255) / 255, alpha: CGFloat((argb >> 24) & 255) / 255)
        }
        guard let text = value as? String, text.hasPrefix("#") else { return nil }
        var hex = String(text.dropFirst())
        if hex.count == 3 || hex.count == 4 { hex = hex.map { "\($0)\($0)" }.joined() }
        guard (hex.count == 6 || hex.count == 8), let number = UInt64(hex, radix: 16) else { return nil }
        let rgba = hex.count == 6 ? (number << 8) | 255 : number
        return UIColor(red: CGFloat((rgba >> 24) & 255) / 255, green: CGFloat((rgba >> 16) & 255) / 255,
                       blue: CGFloat((rgba >> 8) & 255) / 255, alpha: CGFloat(rgba & 255) / 255)
    }
}
