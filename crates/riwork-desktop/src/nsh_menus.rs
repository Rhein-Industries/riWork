//! Process-owned menus for the AppKit host (contracts §2.4).
//!
//! The model uses `nsh/desktop`'s Rust types. NSH 0.3's AppKit menu builder is
//! private and binds its callbacks to one AppView, so the host renders this
//! model itself and resolves the key window at the time of an action.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ptr::NonNull,
    sync::Arc,
};

use block2::RcBlock;
use nsh_core::{
    desktop::{Key, MenuAction as DesktopMenuAction, MenuItem, MenuRole, Shortcut},
    keymap::When,
};
use nsh_platform_macos::{
    objc2::{
        DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
        rc::Retained,
        runtime::{AnyObject, Sel},
        sel,
    },
    objc2_app_kit::{
        NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSMenu, NSMenuItem,
    },
    objc2_foundation::{NSObject, NSObjectProtocol, NSString},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MenuAction {
    /// Captured before the callback is queued, so a later focus change cannot
    /// route an action to a different window.
    pub window_number: Option<isize>,
    pub command: &'static str,
}

pub(crate) struct DockWindow {
    pub window_number: isize,
    pub id: String,
    pub project: String,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DockItem {
    window_number: isize,
    label: String,
    frontmost: bool,
}

type Emit = Arc<dyn Fn(MenuAction) + Send + Sync>;

const COMMANDS: &[&str] = &[
    "openSettings",
    "openProjectSettings",
    "openSchedules",
    "quit",
    "openGlobalDashboard",
    "openGlobalChat",
    "newChat",
    "appPreview",
    "sessions",
    "defaultLayout",
    "gatherTabs",
    "toggleTranscriptMode",
    "expandTranscript",
    "collapseTranscript",
    "biggerText",
    "smallerText",
    "actualSize",
];

// The View-menu subset of acceptance §8. Keep every row and chord in place,
// but neither AppKit validation nor direct action delivery may activate it.
const DEFERRED_MENU_COMMANDS: &[&str] = &[
    "newChat",
    "appPreview",
    "sessions",
    "toggleTranscriptMode",
    "expandTranscript",
    "collapseTranscript",
];

pub(crate) fn app_command(command: &str) -> bool {
    matches!(
        command,
        "quit"
            | "openGlobalDashboard"
            | "openGlobalChat"
            | "biggerText"
            | "smallerText"
            | "actualSize"
    )
}

/// A queued menu action stays bound to the key window captured by AppKit.
/// Closing that window must never redirect the action into another workspace.
pub(crate) fn command_window(
    captured: Option<isize>,
    current_key: Option<isize>,
    live: &[isize],
) -> Option<isize> {
    captured
        .or(current_key)
        .filter(|number| live.contains(number))
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Emit]
    #[name = "RiWorkNshMenuTarget"]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        #[unsafe(method(riworkMenuItemChosen:))]
        fn chosen(&self, item: &NSMenuItem) {
            if let Some(command) = item.tag().checked_sub(1).and_then(|id| COMMANDS.get(id as usize)) {
                self.emit(command);
            }
        }

        #[unsafe(method(riworkDockWindowChosen:))]
        fn dock_window(&self, item: &NSMenuItem) {
            (self.ivars())(MenuAction {
                window_number: Some(item.tag()),
                command: "focusWindow",
            });
        }

        #[unsafe(method(riworkNewWindow:))]
        fn new_window(&self, _sender: Option<&AnyObject>) {
            self.emit("newWindow");
        }

        // Keep the standard selectors, but explicitly offer them to a native
        // text field / terminal first. NSH's own NshView consumes copy: and
        // selectAll: internally, so a nil target would prevent the transcript
        // from exporting its Rust-owned selection through host.on("command").
        #[unsafe(method(cut:))]
        fn cut(&self, sender: Option<&AnyObject>) {
            self.edit(sel!(cut:), "cut", sender);
        }
        #[unsafe(method(copy:))]
        fn copy(&self, sender: Option<&AnyObject>) {
            self.edit(sel!(copy:), "copy", sender);
        }
        #[unsafe(method(paste:))]
        fn paste(&self, sender: Option<&AnyObject>) {
            self.edit(sel!(paste:), "paste", sender);
        }
        #[unsafe(method(selectAll:))]
        fn select_all(&self, sender: Option<&AnyObject>) {
            self.edit(sel!(selectAll:), "selectAll", sender);
        }
    }
);

