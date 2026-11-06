import SwiftUI
import UIKit
import NativeShell
import RiWorkCore

/// NSH owns the hosts, dashboards, projects and project pages (and the terminal in them, the `riwork.terminal` component). These are
/// the native screens that remain: a project's chat, the global orchestrator, the pairing, host, settings, memo and New chat sheets,
/// presented by the same UIKit navigation owner as the NSH root.
@MainActor final class NSHMobileSurfaces: NSObject, UINavigationControllerDelegate, UIGestureRecognizerDelegate, UIAdaptivePresentationControllerDelegate {
    let model: RemoteModel
    var onDismiss: (() -> Void)?
    var onVisibilityChange: ((Bool) -> Void)?
    var onProjects: (() -> Void)?
    var onHostAdded: ((String) -> Void)?
    var onHostRemoved: ((String) -> Void)?
    var onGlobalRequested: (() -> Void)?
    /// A native screen asked for a project's page: at a tab (`shell:<id>`), or with New terminal.
    var onProjectPage: ((_ project: RemoteProject, _ tab: String?, _ newTerminal: Bool) -> Void)?
    /// A project chord typed in the native chat (an `app_chords.ts` id), for the project page once the chat has gone.
    var onChord: ((String) -> Void)?
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
    private var newTab: NewTerminalSheetModel?
    /// A chord the chat handed on: sent when the page is back on top, dropped if the chat stays.
    private var pendingChord: String?

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
        case .chat(let project, let chat): openChat(project: project, chat: chat)
        case .global: openGlobal()
        case .addHost: pairDesktop()
        case .hostDetails(let id):
            guard let desktop = model.desktops.first(where: { $0.id == id }) else { return }
            presentSheet(NSHMobileHostDetails(model: model, desktop: desktop, removed: { [weak self] in self?.onHostRemoved?(id) }), hostID: id)
        case .settings: presentSheet(DisplaySettingsSheet(model: model))
        case .memo:
            // Done closes the editor (keeping what the Mac does not have yet); the sheet goes with it. A swipe closes it the same way.
            let close: () -> Void = { [weak self] in self?.model.closeMemoEditor() }
            if presentSheet(NSHMobileMemoSheet(model: model, closed: { [weak self] in self?.dismissSheet(close) })) { afterSheet = close }
            else { close() }
        case .newChat(let project): openNewChat(project: project)
        case .decision(let hostID, let decision):
            guard let desktop = model.desktops.first(where: { $0.id == hostID }) else { return }
            let item = HostedDashboardDecision(hostID: hostID, hostName: desktop.name, decision: decision)
            presentSheet(DashboardDecisionSheet(item: item, answered: isDecisionAnswered?(item) ?? false) { [weak self] command in
                guard let respond = self?.respondToDecision else { throw DashboardResponseError.hostUnavailable }
                try await respond(item, command)
            }, hostID: hostID)
        case .libraryRecovery:
            presentSheet(NSHMobileLibraryRecovery(model: model))
        }
    }

    /// Pairing material stays in Swift. The existing details/confirmation flow
    /// also applies when a URL arrives while a native workspace is on screen.
    func handleOpenURL(_ url: URL) {
        guard ["somtus", "riwork"].contains(url.scheme?.lowercased() ?? "") else { return }
        guard sheet == nil, navigation?.transitionCoordinator == nil, navigation?.viewIfLoaded?.window != nil else {
            pendingLink = url
            return
        }
        pairDesktop(text: url.absoluteString, fromLink: true, overPresented: true)
    }

    /// A project's chat over its TSX page: Back returns to the page, which still shows the terminal it had.
    private func openChat(project: String, chat: String) {
        let key = "chat:\(model.selectedDesktopID ?? ""):\(chat)"
        guard key != workspaceKey else { return }
        showWorkspace(NSHMobileChatWorkspace(model: model, projectID: project, chatID: chat, back: { [weak self] in self?.pop() },
                                             projects: { [weak self] in self?.onProjects?(); self?.returnToRoot() },
                                             chord: { [weak self] in self?.chatChord($0) }), key: key)
    }
    /// The project's chords in its chat, as the native project screen had them over a chat tab: Display settings opens over the
    /// chat; the others are the page's (another tab, New terminal, Open a worker or shell), so the chat makes way for it first.
    private func chatChord(_ id: String) {
        if id == "display.open" { present(.settings); return }
        guard let navigation, navigation.viewControllers.count > 1, navigation.transitionCoordinator == nil, sheet == nil else { return }
        pendingChord = id
        returnToRoot()
    }

    /// The native New terminal sheet at Chat (it also offers terminals and the orchestrators): a chat it made opens over the page,
    /// a terminal it made becomes the page's tab, a global orchestrator it opened shows the Global screen.
    private func openNewChat(project: String) {
        guard newTab == nil, model.projectID == project, let form = NewTerminalSheetModel(model: model) else { return }
        if model.chatsOffered { form.select(kind: .chat) }
        let chat = model.selectedChatID, session = model.sessionID, global = model.globalScreenRequests
        let finish: () -> Void = { [weak self] in self?.newTabEnded(project: project, chat: chat, session: session, global: global) }
        form.dismiss = { [weak self] in self?.dismissSheet(finish) }
        newTab = form
        guard presentSheet(NewTerminalSheet(sheet: form), canDismiss: { !form.busy }) else { newTab = nil; return }
        afterSheet = finish
    }
    /// On iPad (`PadShell`, a window 600 points and wider) a chat made there opens in its tab on the page (`riwork.chat`); on iPhone
    /// it opens over the page.
    private func newTabEnded(project: String, chat: String?, session: String?, global: Int) {
        guard model.projectID == project else { return }
        if model.globalScreenRequests != global { onGlobalRequested?(); return }
        if let made = model.selectedChatID, made != chat {
            if padLayout, let info = model.project(withID: project) { onProjectPage?(info, "chat:\(made)", false) }
            else { openChat(project: project, chat: made) }
            return
        }
        if let made = model.sessionID, made != session, let info = model.project(withID: project) { onProjectPage?(info, "shell:\(made)", false) }
    }

    private func openGlobal() {
        let key = "global:\(model.selectedDesktopID ?? "")"
        guard key != workspaceKey else { return }
        showWorkspace(NSHMobileGlobalWorkspace(model: model, back: { [weak self] in self?.pop() }, projects: { [weak self] in
            self?.onProjects?()
            self?.returnToRoot()
        }, newTerminal: { [weak self] project in
            self?.onProjectPage?(project, nil, true)
            self?.returnToRoot()
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
                guard let self, self.workspaceKey?.hasPrefix("chat:") == true else { return }
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

    /// The pages are laid out for iPad: `PadShell` takes a window 600 points and wider.
    private var padLayout: Bool { (root?.viewIfLoaded?.bounds.width ?? 0) >= 600 }

    private func openFiles(_ files: [ChatFileEntry], selected: ChatFileEntry) {
        guard let workspace, navigation?.topViewController === workspace else { return }
        pushFiles(files, selected: selected)
    }
    /// A file preview of the chat drawn on the page (`riwork.chat`): pushed over the pages, as the native chat's are over it; Back
    /// returns to the page.
    func openFilesOverPage(_ files: [ChatFileEntry], selected: ChatFileEntry) {
        guard let root, navigation?.topViewController === root, sheet == nil else { return }
        pushFiles(files, selected: selected)
        publishVisibility()
    }
    private func pushFiles(_ files: [ChatFileEntry], selected: ChatFileEntry) {
        guard let navigation, navigation.presentedViewController == nil,
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
        newTab = nil
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
        // After the bridge has said no native screen is up, so the page takes it.
        let chord = pendingChord
        pendingChord = nil
        if let chord, viewController === root { onChord?(chord) }
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

/// A project's chat, over its TSX page (`nav.present` `chat`): the native transcript, composer, approvals and worker cards, with Back
/// and the chat's menu. The model follows the project and puts the chat up as a dashboard row's tab request does; the project's lists
/// are read while it is up (its worker cards follow them), and leaving it puts the terminal back for the page.
private struct NSHMobileChatWorkspace: View {
    @Bindable var model: RemoteModel
    let projectID: String
    let chatID: String
    let back: () -> Void
    let projects: () -> Void
    /// A project chord (`app_chords.ts` id) for the page under the chat.
    let chord: (String) -> Void
    @Environment(\.desktopStyle) private var style
    @State private var notices = 0
    @State private var settled = false
    @State private var shortcutOwner = UUID()

    private var chat: ChatInfo? { model.selectedChat.flatMap { $0.id == chatID ? $0 : nil } }
    private var title: String {
        guard let chat else { return "Chat" }
        return model.orchestrator(ofChat: chat.id)?.title ?? ChatTabs.title(model.chatConversations[chat.id]?.transcript.info ?? chat)
    }
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: title, back: back) {
                if let chat {
                    Menu { ChatMenuSection(model: model, chat: chat, showNotices: { notices += 1 }) } label: {
                        Label("More options", systemImage: "ellipsis").labelStyle(.iconOnly)
                            .frame(width: style.target, height: style.target).contentShape(Rectangle())
                    }.buttonStyle(.plain)
                }
            }
            if let chat {
                ChatScreen(model: model, chat: chat, openNotices: notices).id(chat.id)
            } else {
                VStack(spacing: 12) {
                    if settled {
                        Text("This chat is no longer listed on the Mac.").foregroundStyle(style.muted)
                        Button("Back", action: back).buttonStyle(DesktopButtonStyle(prominent: true))
                    } else { ProgressView() }
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(style.background.ignoresSafeArea())
        .overlay { TabShortcutGuide(controller: model.keyboard.tabShortcuts) }
        .background {
            Group {
                Button("Back", action: back).keyboardShortcut("p", modifiers: [.command, .shift])
                // The project's other chords, as over a chat tab of the native project screen (⌘⇧P stays Back to the page). ⌘B is not
                // one of them: the sidebar it toggles is the iPad page's, where a chat draws in its tab instead.
                ForEach(NSHMobileBridge.projectChords.filter { $0.id != "nav.back" && $0.id != NSHMobileBridge.sidebarChord }, id: \.id) { entry in
                    Button(entry.title) { chord(entry.id) }
                        .keyboardShortcut(KeyEquivalent(Character(entry.input)), modifiers: entry.eventModifiers)
                }
            }
            .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
        }
        .task(id: chatID) {
            await model.chooseProject(projectID)
            if model.selectedChatID != chatID { model.selectChat(chatID) }
            settled = true
        }
        .task(id: projectID) { await model.keepFresh(.sessions) }
        .onAppear {
            model.keyboard.tabShortcuts.setAppHandler(owner: shortcutOwner) { action in
                switch action {
                case .projects: projects()
                case .modelPicker: if model.selectedChat != nil { model.keyboard.requestModelPicker() }
                case .selectTab(let number): chord("tab.select.\(number)")
                case .newTerminal: chord("terminal.new")
                default: break
                }
            }
        }
        .onDisappear {
            model.keyboard.tabShortcuts.clearAppHandler(owner: shortcutOwner)
            if model.selectedChatID == chatID { model.deselectChat() }
        }
    }
}

private extension NSHAppChord {
    var eventModifiers: EventModifiers {
        var result: EventModifiers = []
        if modifiers.contains(.command) { result.insert(.command) }
        if modifiers.contains(.shift) { result.insert(.shift) }
        if modifiers.contains(.alternate) { result.insert(.option) }
        if modifiers.contains(.control) { result.insert(.control) }
        return result
    }
}

/// The project memo editor as a sheet of its own: it goes when the editor closes (Done), as the native project screen's sheet did.
private struct NSHMobileMemoSheet: View {
    @Bindable var model: RemoteModel
    let closed: () -> Void
    var body: some View {
        ProjectMemoSheet(model: model).onChange(of: model.memoEditor == nil) { _, gone in if gone { closed() } }
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
                    if let gateway = current.pairing.gateway {
                        Label("via \(gateway.gatewayName)", systemImage: "network").foregroundStyle(style.muted)
                        Text("This desktop is reached through the gateway's saved SSH connection. Manage that connection on the gateway Mac.")
                    } else {
                        Label(current.pairing.relayHost, systemImage: "network").foregroundStyle(style.muted)
                        Button("Rename desktop", systemImage: "pencil") { renaming = true }.disabled(busy)
                        Button("Remove from this device", systemImage: "trash", role: .destructive) { confirmingRemoval = true }.disabled(busy)
                    }
                }.listRowBackground(style.panel)
                if let failure { Section { Text(failure).foregroundStyle(style.error) }.listRowBackground(style.panel) }
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
