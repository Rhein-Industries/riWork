//! A small modal for the remote flows that need typed text: adding a host from a pairing
//! link, pairing another Mac to this one, and naming a project on a host.
//!
//! Pairing links are secrets. They are held only while the modal is open, never written to
//! disk or a log, never echoed into an error message, and shown cut short.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    AnyElement, Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle,
    IntoElement, KeyDownEvent, MouseButton, Pixels, Point, Render, UTF16Selection, Window, div,
    prelude::*, rgb,
};

use crate::{
    project_settings::{Input, impl_input_handler, input_content},
    remote_hosts::{PairRequest, PairingLink},
    remote_service::Backend,
    remote_tree::validate_project_name,
    theme, ui_text, utf16_to_byte,
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
    /// Which field is being typed in; past the last field are CANCEL, then the action.
    active: usize,
    /// What the work in flight is doing; the modal ignores input meanwhile.
    busy: Option<&'static str>,
    error: Option<String>,
    /// A freshly made pairing link, on the result page of a pairing.
    link: Option<PairingLink>,
    copied: bool,
    focus: FocusHandle,
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
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_global::<theme::Appearance>(|_, cx| cx.notify())
            .detach();
        let fields = match &kind {
            PromptKind::AddHost => vec![
                Field::new("PAIRING LINK", "Paste the link (⌘V)", String::new(), true),
                Field::new(
                    "NAME (OPTIONAL)",
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
                Field::new("NAME OF THE OTHER MAC", "MacBook", name.clone(), false),
                Field::new(
                    "RELAY",
                    "wss://relay.example.com/v1/ws",
                    relay.clone(),
                    false,
                ),
                Field::new(
                    "RELAY ROUTES FILE",
                    "relay-routes.json",
                    routes.to_string_lossy().into_owned(),
                    false,
                ),
            ],
            PromptKind::NewProject { .. } => {
                vec![Field::new("PROJECT NAME", "My App", String::new(), false)]
            }
        };
        // A pairing starts on the first field that is still empty.
        let active = fields
            .iter()
            .position(|field| field.input.text.is_empty())
            .unwrap_or(0);
        Self {
            kind,
            backend,
            fields,
            active,
            busy: None,
            error: None,
            link: None,
            copied: false,
            focus: cx.focus_handle(),
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
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

    fn input(&self) -> &Input {
        &self.fields[self.active.min(self.fields.len() - 1)].input
    }

    fn input_mut(&mut self) -> &mut Input {
        let index = self.active.min(self.fields.len() - 1);
        &mut self.fields[index].input
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

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let handled = match event.keystroke.key.as_str() {
            "escape" => {
                if self.busy.is_none() {
                    cx.emit(RemotePromptEvent::Closed);
                }
                true
            }
            "enter" | "return" => {
                match self.button_at(self.active) {
                    Some(button) => self.press(button, cx),
                    None => self.submit(cx),
                }
                true
            }
            "space" if !self.accepts_input() => {
                if let Some(button) = self.button_at(self.active) {
                    self.press(button, cx);
                }
                true
            }
            "tab" => {
                let slots = self.slot_count();
                let step = if event.keystroke.modifiers.shift {
                    slots - 1
                } else {
                    1
                };
                self.active = (self.active + step) % slots;
                true
            }
            _ => {
                let handled = self.accepts_input() && self.input_mut().key(event, cx);
                if handled {
                    self.edited(cx);
                }
                handled
            }
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn title(&self) -> String {
        match &self.kind {
            PromptKind::AddHost => "ADD HOST".to_owned(),
            PromptKind::PairMac { .. } => "PAIR ANOTHER MAC".to_owned(),
            PromptKind::NewProject { host_label, .. } => format!("NEW PROJECT ON {host_label}"),
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
            PromptKind::AddHost => "ADD HOST  ↵",
            PromptKind::PairMac { .. } => "MAKE LINK  ↵",
            PromptKind::NewProject { .. } => "CREATE  ↵",
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
        div()
            .id(id)
            .px(ui_text::space(12.0))
            .py(ui_text::space(8.0))
            .cursor_pointer()
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
            .child(label)
            .on_click(cx.listener(move |prompt, _, _, cx| prompt.press(press, cx)))
            .into_any_element()
    }
}

impl Render for RemotePrompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let focused = self.focus.is_focused(window);
        let entity = cx.entity();
        let fields = self
            .fields
            .iter()
            .enumerate()
            .filter(|_| self.link.is_none())
            .map(|(index, field)| {
                let active = self.active == index && focused && self.busy.is_none();
                let shown = if field.secret {
                    let text = masked(&field.input.text);
                    let end = text.len();
                    Input {
                        text,
                        selection: end..end,
                        ..Input::default()
                    }
                } else {
                    Input {
                        text: field.input.text.clone(),
                        selection: field.input.selection.clone(),
                        reversed: field.input.reversed,
                        marked: field.input.marked.clone(),
                    }
                };
                div()
                    .id(("remote-prompt-field", index))
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(4.0))
                    .child(
                        div()
                            .text_size(ui_text::text(9.0))
                            .text_color(rgb(colors.muted))
                            .child(field.label),
                    )
                    .child(input_content(
                        &shown,
                        active,
                        field.placeholder,
                        &self.focus,
                        entity.clone(),
                        colors,
                    ))
                    .on_click(cx.listener(move |prompt, _, window, cx| {
                        if prompt.busy.is_none() {
                            prompt.active = index;
                            prompt.focus.focus(window, cx);
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
            (if self.copied { "COPIED" } else { "COPY LINK" }, "DONE  ↵")
        } else {
            ("CANCEL", self.action_label())
        };
        let base = self.field_slots();
        div()
            .id("remote-prompt")
            .occlude()
            .track_focus(&self.focus)
            .key_context("RemotePrompt")
            .on_key_down(cx.listener(Self::key_down))
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
            )
    }
}

impl_input_handler!(RemotePrompt);

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