impl MenuTarget {
    fn emit(&self, command: &'static str) {
        if DEFERRED_MENU_COMMANDS.contains(&command) {
            return;
        }
        let app = NSApplication::sharedApplication(self.mtm());
        (self.ivars())(MenuAction {
            window_number: app.keyWindow().map(|window| window.windowNumber()),
            command,
        });
    }

    fn edit(&self, action: Sel, command: &'static str, sender: Option<&AnyObject>) {
        let app = NSApplication::sharedApplication(self.mtm());
        // SAFETY: these four selectors are standard AppKit responder actions.
        // Asking for the chain's target does not invoke this explicit menu
        // target; it is an NSObject outside the responder chain.
        if let Some(target) = unsafe { app.targetForAction_to_from(action, None, sender) } {
            if !is_nsh_view(&target) {
                // SAFETY: AppKit selected an object that answers this action.
                if unsafe { app.sendAction_to_from(action, Some(&target), sender) } {
                    return;
                }
            }
        }
        self.emit(command);
    }

    fn handle_key_equivalent(&self, event: &NSEvent) -> bool {
        let flags = event.modifierFlags();
        let key = event
            .charactersIgnoringModifiers()
            .map(|text| text.to_string());
        // NSH 0.3 has no Menu key in its parser. Route the dedicated PC
        // application key through the same per-window command channel.
        if dedicated_menu_key(event.keyCode(), key.as_deref(), flags) {
            let app = NSApplication::sharedApplication(self.mtm());
            let Some(window) = app.keyWindow() else {
                return false;
            };
            if event.window(self.mtm()).as_deref() != Some(&*window)
                || !window.firstResponder().is_some_and(|r| is_nsh_view(&r))
            {
                return false;
            }
            self.emit("tabContextMenu");
            return true;
        }
        if !flags.contains(NSEventModifierFlags::Command)
            || flags.intersects(NSEventModifierFlags::Control | NSEventModifierFlags::Option)
        {
            return false;
        }
        let shifted = flags.contains(NSEventModifierFlags::Shift);
        let Some(command) = key
            .as_deref()
            .and_then(|key| intercepted_command(key, shifted))
        else {
            return false;
        };
        let app = NSApplication::sharedApplication(self.mtm());
        let Some(window) = app.keyWindow() else {
            if app_command(command) {
                self.emit(command);
                return true;
            }
            return false;
        };
        if event.window(self.mtm()).as_deref() != Some(&*window) {
            return false;
        }
        if matches!(command, "biggerText" | "smallerText" | "actualSize") {
            if let Some(responder) = window.firstResponder() {
                // Native terminal views implement performKeyEquivalent for
                // their Ghostty font keys. Consume once if they handle it:
                // neither the UI-text menu nor an NSH keymap sees it again.
                if !is_nsh_view(&responder) && responder.performKeyEquivalent(event) {
                    return true;
                }
            }
        }
        self.emit(command);
        true
    }
}

fn dedicated_menu_key(code: u16, key: Option<&str>, flags: NSEventModifierFlags) -> bool {
    !flags.intersects(
        NSEventModifierFlags::Command
            | NSEventModifierFlags::Control
            | NSEventModifierFlags::Option
            | NSEventModifierFlags::Shift,
    ) && (code == 0x6e || key == Some("\u{f735}"))
}

fn is_nsh_view(object: &AnyObject) -> bool {
    let mut class = Some(object.class());
    while let Some(current) = class {
        if current.name().to_bytes() == b"NshView" {
            return true;
        }
        class = current.superclass();
    }
    false
}

fn intercepted_command(key: &str, shifted: bool) -> Option<&'static str> {
    match (key, shifted) {
        ("w" | "W", false) => Some("closeTab"),
        ("n" | "N", true) => Some("newProjectWindow"),
        ("=" | "+", _) => Some("biggerText"),
        ("-", false) => Some("smallerText"),
        ("0", false) => Some("actualSize"),
        _ => None,
    }
}

