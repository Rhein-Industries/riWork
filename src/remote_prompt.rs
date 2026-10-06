//! A small modal for the remote flows that need typed text: adding a host from a pairing
//! link, pairing another Mac to this one, and naming a project on a host.
//!
//! Pairing links are secrets. They are held only while the modal is open, never written to
//! disk or a log, never echoed into an error message, and displayed through a masked library input.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::text_input::{self, EnterBehavior, InputEvent, InputState};
use gpui::{
    AnyElement, ClipboardItem, Context, EventEmitter, FocusHandle, IntoElement, KeyDownEvent,
    MouseButton, Render, Window, div, prelude::*, rgb,
};
use gpui::{Entity, Focusable, Subscription};

use crate::{
    behavior_controls as behavior,
    project_settings::Input,
    remote_hosts::{PairRequest, PairingLink},
    remote_service::Backend,
    remote_tree::validate_project_name,
    theme, ui_text,
};

/// What the modal is for.
pub enum PromptKind {
    /// Paste a pairing link made on the other Mac.
    AddHost,
    /// Make a link that lets another Mac control this one.
    PairMac {
        relay: String,
        name: String,
        routes: PathBuf,
    },
    /// Name a project to create on a host.
    NewProject { host_id: String, host_label: String },
}

pub enum RemotePromptEvent {
    /// Closed without doing anything, or after the pairing link was shown.
    Closed,
    /// A host was paired; the registry has a new entry.
    HostAdded { label: String },
    /// A project name to send to the host. The caller sends it once.
    NewProject { host_id: String, name: String },
}

/// One line of text the modal asks for.
struct Field {
    label: &'static str,
    placeholder: &'static str,
    input: Input,
    /// Shown as a count instead of its text.
    secret: bool,
}

impl Field {
    fn new(label: &'static str, placeholder: &'static str, text: String, secret: bool) -> Self {
        Self {
            label,
            placeholder,
            input: Input::new(text),
            secret,
        }
    }
}

pub struct RemotePrompt {
    kind: PromptKind,
    backend: Result<Arc<Backend>, String>,
    fields: Vec<Field>,
    input_states: Vec<Entity<InputState>>,
    _input_subscriptions: Vec<Subscription>,
    /// Which field is being typed in; past the last field are CANCEL, then the action.
    active: usize,
    /// What the work in flight is doing; the modal ignores input meanwhile.
    busy: Option<&'static str>,
    error: Option<String>,
    /// A freshly made pairing link, on the result page of a pairing.
    link: Option<PairingLink>,
    copied: bool,
    focus: FocusHandle,
    dialog: gpui_kit::base::DialogHandle,
    return_focus: Option<FocusHandle>,
    button_focus: [FocusHandle; 2],
    #[cfg(test)]
    parent_enter_actions: usize,
    #[cfg(test)]
    parent_enter_keys: usize,
}

impl EventEmitter<RemotePromptEvent> for RemotePrompt {}

/// The relay's route manifest `pair` appends to, unless the person picks another.
pub fn default_routes_file(home: &Path) -> PathBuf {
    home.join("remote").join("relay-routes.json")
}

/// A pasted link, as far as can be told without redeeming it. Nothing of the text goes into
/// the message.
fn check_link(text: &str) -> Result<String, String> {
    let link = text.trim();
    if link.is_empty() {
        return Err("Paste the pairing link from the other Mac.".to_owned());
    }
    if !link.starts_with("riwork://pair?") || link.contains(char::is_whitespace) {
        return Err("That is not a RiWork pairing link.".to_owned());
    }
    Ok(link.to_owned())
}

/// A secret link for the screen: its start is not secret, its length is a useful check that
/// the whole link arrived, and the rest stays out of sight.
fn masked(link: &str) -> String {
    if link.is_empty() {
        return String::new();
    }
    let head = link.chars().take(16).collect::<String>();
    format!("{head}…  ({} characters)", link.chars().count())
}