/// Retain for the lifetime of the host: NSMenuItem does not retain its target.
/// No delegate is installed; lifecycle and Dock menus remain host-owned.
pub(crate) struct HostMenus {
    _target: Retained<MenuTarget>,
    menu: Retained<NSMenu>,
    monitor: Option<Retained<AnyObject>>,
    dock: RefCell<Option<(Vec<DockItem>, Retained<NSMenu>)>>,
}

impl HostMenus {
    pub(crate) fn install(mtm: MainThreadMarker, emit: Emit) -> Self {
        let target = MenuTarget::alloc(mtm).set_ivars(emit);
        // SAFETY: NSObject's designated initializer on a newly allocated object.
        let target: Retained<MenuTarget> = unsafe { msg_send![super(target), init] };
        let menu = render_menu(mtm, "", &menu_model(), &target);
        NSApplication::sharedApplication(mtm).setMainMenu(Some(&menu));

        let key_target = target.clone();
        let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit supplies a live NSEvent for this callback.
            if key_target.handle_key_equivalent(unsafe { event.as_ref() }) {
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            }
        });
        // SAFETY: the block returns the supplied event or null, and the monitor
        // is removed before the retained target is released.
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
        };
        Self {
            _target: target,
            menu,
            monitor,
            dock: RefCell::new(None),
        }
    }

    /// AppKit asks the application delegate for this menu on demand. The host
    /// supplies its current windows and last key window, including when another
    /// application is frontmost. Closed-window menu actions remain harmless:
    /// the host resolves their exact native window number, never a fallback.
    pub(crate) fn dock_menu(
        &self,
        windows: &[DockWindow],
        frontmost: Option<isize>,
    ) -> Retained<NSMenu> {
        let items = dock_items(windows, frontmost);
        let mut cache = self.dock.borrow_mut();
        if let Some((previous, menu)) = &*cache {
            if previous == &items {
                return menu.clone();
            }
        }
        let mtm = self._target.mtm();
        let menu = NSMenu::new(mtm);
        for item in &items {
            let native = make_item(mtm, &item.label, Some(sel!(riworkDockWindowChosen:)), None);
            native.setTag(item.window_number);
            native.setState(if item.frontmost { 1 } else { 0 });
            // SAFETY: the retained target implements this selector.
            unsafe { native.setTarget(Some(&self._target)) };
            menu.addItem(&native);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let new_window = make_item(mtm, "New Window", Some(sel!(riworkNewWindow:)), None);
        // SAFETY: the retained target implements this selector.
        unsafe { new_window.setTarget(Some(&self._target)) };
        menu.addItem(&new_window);
        *cache = Some((items, menu.clone()));
        menu
    }
}

/// Keep GPUI's existing Dock label and ordering rules without linking GPUI.
fn dock_items(windows: &[DockWindow], frontmost: Option<isize>) -> Vec<DockItem> {
    fn clean(text: &str) -> Option<String> {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        (!text.is_empty()).then_some(text)
    }
    let mut rows = windows
        .iter()
        .map(|window| {
            (
                window,
                clean(&window.project).unwrap_or_else(|| "Untitled project".into()),
                window.branch.as_deref().and_then(clean),
            )
        })
        .collect::<Vec<_>>();
    rows.sort_by_cached_key(|(window, project, branch)| {
        (
            project.to_lowercase(),
            project.clone(),
            branch.clone().unwrap_or_default().to_lowercase(),
            window.id.clone(),
        )
    });
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (_, project, _) in &rows {
        *counts.entry(project.as_str()).or_default() += 1;
    }
    let mut taken = HashSet::new();
    rows.iter()
        .map(|(window, project, branch)| {
            let base = match branch {
                Some(branch) if counts[project.as_str()] > 1 => format!("{project} · {branch}"),
                _ => project.clone(),
            };
            let mut label = base.clone();
            let mut copy = 1;
            while taken.contains(&label) {
                copy += 1;
                label = format!("{base} ({copy})");
            }
            taken.insert(label.clone());
            DockItem {
                window_number: window.window_number,
                label,
                frontmost: frontmost == Some(window.window_number),
            }
        })
        .collect()
}

impl Drop for HostMenus {
    fn drop(&mut self) {
        if let Some(monitor) = self.monitor.take() {
            // SAFETY: this is the token AppKit returned when installing it.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        let app = NSApplication::sharedApplication(self._target.mtm());
        if app.mainMenu().as_deref() == Some(&*self.menu) {
            app.setMainMenu(None);
        }
    }
}

fn item(label: &str, command: &str, shortcut: Option<&str>) -> MenuItem {
    let id = COMMANDS
        .iter()
        .position(|candidate| *candidate == command)
        .unwrap() as u32
        + 1;
    MenuItem::Action(DesktopMenuAction {
        id,
        label: label.into(),
        shortcut: shortcut.map(|shortcut| Shortcut::parse(shortcut, true).unwrap()),
        enabled: !DEFERRED_MENU_COMMANDS.contains(&command),
        checked: None,
        role: None,
        when: When::Always,
    })
}

fn edit_item(label: &str, role: MenuRole) -> MenuItem {
    MenuItem::Action(DesktopMenuAction {
        id: 0,
        label: label.into(),
        shortcut: role.shortcut(true),
        enabled: true,
        checked: None,
        role: Some(role),
        when: When::Always,
    })
}

fn submenu(label: &str, items: Vec<MenuItem>) -> MenuItem {
    MenuItem::Submenu {
        label: label.into(),
        enabled: true,
        items,
    }
}

fn menu_model() -> Vec<MenuItem> {
    use MenuItem::Separator;
    vec![
        submenu(
            "RiWork",
            vec![
                item("Settings…", "openSettings", Some("cmd+,")),
                item(
                    "Project Settings…",
                    "openProjectSettings",
                    Some("cmd+alt+a"),
                ),
                item("Automations", "openSchedules", Some("cmd+shift+s")),
                Separator,
                item("Quit RiWork", "quit", Some("cmd+q")),
            ],
        ),
        submenu(
            "Edit",
            vec![
                edit_item("Cut", MenuRole::Cut),
                edit_item("Copy", MenuRole::Copy),
                edit_item("Paste", MenuRole::Paste),
                Separator,
                edit_item("Select All", MenuRole::SelectAll),
            ],
        ),
        submenu(
            "View",
            vec![
                item("Global Dashboard", "openGlobalDashboard", Some("cmd+alt+g")),
                item("Global Chat", "openGlobalChat", Some("cmd+shift+o")),
                Separator,
                item("New chat…", "newChat", Some("cmd+alt+shift+c")),
                item("App preview…", "appPreview", None),
                item("Sessions", "sessions", Some("cmd+alt+s")),
                item("Default Layout", "defaultLayout", Some("cmd+alt+l")),
                item(
                    "Gather Tabs into Main Pane",
                    "gatherTabs",
                    Some("cmd+shift+m"),
                ),
                Separator,
                item(
                    "Toggle Normal / Verbose",
                    "toggleTranscriptMode",
                    Some("cmd+shift+v"),
                ),
                item(
                    "Expand All Activity",
                    "expandTranscript",
                    Some("cmd+alt+right"),
                ),
                item(
                    "Collapse All Activity",
                    "collapseTranscript",
                    Some("cmd+alt+left"),
                ),
                Separator,
                item("Bigger Text", "biggerText", Some("cmd+=")),
                item("Smaller Text", "smallerText", Some("cmd+-")),
                item("Actual Size", "actualSize", Some("cmd+0")),
            ],
        ),
    ]
}

fn render_menu(
    mtm: MainThreadMarker,
    title: &str,
    items: &[MenuItem],
    target: &MenuTarget,
) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    // R-DEFER is application state, not responder-chain availability.
    menu.setAutoenablesItems(false);
    for item in items {
        match item {
            MenuItem::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            MenuItem::Submenu { label, items, .. } => {
                let holder = make_item(mtm, label, None, None);
                holder.setSubmenu(Some(&render_menu(mtm, label, items, target)));
                menu.addItem(&holder);
            }
            MenuItem::Action(action) => {
                let selector = match action.role {
                    Some(MenuRole::Cut) => sel!(cut:),
                    Some(MenuRole::Copy) => sel!(copy:),
                    Some(MenuRole::Paste) => sel!(paste:),
                    Some(MenuRole::SelectAll) => sel!(selectAll:),
                    _ => sel!(riworkMenuItemChosen:),
                };
                let ns = make_item(mtm, &action.label, Some(selector), action.shortcut.as_ref());
                ns.setTag(action.id as isize);
                ns.setEnabled(action.enabled);
                // SAFETY: target answers all selectors used here. HostMenus
                // retains it for as long as the application menu is installed.
                unsafe { ns.setTarget(Some(target)) };
                menu.addItem(&ns);
            }
        }
    }
    menu
}