impl RemotePrompt {
    pub fn new(
        kind: PromptKind,
        backend: Result<Arc<Backend>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let return_focus = window.focused(cx);
        cx.observe_global::<theme::Appearance>(|_, cx| cx.notify())
            .detach();
        let fields = match &kind {
            PromptKind::AddHost => vec![
                Field::new("Pairing link", "Paste the link (⌘V)", String::new(), true),
                Field::new(
                    "Name (optional)",
                    "How this Mac is shown here",
                    String::new(),
                    false,
                ),
            ],
            PromptKind::PairMac {
                relay,
                name,
                routes,
            } => vec![
                Field::new("Name of the other Mac", "MacBook", name.clone(), false),
                Field::new(
                    "Relay",
                    "wss://relay.example.com/v1/ws",
                    relay.clone(),
                    false,
                ),
                Field::new(
                    "Relay routes file",
                    "relay-routes.json",
                    routes.to_string_lossy().into_owned(),
                    false,
                ),
            ],
            PromptKind::NewProject { .. } => {
                vec![Field::new("Project name", "My App", String::new(), false)]
            }
        };
        // A pairing starts on the first field that is still empty.
        let active = fields
            .iter()
            .position(|field| field.input.text.is_empty())
            .unwrap_or(0);
        let input_states: Vec<_> = fields
            .iter()
            .map(|field| {
                let state = text_input::single_line(
                    field.input.text.clone(),
                    field.placeholder,
                    window,
                    cx,
                );
                state.update(cx, |input, cx| input.set_masked(field.secret, window, cx));
                state
            })
            .collect();
        let subscriptions = input_states
            .iter()
            .enumerate()
            .map(|(index, state)| {
                cx.subscribe_in(state, window, move |prompt, state, event, _, cx| {
                    if prompt.busy.is_some() || prompt.link.is_some() {
                        return;
                    }
                    match event {
                        InputEvent::Change => {
                            prompt.fields[index].input.text = state.read(cx).value().to_string();
                            prompt.edited(cx);
                        }
                        InputEvent::Focus => {
                            prompt.active = index;
                            cx.notify();
                        }
                        _ if text_input::is_submit(event, EnterBehavior::Submit) => {
                            prompt.submit(cx)
                        }
                        _ => {}
                    }
                })
            })
            .collect();
        Self {
            kind,
            backend,
            fields,
            input_states,
            _input_subscriptions: subscriptions,
            active,
            busy: None,
            error: None,
            link: None,
            copied: false,
            focus: cx.focus_handle(),
            dialog: gpui_kit::base::DialogHandle::new(true),
            return_focus,
            button_focus: [cx.focus_handle(), cx.focus_handle()],
            #[cfg(test)]
            parent_enter_actions: 0,
            #[cfg(test)]
            parent_enter_keys: 0,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.accepts_input() {
            self.input_states[self.active]
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        } else if let Some(button) = self.button_at(self.active).filter(|button| *button < 2) {
            self.button_focus[button].focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
    }

    /// The relay and name of a pairing, to start the next one from. Neither is secret.
    pub fn pair_defaults(&self) -> Option<(String, String)> {
        matches!(self.kind, PromptKind::PairMac { .. }).then(|| (self.text(1), self.text(0)))
    }

    /// How many fields are on screen; the result page of a pairing has none.
    fn field_slots(&self) -> usize {
        if self.link.is_some() {
            0
        } else {
            self.fields.len()
        }
    }

    /// Tab visits the fields, then the two buttons.
    fn slot_count(&self) -> usize {
        self.field_slots() + 2
    }

    /// Which of the two buttons `slot` is, if it is one.
    fn button_at(&self, slot: usize) -> Option<usize> {
        slot.checked_sub(self.field_slots())
    }

    /// Typing goes to a field, and only while no work is running.
    fn accepts_input(&self) -> bool {
        self.busy.is_none() && self.active < self.field_slots()
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    fn text(&self, index: usize) -> String {
        self.fields[index].input.text.trim().to_owned()
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.link.is_some() {
            return;
        }
        for (field, state) in self.fields.iter_mut().zip(&self.input_states) {
            field.input.text = state.read(cx).value().to_string();
        }
        self.error = None;
        match &self.kind {
            PromptKind::NewProject { host_id, .. } => match validate_project_name(&self.text(0)) {
                Ok(name) => cx.emit(RemotePromptEvent::NewProject {
                    host_id: host_id.clone(),
                    name,
                }),
                Err(error) => self.error = Some(error),
            },
            PromptKind::AddHost => self.add_host(cx),
            PromptKind::PairMac { .. } => self.pair(cx),
        }
        cx.notify();
    }

    fn add_host(&mut self, cx: &mut Context<Self>) {
        let link = match check_link(&self.fields[0].input.text) {
            Ok(link) => link,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let label = self.text(1);
        let backend = match &self.backend {
            Ok(backend) => backend.clone(),
            Err(error) => {
                self.error = Some(error.clone());
                return;
            }
        };
        self.busy = Some("Pairing…");
        let shown_label = label.clone();
        let work = cx
            .background_executor()
            .spawn(async move { backend.add_host(&link, &label) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |prompt, cx| {
                prompt.busy = None;
                match result {
                    // The link is spent whether or not it is kept; it is never shown again.
                    Ok(()) => cx.emit(RemotePromptEvent::HostAdded { label: shown_label }),
                    Err(error) => prompt.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pair(&mut self, cx: &mut Context<Self>) {
        let (name, relay, routes) = (self.text(0), self.text(1), self.text(2));
        if name.is_empty() || relay.is_empty() || routes.is_empty() {
            self.error = Some("Fill in the name, the relay and the routes file.".to_owned());
            return;
        }
        let backend = match &self.backend {
            Ok(backend) => backend.clone(),
            Err(error) => {
                self.error = Some(error.clone());
                return;
            }
        };
        self.busy = Some("Making a pairing link…");
        let request = PairRequest {
            name,
            relay,
            routes: PathBuf::from(routes),
        };
        let work = cx
            .background_executor()
            .spawn(async move { backend.pair_desktop(&request) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |prompt, cx| {
                prompt.busy = None;
                match result {
                    Ok(link) => {
                        prompt.link = Some(link);
                        // The page now has COPY and DONE; COPY is the likely next step.
                        prompt.active = 0;
                    }
                    Err(error) => prompt.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_link(&mut self, cx: &mut Context<Self>) {
        if let Some(link) = &self.link {
            cx.write_to_clipboard(ClipboardItem::new_string(link.expose().to_owned()));
            self.copied = true;
            cx.notify();
        }
    }

    /// The first button cancels, or copies on the result page; the second does the work,
    /// or closes the result page.
    fn press(&mut self, button: usize, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        match (self.link.is_some(), button) {
            (false, 0) => cx.emit(RemotePromptEvent::Closed),
            (false, _) => self.submit(cx),
            (true, 0) => self.copy_link(cx),
            (true, _) => cx.emit(RemotePromptEvent::Closed),
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self
            .button_focus
            .iter()
            .position(|focus| focus.is_focused(window))
        {
            self.active = self.field_slots() + button;
        }

        #[cfg(test)]
        if matches!(event.keystroke.key.as_str(), "enter" | "return") {
            self.parent_enter_keys += 1;
        }
        if self.accepts_input()
            && matches!(event.keystroke.key.as_str(), "escape" | "tab")
            && crate::form_input::is_composing(&self.input_states[self.active], window, cx)
        {
            return;
        }
        if self.busy.is_some() {
            cx.stop_propagation();
            return;
        }
        let handled = match event.keystroke.key.as_str() {
            "escape" => {
                crate::project_settings::close_modal(
                    &self.dialog,
                    &self.focus,
                    &self.return_focus,
                    window,
                    cx,
                );
                if self.busy.is_none() {
                    cx.emit(RemotePromptEvent::Closed);
                }
                true
            }
            "tab" => {
                crate::project_settings::modal_tab(event.keystroke.modifiers.shift, window, cx);
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn title(&self) -> String {
        match &self.kind {
            PromptKind::AddHost => ui_text::cased("Add host").to_string(),
            PromptKind::PairMac { .. } => ui_text::cased("Pair another Mac").to_string(),
            PromptKind::NewProject { host_label, .. } => {
                format!("{} {host_label}", ui_text::cased("New project on"))
            }
        }
    }

    fn description(&self) -> &'static str {
        match (&self.kind, self.link.is_some()) {
            (PromptKind::AddHost, _) => {
                "On the other Mac, open Settings → Remote → Pair another Mac, and paste the link it shows."
            }
            (PromptKind::PairMac { .. }, false) => {
                "Makes a one-time link. Whoever has it can control this Mac's terminals once it is redeemed."
            }
            (PromptKind::PairMac { .. }, true) => {
                "Paste it on the other Mac under Settings → Remote → Add host. It works once, for about ten minutes, and is not saved here. Add the new route to your relay's routes file and restart the relay before using it."
            }
            (PromptKind::NewProject { .. }, _) => {
                "The project is made in the host's default projects folder."
            }
        }
    }

    fn action_label(&self) -> &'static str {
        match &self.kind {
            PromptKind::AddHost => "Add host  ↵",
            PromptKind::PairMac { .. } => "Make link  ↵",
            PromptKind::NewProject { .. } => "Create  ↵",
        }
    }

    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        primary: bool,
        focused: bool,
        press: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let disabled = self.busy.is_some();
        behavior::button_content(id, label, ui_text::cased(label))
            .disabled(disabled)
            .track_focus(&self.button_focus[press])
            .focus_visible(move |style| style.border_color(rgb(colors.focus)))
            .px(ui_text::space(12.0))
            .py(ui_text::space(8.0))
            .border_1()
            .border_color(rgb(if focused {
                colors.focus
            } else if primary {
                colors.cyan
            } else {
                colors.panel
            }))
            .bg(rgb(if primary {
                colors.panel_active
            } else {
                colors.panel
            }))
            .text_color(rgb(if disabled {
                colors.muted
            } else if primary {
                colors.cyan
            } else {
                colors.muted
            }))
            .on_click(cx.listener(move |prompt, _, window, cx| {
                let closing = prompt.busy.is_none()
                    && ((prompt.link.is_none() && press == 0)
                        || (prompt.link.is_some() && press == 1));
                if closing {
                    crate::project_settings::close_modal(
                        &prompt.dialog,
                        &prompt.focus,
                        &prompt.return_focus,
                        window,
                        cx,
                    );
                }
                prompt.press(press, cx);
            }))
            .into_any_element()
    }
}

impl Render for RemotePrompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        // The result intentionally replaces the editor. Only transfer focus if that removed editor still owned it.
        if self.link.is_some()
            && self
                .input_states
                .iter()
                .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
        {
            self.button_focus[0].focus(window, cx);
        }
        let focused = self.focus.contains_focused(window, cx);
        let fields = self
            .fields
            .iter()
            .enumerate()
            .filter(|_| self.link.is_none())
            .map(|(index, field)| {
                div()
                    .id(("remote-prompt-field", index))
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(4.0))
                    .child(
                        div()
                            .text_size(ui_text::text(9.0))
                            .text_color(rgb(colors.muted))
                            .child(ui_text::cased(field.label)),
                    )
                    .child(crate::form_input::frame(
                        ("remote-input", index),
                        &self.input_states[index],
                        self.busy.is_some(),
                        window,
                        cx,
                    ))
                    .on_click(cx.listener(move |prompt, _, window, cx| {
                        if prompt.busy.is_none() {
                            prompt.active = index;
                            prompt.focus(window, cx);
                            cx.notify();
                        }
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let result = self.link.as_ref().map(|link| {
            div()
                .p(ui_text::space(10.0))
                .bg(rgb(colors.bg))
                .border_1()
                .border_color(rgb(colors.divider))
                .text_color(rgb(colors.cyan))
                .overflow_hidden()
                .text_ellipsis()
                .child(masked(link.expose()))
        });
        let (first, second) = if self.link.is_some() {
            (if self.copied { "Copied" } else { "Copy link" }, "Done  ↵")
        } else {
            ("Cancel", self.action_label())
        };
        let base = self.field_slots();
        let panel = div()
            .id("remote-prompt")
            .occlude()
            .key_context("RemotePrompt")
            .capture_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .w(ui_text::space(480.0))
            .max_w_full()
            .max_h(gpui::relative(0.9))
            .overflow_y_scroll()
            .p(ui_text::space(18.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(14.0))
            .font_family(ui_text::ui_family())
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.magenta))
            .text_color(rgb(colors.text))
            .text_size(ui_text::text(11.0))
            .child(div().text_color(rgb(colors.cyan)).child(self.title()))
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(10.0))
                    .child(self.description()),
            )
            .children(fields)
            .children(result)
            .children(
                self.busy
                    .map(|busy| div().text_color(rgb(colors.cyan)).child(busy)),
            )
            .children(
                self.error
                    .as_ref()
                    .map(|error| div().text_color(rgb(colors.gold)).child(error.clone())),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(ui_text::space(10.0))
                    .child(self.button(
                        "remote-prompt-first",
                        first,
                        false,
                        focused && self.active == base,
                        0,
                        cx,
                    ))
                    .child(self.button(
                        "remote-prompt-second",
                        second,
                        true,
                        focused && self.active == base + 1,
                        1,
                        cx,
                    )),
            );
        #[cfg(test)]
        let panel = panel.on_action(cx.listener(
            |prompt, _: &gpui_kit::base::input::Enter, _, _| {
                prompt.parent_enter_actions += 1;
            },
        ));
        gpui_kit::base::Dialog::new(cx)
            .handle(self.dialog.clone())
            .focus_handle(self.focus.clone())
            .close_on_escape(false)
            .close_on_backdrop_press(false)
            .on_ok(|_, _, _| false)
            .on_cancel(|_, _, _| false)
            .popup(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_pairing_link_is_accepted_and_the_text_never_comes_back_in_an_error() {
        let link = "riwork://pair?v=2&data=SECRETSECRET";
        assert_eq!(check_link(&format!("  {link}\n")), Ok(link.to_owned()));
        for bad in [
            "",
            "   ",
            "https://example.com/pair?v=2",
            "riwork://other?v=2",
            "riwork://pair?v=2 and more",
            "SECRETSECRET",
        ] {
            let error = check_link(bad).unwrap_err();
            assert!(!error.contains("SECRETSECRET"), "{error}");
            assert!(!error.contains("riwork://"), "{error}");
        }
    }

    #[test]
    fn a_link_on_screen_shows_its_start_and_its_length_but_not_the_rest() {
        let link = "riwork://pair?v=2&data=SECRETSECRETSECRETSECRET";
        let shown = masked(link);
        assert!(shown.starts_with("riwork://pair?v"), "{shown}");
        assert!(shown.contains(&format!("{} characters", link.chars().count())));
        assert!(!shown.contains("SECRET") && !shown.contains("data="));
        assert_eq!(masked(""), "");
    }

    #[test]
    fn pairing_starts_from_the_relay_routes_file_in_the_remote_state_folder() {
        assert_eq!(
            default_routes_file(Path::new("/Users/me/.local/share/riwork")),
            PathBuf::from("/Users/me/.local/share/riwork/remote/relay-routes.json")
        );
    }
}

#[cfg(test)]
mod kit_form_tests {
    use super::*;
    use gpui::{ElementInputHandler, InputHandler, TestAppContext};
    use gpui_kit::test::TestWindowExt;
    use std::{cell::RefCell, rc::Rc};

    #[gpui::test]
    fn kit_project_name_has_one_submit_and_preserves_composition(cx: &mut TestAppContext) {
        let (handle, prompt) = crate::form_input::test_window(cx, |window, cx| {
            RemotePrompt::new(
                PromptKind::NewProject {
                    host_id: "inert-host".into(),
                    host_label: "Fixture".into(),
                },
                Err("inert backend".into()),
                window,
                cx,
            )
        });
        let names = Rc::new(RefCell::new(Vec::new()));
        let observed = names.clone();
        let _subscription = cx.update(|app| {
            app.subscribe(&prompt, move |_, event, _| {
                if let RemotePromptEvent::NewProject { name, .. } = event {
                    observed.borrow_mut().push(name.clone());
                }
            })
        });
        let state = cx
            .update_window(handle.into(), |_, window, app| {
                window.render_frame(app);
                window.click(("remote-input", 0usize), app);
                window.input("Draft", app);
                prompt.read(app).input_states[0].clone()
            })
            .unwrap();
        cx.run_until_parked();
        let identity = state.entity_id();
        cx.update_window(handle.into(), |_, window, app| {
            state.update(app, |state, cx| state.set_selected_range(0..5, cx));
            let mut handler = ElementInputHandler::new(
                window.find(("remote-input", 0usize)).bounds(),
                state.clone(),
            );
            handler.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            window.render_frame(app);
            window.press("enter", app);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(names.borrow().is_empty());
        cx.update_window(handle.into(), |_, window, app| {
            let mut handler = ElementInputHandler::new(
                window.find(("remote-input", 0usize)).bounds(),
                state.clone(),
            );
            handler.replace_text_in_range(None, "日本語", window, app);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(names.borrow().is_empty());
        cx.update_window(handle.into(), |_, window, app| {
            window.render_frame(app);
            window.press("enter", app);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(names.borrow().as_slice(), &["日本語"]);
        cx.update_window(handle.into(), |_, window, app| {
            assert_eq!(prompt.read(app).parent_enter_actions, 0);
            assert_eq!(prompt.read(app).parent_enter_keys, 0);
            prompt.update(app, |_, cx| cx.notify());
            window.render_frame(app);
            assert_eq!(prompt.read(app).input_states[0].entity_id(), identity);
            assert_eq!(state.read(app).value(), "日本語");
        })
        .unwrap();
    }

    #[gpui::test]
    fn kit_pairing_secret_is_masked_and_busy_keeps_its_draft(cx: &mut TestAppContext) {
        let (handle, prompt) = crate::form_input::test_window(cx, |window, cx| {
            RemotePrompt::new(PromptKind::AddHost, Err("inert backend".into()), window, cx)
        });
        let state = cx
            .update_window(handle.into(), |_, window, app| {
                window.render_frame(app);
                window.click(("remote-input", 0usize), app);
                window.input("riwork://pair?synthetic-secret", app);
                prompt.read(app).input_states[0].clone()
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert!(state.read(app).presentation().is_masked());
            window.press("cmd-a", app);
            app.write_to_clipboard(ClipboardItem::new_string("synthetic-sentinel".into()));
            window.press("cmd-c", app);
            window.press("cmd-x", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, _, app| {
            assert_eq!(
                app.read_from_clipboard().unwrap().text().as_deref(),
                Some("synthetic-sentinel")
            );
            assert_eq!(state.read(app).value(), "riwork://pair?synthetic-secret");
        })
        .unwrap();
        cx.update_window(handle.into(), |_, window, app| {
            window.render_frame(app);
            window.press("tab", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert!(
                prompt.read(app).input_states[1]
                    .read(app)
                    .focus_handle(app)
                    .is_focused(window)
            );
            window.render_frame(app);
            window.press("shift-tab", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert!(state.read(app).focus_handle(app).is_focused(window));
            prompt.update(app, |prompt, cx| {
                prompt.busy = Some("Inert pending");
                cx.notify();
            });
            window.render_frame(app);
            window.input("ignored", app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, app| {
            assert_eq!(state.read(app).value(), "riwork://pair?synthetic-secret");
            prompt.update(app, |prompt, cx| {
                prompt.busy = None;
                cx.notify();
            });
            window.render_frame(app);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, _, app| {
            assert_eq!(state.read(app).value(), "riwork://pair?synthetic-secret")
        })
        .unwrap();
    }

    #[gpui::test]
    fn kit_pair_fields_retain_supplied_defaults(cx: &mut TestAppContext) {
        let (handle, prompt) = crate::form_input::test_window(cx, |window, cx| {
            RemotePrompt::new(
                PromptKind::PairMac {
                    name: "Fixture Mac".into(),
                    relay: "wss://fixture.invalid/ws".into(),
                    routes: PathBuf::from("/inert/relay-routes.json"),
                },
                Err("inert backend".into()),
                window,
                cx,
            )
        });
        let states = cx
            .update_window(handle.into(), |_, window, app| {
                let states = prompt.read(app).input_states.clone();
                prompt.update(app, |_, cx| cx.notify());
                window.render_frame(app);
                states
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, _, app| {
            assert_eq!(states.len(), 3);
            assert_eq!(states[0].read(app).value(), "Fixture Mac");
            assert_eq!(states[1].read(app).value(), "wss://fixture.invalid/ws");
            assert_eq!(states[2].read(app).value(), "/inert/relay-routes.json");
            assert_eq!(
                prompt.read(app).input_states[2].entity_id(),
                states[2].entity_id()
            );
        })
        .unwrap();
    }
}

#[cfg(test)]
mod kit_control_tests {
    use super::*;
    use crate::form_input::{test_turn, test_window};
    use gpui::TestAppContext;
    use gpui_kit::test::TestWindowExt;

    #[gpui::test]
    fn remote_dialog_base_cancel_enter_and_space_emit_once(cx: &mut TestAppContext) {
        let (window, owner) = test_window(cx, |_, cx| ReturnFixture {
            invoker: cx.focus_handle(),
            prompt: None,
            subscription: None,
            closes: 0,
        });
        for (index, key) in ["enter", "space"].into_iter().enumerate() {
            test_turn(cx, window, |window, app| {
                window.click("open-inert-modal", app)
            });
            test_turn(cx, window, |window, app| {
                let prompt = owner.read(app).prompt.clone().unwrap();
                prompt.read(app).button_focus[0].focus(window, app);
            });
            test_turn(cx, window, |window, _| {
                let cancel = window.find("remote-prompt-first");
                assert_eq!(cancel.role(), Some(gpui::Role::Button));
                assert_eq!(cancel.label(), Some("Cancel"));
                assert_eq!(cancel.focused(), Some(true));
            });
            test_turn(cx, window, |window, app| window.press(key, app));
            assert_eq!(
                owner.read_with(cx, |owner, _| owner.closes),
                index + 1,
                "one domain close per Base {key} activation"
            );
        }
    }

    #[gpui::test]
    fn remote_dialog_tab_traps_focus_and_busy_buttons_preserve_draft(cx: &mut TestAppContext) {
        let (window, prompt) = test_window(cx, |window, cx| {
            RemotePrompt::new(
                PromptKind::NewProject {
                    host_id: "inert-host".into(),
                    host_label: "Fixture".into(),
                },
                Err("inert backend".into()),
                window,
                cx,
            )
        });
        test_turn(cx, window, |window, app| {
            prompt.read(app).button_focus[1].focus(window, app);
            window.press("tab", app);
        });
        test_turn(cx, window, |window, app| {
            let input = &prompt.read(app).input_states[0];
            assert!(input.read(app).focus_handle(app).is_focused(window));
            window.input("Synthetic draft", app);
        });
        test_turn(cx, window, |_, app| {
            prompt.update(app, |prompt, cx| {
                prompt.busy = Some("Synthetic pending");
                cx.notify();
            })
        });
        test_turn(cx, window, |window, app| {
            window.click("remote-prompt-second", app)
        });
        test_turn(cx, window, |window, app| {
            window.click("remote-prompt-first", app)
        });
        test_turn(cx, window, |_, app| {
            let prompt = prompt.read(app);
            assert_eq!(prompt.input_states[0].read(app).value(), "Synthetic draft");
            assert_eq!(prompt.busy, Some("Synthetic pending"));
            assert!(prompt.error.is_none());
            assert_eq!(prompt.parent_enter_actions, 0);
        });
    }

    struct ReturnFixture {
        invoker: gpui::FocusHandle,
        prompt: Option<Entity<RemotePrompt>>,
        subscription: Option<gpui::Subscription>,
        closes: usize,
    }
    impl Render for ReturnFixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(
                    behavior::button_content(
                        "open-inert-modal",
                        "Open synthetic form",
                        "Open synthetic form",
                    )
                    .track_focus(&self.invoker)
                    .on_click(cx.listener(|owner, _, window, cx| {
                        let prompt = cx.new(|cx| {
                            RemotePrompt::new(
                                PromptKind::NewProject {
                                    host_id: "inert-host".into(),
                                    host_label: "Fixture".into(),
                                },
                                Err("inert backend".into()),
                                window,
                                cx,
                            )
                        });
                        owner.subscription =
                            Some(cx.subscribe_in(&prompt, window, |owner, _, event, _, cx| {
                                if matches!(event, RemotePromptEvent::Closed) {
                                    owner.closes += 1;
                                    owner.prompt = None;
                                    cx.notify();
                                }
                            }));
                        prompt.update(cx, |prompt, cx| prompt.focus(window, cx));
                        owner.prompt = Some(prompt);
                        cx.notify();
                    })),
                )
                .children(self.prompt.clone())
        }
    }
    #[gpui::test]
    fn remote_dialog_cancel_restores_invoker_after_subscribed_close(cx: &mut TestAppContext) {
        let (window, owner) = test_window(cx, |_, cx| ReturnFixture {
            invoker: cx.focus_handle(),
            prompt: None,
            subscription: None,
            closes: 0,
        });
        test_turn(cx, window, |window, app| {
            window.click("open-inert-modal", app)
        });
        test_turn(cx, window, |window, app| {
            let prompt = owner.read(app).prompt.clone().unwrap();
            prompt.read(app).button_focus[0].focus(window, app);
            window.press("space", app);
        });
        test_turn(cx, window, |window, app| {
            assert_eq!(owner.read(app).closes, 1);
            assert!(owner.read(app).prompt.is_none());
            assert!(owner.read(app).invoker.is_focused(window));
            assert_eq!(window.find("open-inert-modal").focused(), Some(true));
        });
    }
}