fn make_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    shortcut: Option<&Shortcut>,
) -> Retained<NSMenuItem> {
    let (key, mask) = shortcut
        .map(key_equivalent)
        .unwrap_or_else(|| (String::new(), NSEventModifierFlags::empty()));
    // SAFETY: action is one of MenuTarget's selectors or absent for a submenu.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(&key),
        )
    };
    item.setKeyEquivalentModifierMask(mask);
    item
}

fn key_equivalent(shortcut: &Shortcut) -> (String, NSEventModifierFlags) {
    let key = match shortcut.key {
        Key::Char(character) => character.to_string(),
        Key::Left => "\u{f702}".into(),
        Key::Right => "\u{f703}".into(),
        _ => unreachable!("the host menu only uses character and arrow keys"),
    };
    let mut mask = NSEventModifierFlags::empty();
    for (set, flag) in [
        (shortcut.meta, NSEventModifierFlags::Command),
        (shortcut.ctrl, NSEventModifierFlags::Control),
        (shortcut.alt, NSEventModifierFlags::Option),
        (shortcut.shift, NSEventModifierFlags::Shift),
    ] {
        if set {
            mask |= flag;
        }
    }
    (key, mask)
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // Included in the harness=false AppKit test target.
    use super::*;

    #[test]
    fn dedicated_menu_key_routes_without_intercepting_modified_or_text_keys() {
        let none = NSEventModifierFlags::empty();
        assert!(dedicated_menu_key(0x6e, None, none));
        assert!(dedicated_menu_key(0, Some("\u{f735}"), none));
        assert!(!dedicated_menu_key(0, Some("m"), none));
        for modifier in [
            NSEventModifierFlags::Command,
            NSEventModifierFlags::Control,
            NSEventModifierFlags::Option,
            NSEventModifierFlags::Shift,
        ] {
            assert!(!dedicated_menu_key(0x6e, Some("\u{f735}"), modifier));
        }
    }

    #[test]
    fn command_ids_are_the_v1_set() {
        let mut commands = HashSet::new();
        MenuItem::each_action(&menu_model(), &mut |action| {
            if action.role.is_none() {
                let command = COMMANDS[action.id as usize - 1];
                assert!(commands.insert(command), "duplicate command: {command}");
            }
        });
        assert_eq!(
            commands,
            HashSet::from([
                "openSettings",
                "openProjectSettings",
                "openSchedules",
                "quit",
                "openGlobalDashboard",
                "openGlobalChat",
                "newChat",
                "appPreview",
                "sessions",
                "defaultLayout",
                "gatherTabs",
                "toggleTranscriptMode",
                "expandTranscript",
                "collapseTranscript",
                "biggerText",
                "smallerText",
                "actualSize",
            ])
        );
    }

    #[test]
    fn window_commands_go_to_the_key_window() {
        assert_eq!(command_window(Some(7), Some(9), &[7, 9]), Some(7));
        assert_eq!(command_window(None, Some(9), &[7, 9]), Some(9));
        assert_eq!(command_window(Some(7), Some(9), &[9]), None);
        assert_eq!(command_window(None, None, &[7, 9]), None);
    }

    #[test]
    fn app_commands_work_with_no_window() {
        for command in [
            "quit",
            "openGlobalDashboard",
            "openGlobalChat",
            "biggerText",
            "smallerText",
            "actualSize",
        ] {
            assert!(app_command(command));
        }
        for command in ["closeTab", "openSettings", "gatherTabs", "defaultLayout"] {
            assert!(!app_command(command));
        }
    }

    #[test]
    fn app_keymap_declaration_matches_native_menu_equivalents() {
        let source = include_str!("../../../apps/riwork/keyboard.ts");
        for (chord, command) in [
            ("alt+g", "openGlobalDashboard"),
            ("shift+o", "openGlobalChat"),
            ("q", "quit"),
            ("=", "biggerText"),
            ("-", "smallerText"),
            ("0", "actualSize"),
        ] {
            assert!(source.contains(&format!("[\"mod+{chord}\", \"{command}\"]")));
            let mut matches = 0;
            MenuItem::each_action(&menu_model(), &mut |action| {
                if action.role.is_none() && COMMANDS[action.id as usize - 1] == command {
                    assert_eq!(
                        action.shortcut,
                        Some(Shortcut::parse(&format!("cmd+{chord}"), true).unwrap())
                    );
                    matches += 1;
                }
            });
            assert_eq!(matches, 1, "{command} has exactly one native owner");
        }
    }

    #[test]
    fn routing() {
        window_commands_go_to_the_key_window();
        app_commands_work_with_no_window();
        assert!(
            DEFERRED_MENU_COMMANDS
                .iter()
                .all(|command| !app_command(command))
        );
    }

    #[test]
    fn deferred_menu_commands_remain_present_disabled_and_keep_their_chords() {
        let model = menu_model();
        let mut seen = Vec::new();
        MenuItem::each_action(&model, &mut |action| {
            if action.role.is_some() {
                return;
            }
            let command = COMMANDS[action.id as usize - 1];
            assert_eq!(action.enabled, !DEFERRED_MENU_COMMANDS.contains(&command));
            if !action.enabled {
                seen.push(command);
                assert!(action.shortcut.is_some() || command == "appPreview");
            }
        });
        assert_eq!(seen, DEFERRED_MENU_COMMANDS);
    }

    #[test]
    fn frozen_menu_has_only_riwork_edit_and_view_and_no_close_window_action() {
        let model = menu_model();
        let names = model
            .iter()
            .map(|menu| match menu {
                MenuItem::Submenu { label, .. } => label.as_str(),
                _ => panic!("top-level items must be submenus"),
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["RiWork", "Edit", "View"]);
        MenuItem::each_action(&model, &mut |action| {
            assert_ne!(action.role, Some(MenuRole::Close));
            assert_ne!(
                action.shortcut,
                Some(Shortcut::parse("cmd+w", true).unwrap())
            );
        });
    }

    #[test]
    fn close_tab_and_font_keys_preserve_their_modifier_boundaries() {
        assert_eq!(intercepted_command("w", false), Some("closeTab"));
        assert_eq!(intercepted_command("W", false), Some("closeTab"));
        assert_eq!(intercepted_command("w", true), None);
        assert_eq!(intercepted_command("N", true), Some("newProjectWindow"));
        assert_eq!(intercepted_command("n", false), None);
        assert_eq!(intercepted_command("=", true), Some("biggerText"));
        assert_eq!(intercepted_command("+", false), Some("biggerText"));
        assert_eq!(intercepted_command("-", false), Some("smallerText"));
        assert_eq!(intercepted_command("0", false), Some("actualSize"));
        assert_eq!(intercepted_command("0", true), None);
    }

    #[test]
    fn dock_labels_are_stable_unique_and_keep_the_last_key_window_marked() {
        let window = |number: isize, project: &str, branch: Option<&str>| DockWindow {
            window_number: number,
            id: format!("id-{number}"),
            project: project.into(),
            branch: branch.map(str::to_owned),
        };
        let mut windows = vec![
            window(4, "  ", None),
            window(2, " Alpha\nProject ", Some("main")),
            window(3, "zeta", None),
            window(1, "Alpha Project", Some("main")),
        ];
        let items = dock_items(&windows, Some(2));
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Alpha Project · main",
                "Alpha Project · main (2)",
                "Untitled project",
                "zeta"
            ]
        );
        assert!(items[1].frontmost);
        windows.reverse();
        assert_eq!(dock_items(&windows, Some(2)), items);
        windows.retain(|window| window.window_number != 2);
        assert!(
            dock_items(&windows, Some(2))
                .iter()
                .all(|item| !item.frontmost)
        );

        let colliding = [
            window(1, "Project", None),
            window(2, "Project", None),
            window(3, "Project (2)", None),
        ];
        let labels = dock_items(&colliding, None)
            .into_iter()
            .map(|item| item.label)
            .collect::<HashSet<_>>();
        assert_eq!(labels.len(), colliding.len());
    }
}
