//! Explicit new-chat choices. Reading saved model metadata never contacts the host.

use crate::{
    behavior_controls as behavior,
    chat::{
        log,
        model::{ApprovalMode, ChatInfo, ModelOption, NewChat, Provider},
    },
    codex_accounts, form_input, sessions,
    text_input::{self, InputEvent, InputState},
    theme, ui_text,
};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, IntoElement, Render, Subscription,
    Window, div, prelude::*, rgb,
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub worktree_id: Option<String>,
    pub label: String,
    pub path: PathBuf,
}

/// Confirmation may outlive its captured project/worktree snapshot.
pub fn context_matches(request: &NewChat, project: &str, locations: &[Location]) -> bool {
    request.project_id.as_deref() == Some(project)
        && locations.iter().any(|location| {
            location.worktree_id == request.worktree_id && location.path == request.cwd
        })
}

#[derive(Default)]
struct Models {
    supported: Vec<ModelOption>,
    configured: Vec<String>,
    account: Option<String>,
    account_label: Option<String>,
    error: Option<String>,
}

fn eligible(info: &ChatInfo, provider: Provider, account: Option<&str>) -> bool {
    info.provider == provider
        && (provider == Provider::Claude || info.codex_account_id.as_deref() == account)
}

fn decode_models(id: &str, line: &[u8]) -> Option<Vec<ModelOption>> {
    if line.last() != Some(&b'\n') {
        return None;
    }
    let envelope: crate::chat::wire::Envelope = serde_json::from_slice(line).ok()?;
    if envelope.chat_id != id {
        return None;
    }
    match envelope.event {
        crate::chat::model::ChatEvent::Models { models } => Some(models),
        _ => None,
    }
}

/// Read only a bounded log tail, excluding incomplete lines and foreign UUIDs.
/// Opening a ChatLog would repair/write its tail; this reader never does that.
fn model_metadata(home: &Path, id: &str) -> Option<Vec<ModelOption>> {
    let dir = log::chat_dir(home, id)?;
    let mut file = File::open(dir.join("events.jsonl")).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(2 * 1024 * 1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = BufReader::new(file.take(length - start));
    if start > 0 {
        reader.skip_until(b'\n').ok()?;
    }
    let mut latest = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 || line.last() != Some(&b'\n') {
            break;
        }
        if let Some(models) = decode_models(id, &line) {
            latest = Some(models);
        }
    }
    latest
}

/// Use the newest saved driver list for this provider/account, and label other
/// saved model IDs as previously configured. Never claim they are supported.
fn saved_models(home: &Path, project: &str, provider: Provider) -> Models {
    let binding = match provider {
        Provider::Codex => match sessions::selected_codex_binding(home, Some(project)) {
            Ok(binding) => Some(binding),
            Err(error) => {
                return Models {
                    error: Some(error),
                    ..Default::default()
                };
            }
        },
        Provider::Claude => None,
    };
    let mut result = Models {
        account: binding.as_ref().and_then(|binding| binding.id.clone()),
        account_label: binding
            .map(|binding| binding.label.unwrap_or_else(|| "System default".into())),
        ..Default::default()
    };
    let mut found_metadata = false;
    let infos = log::read_infos(home);
    for info in infos
        .iter()
        .rev()
        .filter(|info| eligible(info, provider, result.account.as_deref()))
        .take(12)
    {
        if let Some(id) = &info.model
            && valid_model(id)
            && !result.configured.contains(id)
        {
            result.configured.push(id.clone());
        }
        if !found_metadata && let Some(models) = model_metadata(home, &info.id) {
            found_metadata = true;
            result.supported = models
                .into_iter()
                .filter(|model| valid_model(&model.id))
                .collect();
        }
    }
    result
        .configured
        .retain(|id| !result.supported.iter().any(|model| &model.id == id));
    result
}

fn valid_model(id: &str) -> bool {
    !id.trim().is_empty() && id.chars().count() <= 100 && !id.chars().any(char::is_control)
}

/// Main/unlocked routing is independent of the Sessions filter and focus mode.
/// If all panes are locked, refuse placement before creating a host session.
pub fn destination(
    panes: impl IntoIterator<Item = u64>,
    main: Option<u64>,
    selected: u64,
    locked: &dyn Fn(u64) -> bool,
) -> Option<u64> {
    let panes: Vec<_> = panes.into_iter().collect();
    main.filter(|id| panes.contains(id) && !locked(*id))
        .or_else(|| (panes.contains(&selected) && !locked(selected)).then_some(selected))
        .or_else(|| panes.into_iter().find(|id| !locked(*id)))
}

/// A single-use gate: only explicit confirmation yields a creation request.
/// Render/search/provider selection never consume it; cancel invalidates it.
#[derive(Default)]
struct Confirmation(bool);
impl Confirmation {
    fn take(&mut self, request: NewChat) -> Option<NewChat> {
        if self.0 {
            return None;
        }
        self.0 = true;
        Some(request)
    }
}

pub enum ChoiceEvent {
    Closed,
    Confirmed(NewChat),
}

pub struct ChatChoice {
    project: String,
    locations: Vec<Location>,
    location: usize,
    provider: Provider,
    mode: ApprovalMode,
    effort: Option<String>,
    fast: bool,
    models: [Models; 2],
    loading: bool,
    error: Option<String>,
    confirmation: Confirmation,
    model: Entity<InputState>,
    search: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
    focus: FocusHandle,
    dialog: gpui_kit::base::DialogHandle,
    return_focus: Option<FocusHandle>,
    controls: BTreeMap<String, FocusHandle>,
}
impl EventEmitter<ChoiceEvent> for ChatChoice {}

fn provider_index(provider: Provider) -> usize {
    usize::from(provider == Provider::Claude)
}
fn provider_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
    }
}

const MODES: [(ApprovalMode, &str); 4] = [
    (ApprovalMode::Supervised, "Supervised"),
    (ApprovalMode::AutoEdit, "Auto-edit"),
    (ApprovalMode::Full, "Full · unrestricted"),
    (ApprovalMode::Plan, "Plan"),
];

impl ChatChoice {
    pub fn new(
        project: String,
        home: PathBuf,
        locations: Vec<Location>,
        selected_worktree: Option<&str>,
        provider: Option<Provider>,
        unrestricted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let choice = Self::draft(
            project.clone(),
            locations,
            selected_worktree,
            provider,
            unrestricted,
            window,
            cx,
        );
        let metadata_project = project.clone();
        let work = cx.background_executor().spawn(async move {
            [
                saved_models(&home, &metadata_project, Provider::Codex),
                saved_models(&home, &metadata_project, Provider::Claude),
            ]
        });
        cx.spawn(async move |this, cx| {
            let models = work.await;
            let _ = this.update(cx, |choice, cx| {
                choice.apply_models(models, cx);
                cx.notify();
            });
        })
        .detach();
        choice
    }

    /// Construct retained controls without I/O; fixtures use this same draft owner.
    fn draft(
        project: String,
        locations: Vec<Location>,
        selected_worktree: Option<&str>,
        provider: Option<Provider>,
        unrestricted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = text_input::single_line("", "Provider default, or type a model ID", window, cx);
        let search = text_input::single_line("", "Search saved models", window, cx);
        let subscriptions = [&model, &search]
            .into_iter()
            .map(|input| cx.subscribe(input, |_, _, _: &InputEvent, cx| cx.notify()))
            .collect();
        let location = locations
            .iter()
            .position(|location| location.worktree_id.as_deref() == selected_worktree)
            .unwrap_or(0);
        let mut controls = BTreeMap::new();
        for key in [
            "provider-0",
            "provider-1",
            "mode-0",
            "mode-1",
            "mode-2",
            "mode-3",
            "model-default",
            "effort-default",
            "fast",
            "cancel",
            "confirm",
        ] {
            controls.insert(key.to_owned(), cx.focus_handle());
        }
        for index in 0..locations.len() {
            controls.insert(format!("location-{index}"), cx.focus_handle());
        }
        cx.observe_global::<theme::Appearance>(|_, cx| cx.notify())
            .detach();
        Self {
            project,
            locations,
            location,
            provider: provider.unwrap_or(Provider::Codex),
            mode: if unrestricted {
                ApprovalMode::Full
            } else {
                ApprovalMode::Supervised
            },
            effort: None,
            fast: false,
            models: Default::default(),
            loading: true,
            error: None,
            confirmation: Default::default(),
            model,
            search,
            _subscriptions: subscriptions,
            focus: cx.focus_handle(),
            dialog: gpui_kit::base::DialogHandle::new(true),
            return_focus: window.focused(cx),
            controls,
        }
    }

    fn apply_models(&mut self, models: [Models; 2], cx: &mut Context<Self>) {
        self.models = models;
        self.loading = false;
        for (provider, models) in self.models.iter().enumerate() {
            for (index, model) in models.supported.iter().enumerate() {
                self.controls
                    .insert(format!("model-{provider}-{index}"), cx.focus_handle());
                for effort in &model.efforts {
                    self.controls
                        .entry(format!("effort-{effort}"))
                        .or_insert_with(|| cx.focus_handle());
                }
            }
            for index in 0..models.configured.len() {
                self.controls
                    .insert(format!("configured-{provider}-{index}"), cx.focus_handle());
            }
        }
    }

    fn selected_model(&self, cx: &Context<Self>) -> Option<&ModelOption> {
        let value = self.model.read(cx).value();
        let models = &self.models[provider_index(self.provider)].supported;
        if value.trim().is_empty() {
            models.iter().find(|model| model.is_default)
        } else {
            models.iter().find(|model| model.id == value.trim())
        }
    }

    fn pick_model(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        form_input::set_value(&self.model, id, window, cx);
        self.effort = None;
        self.fast = false;
        self.error = None;
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirmation.0 = true;
        crate::project_settings::close_modal(
            &self.dialog,
            &self.focus,
            &self.return_focus,
            window,
            cx,
        );
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.confirmation.0 {
            return;
        }
        if form_input::is_composing(&self.model, window, cx)
            || form_input::is_composing(&self.search, window, cx)
        {
            return;
        }
        let models = &self.models[provider_index(self.provider)];
        if let Some(error) = &models.error {
            self.error = Some(error.clone());
            cx.notify();
            return;
        }
        let id = self.model.read(cx).value().trim().to_owned();
        if !id.is_empty() && !valid_model(&id) {
            self.error =
                Some("Use a model ID of at most 100 characters without control characters.".into());
            cx.notify();
            return;
        }
        let Some(location) = self.locations.get(self.location) else {
            return;
        };
        let selected = self.selected_model(cx);
        let effort = self
            .effort
            .clone()
            .filter(|effort| selected.is_some_and(|model| model.efforts.contains(effort)));
        let fast = self.fast && selected.is_some_and(|model| model.supports_fast);
        let request = NewChat {
            provider: self.provider,
            project_id: Some(self.project.clone()),
            worktree_id: location.worktree_id.clone(),
            cwd: location.path.clone(),
            codex_account_id: (self.provider == Provider::Codex).then(|| {
                models
                    .account
                    .clone()
                    .unwrap_or_else(|| codex_accounts::SYSTEM_DEFAULT_ID.into())
            }),
            title: None,
            approval_mode: self.mode,
            model: (!id.is_empty()).then_some(id),
            effort,
            fast,
            orchestrator: None,
        };
        if let Some(request) = self.confirmation.take(request) {
            self.close(window, cx);
            cx.emit(ChoiceEvent::Confirmed(request));
        }
    }

    fn button(
        &self,
        key: String,
        label: String,
        selected: bool,
        disabled: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let id = format!("new-chat-{key}");
        form_input::control_element(
            id.clone(),
            behavior::button(
                id,
                label,
                if selected {
                    crate::controls::Button::Primary
                } else {
                    crate::controls::Button::Secondary
                },
                colors,
            )
            .disabled(disabled)
            .max_w_full()
            .track_focus(&self.controls[&key])
            .on_click(cx.listener(move |choice, _, window, cx| action(choice, window, cx))),
        )
    }

    fn group(label: &'static str, children: Vec<AnyElement>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(ui_text::space(6.0))
            .child(div().child(label))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(ui_text::space(6.0))
                    .children(children),
            )
            .into_any_element()
    }
}

impl Render for ChatChoice {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let index = provider_index(self.provider);
        let blocked = self.loading || self.models[index].error.is_some() || self.confirmation.0;
        let providers = [Provider::Codex, Provider::Claude]
            .into_iter()
            .enumerate()
            .map(|(index, provider)| {
                self.button(
                    format!("provider-{index}"),
                    provider_label(provider).into(),
                    self.provider == provider,
                    false,
                    cx,
                    move |choice, window, cx| {
                        if choice.provider != provider {
                            choice.provider = provider;
                            choice.pick_model(String::new(), window, cx);
                            form_input::set_value(&choice.search, String::new(), window, cx);
                        }
                    },
                )
            })
            .collect();
        let modes = MODES
            .into_iter()
            .enumerate()
            .map(|(index, (mode, label))| {
                self.button(
                    format!("mode-{index}"),
                    label.into(),
                    self.mode == mode,
                    false,
                    cx,
                    move |choice, _, cx| {
                        choice.mode = mode;
                        cx.notify();
                    },
                )
            })
            .collect();
        let locations = self
            .locations
            .iter()
            .enumerate()
            .map(|(index, location)| {
                self.button(
                    format!("location-{index}"),
                    location.label.clone(),
                    self.location == index,
                    false,
                    cx,
                    move |choice, _, cx| {
                        choice.location = index;
                        cx.notify();
                    },
                )
            })
            .collect();
        let query = self.search.read(cx).value().trim().to_lowercase();
        let current = self.model.read(cx).value().trim().to_owned();
        let mut model_rows = vec![self.button(
            "model-default".into(),
            "Provider default".into(),
            current.is_empty(),
            false,
            cx,
            |choice, window, cx| choice.pick_model(String::new(), window, cx),
        )];
        for (row, model) in self.models[index].supported.iter().enumerate() {
            if !format!("{} {} {}", model.name, model.id, model.description)
                .to_lowercase()
                .contains(&query)
            {
                continue;
            }
            let id = model.id.clone();
            model_rows.push(self.button(
                format!("model-{index}-{row}"),
                format!(
                    "{}{}",
                    model.name,
                    if model.is_default { " · default" } else { "" }
                ),
                current == id,
                false,
                cx,
                move |choice, window, cx| choice.pick_model(id.clone(), window, cx),
            ));
        }
        for (row, id) in self.models[index].configured.iter().enumerate() {
            if !id.to_lowercase().contains(&query) {
                continue;
            }
            let id = id.clone();
            model_rows.push(self.button(
                format!("configured-{index}-{row}"),
                format!("{id} · previously configured"),
                current == id,
                false,
                cx,
                move |choice, window, cx| choice.pick_model(id.clone(), window, cx),
            ));
        }
        let selected = self.selected_model(cx);
        let efforts = selected
            .map(|model| model.efforts.clone())
            .unwrap_or_default();
        let supports_fast = selected.is_some_and(|model| model.supports_fast);
        let mut effort_rows = Vec::new();
        if !efforts.is_empty() {
            effort_rows.push(self.button(
                "effort-default".into(),
                "Default effort".into(),
                self.effort.is_none(),
                false,
                cx,
                |choice, _, cx| {
                    choice.effort = None;
                    cx.notify();
                },
            ));
            for effort in efforts {
                effort_rows.push(self.button(
                    format!("effort-{effort}"),
                    effort.clone(),
                    self.effort.as_ref() == Some(&effort),
                    false,
                    cx,
                    move |choice, _, cx| {
                        choice.effort = Some(effort.clone());
                        cx.notify();
                    },
                ));
            }
        }
        let note = if self.loading {
            "Reading saved model choices…"
        } else if self.models[index].supported.is_empty() {
            "No saved supported-model list. Use provider default or type an ID; refresh models in the chat."
        } else {
            "Models from saved provider metadata. Availability is checked when the chat starts."
        };
        let panel = div()
            .id("new-chat-dialog")
            .role(gpui::Role::Dialog)
            .aria_label("New chat")
            .w(ui_text::space(560.0))
            .max_w_full()
            .max_h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(ui_text::space(14.0))
            .p(ui_text::space(20.0))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.divider))
            .rounded(ui_text::space(6.0))
            .text_color(rgb(colors.text))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(12.0))
            .on_key_down(
                cx.listener(|choice, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        if form_input::is_composing(&choice.model, window, cx)
                            || form_input::is_composing(&choice.search, window, cx)
                        {
                            return;
                        }
                        choice.close(window, cx);
                        cx.emit(ChoiceEvent::Closed);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(div().text_size(ui_text::text(18.0)).child("New chat"))
            .child(Self::group("Provider", providers))
            .children(self.models[index].account_label.as_ref().map(|label| {
                div()
                    .text_color(rgb(colors.muted))
                    .child(format!("Codex account · {label}"))
            }))
            .child(Self::group("Worktree", locations))
            .children(self.locations.get(self.location).map(|location| {
                div()
                    .text_color(rgb(colors.muted))
                    .child(location.path.to_string_lossy().into_owned())
            }))
            .child(Self::group("Permissions", modes))
            .child(div().text_color(rgb(colors.muted)).child(match self.mode {
                ApprovalMode::Supervised => "Asks before commands and edits",
                ApprovalMode::AutoEdit => "Edits the workspace freely, asks for the rest",
                ApprovalMode::Full => "Never asks before acting",
                ApprovalMode::Plan => "Plans first and changes nothing",
            }))
            .child(form_input::frame(
                "new-chat-model-search",
                &self.search,
                false,
                window,
                cx,
            ))
            .child(
                div()
                    .id("new-chat-model-list")
                    .max_h(ui_text::space(170.0))
                    .overflow_y_scroll()
                    .child(Self::group("Model", model_rows)),
            )
            .child(form_input::frame(
                "new-chat-model-input",
                &self.model,
                false,
                window,
                cx,
            ))
            .child(div().text_color(rgb(colors.muted)).child(note))
            .children((!effort_rows.is_empty()).then(|| Self::group("Reasoning", effort_rows)))
            .children(supports_fast.then(|| {
                self.button(
                    "fast".into(),
                    if self.fast { "Fast on" } else { "Fast off" }.into(),
                    self.fast,
                    false,
                    cx,
                    |choice, _, cx| {
                        choice.fast = !choice.fast;
                        cx.notify();
                    },
                )
            }))
            .children(
                self.error
                    .as_ref()
                    .or(self.models[index].error.as_ref())
                    .map(|error| div().text_color(rgb(colors.gold)).child(error.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(ui_text::space(8.0))
                    .child(self.button(
                        "cancel".into(),
                        "Cancel".into(),
                        false,
                        false,
                        cx,
                        |choice, window, cx| {
                            choice.close(window, cx);
                            cx.emit(ChoiceEvent::Closed);
                        },
                    ))
                    .child(self.button(
                        "confirm".into(),
                        "Create chat".into(),
                        true,
                        blocked,
                        cx,
                        |choice, window, cx| choice.confirm(window, cx),
                    )),
            );
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
    use crate::form_input::test_turn;
    use gpui::{ElementInputHandler, InputHandler, TestAppContext};
    use gpui_kit::test::TestWindowExt;
    use std::{cell::RefCell, rc::Rc};

    // Same retained owner and Base controls as production, with metadata injected.
    // No Workspace, host, provider, socket, process or filesystem is constructed.
    fn fixture(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, Entity<ChatChoice>) {
        crate::form_input::test_window(cx, |window, cx| {
            let mut choice = ChatChoice::draft(
                "fixture-project".into(),
                vec![
                    Location {
                        worktree_id: None,
                        label: "Project root".into(),
                        path: "/fixture/root".into(),
                    },
                    Location {
                        worktree_id: Some("fixture-tree".into()),
                        label: "Worktree".into(),
                        path: "/fixture/tree".into(),
                    },
                ],
                Some("fixture-tree"),
                None,
                false,
                window,
                cx,
            );
            let models = |id: &str, name: &str| Models {
                supported: vec![ModelOption {
                    id: id.into(),
                    name: name.into(),
                    efforts: vec!["high".into()],
                    supports_fast: true,
                    is_default: true,
                    ..Default::default()
                }],
                ..Default::default()
            };
            choice.apply_models(
                [
                    models("codex-fixture", "Codex fixture"),
                    models("claude-fixture", "Claude fixture"),
                ],
                cx,
            );
            choice
        })
    }

    #[gpui::test]
    fn browsing_model_search_and_enter_never_create_and_confirmation_is_single_use(
        cx: &mut TestAppContext,
    ) {
        let (handle, choice) = fixture(cx);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let observed = requests.clone();
        let _subscription = cx.update(|app| {
            app.subscribe(&choice, move |_, event: &ChoiceEvent, _| {
                if let ChoiceEvent::Confirmed(request) = event {
                    observed.borrow_mut().push(request.clone());
                }
            })
        });
        let identities = choice.read_with(cx, |choice, _| {
            (choice.model.entity_id(), choice.search.entity_id())
        });
        test_turn(cx, handle, |window, app| {
            window.click("new-chat-provider-1", app);
            window.click("new-chat-model-search", app);
            window.input("Claude", app);
        });
        assert!(requests.borrow().is_empty());
        test_turn(cx, handle, |window, app| {
            assert!(
                window.try_find("new-chat-model-0-0").is_none(),
                "Codex models do not appear under Claude"
            );
            window.click("new-chat-model-1-0", app);
            window.click("new-chat-mode-2", app);
            window.click("new-chat-model-input", app);
            window.press("enter", app);
        });
        assert!(requests.borrow().is_empty());
        assert_eq!(
            choice.read_with(cx, |choice, _| (
                choice.model.entity_id(),
                choice.search.entity_id()
            )),
            identities
        );
        test_turn(cx, handle, |window, app| {
            let node = crate::form_input::test_ax_node(window, app, "new-chat-confirm");
            assert_eq!(node.role(), gpui::Role::Button);
            assert_eq!(node.label(), Some("Create chat"));
            // Invoke the actual confirmation callback twice to model queued duplicate activation.
            choice.update(app, |choice, cx| {
                choice.confirm(window, cx);
                choice.confirm(window, cx);
            });
        });
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.provider, Provider::Claude);
        assert_eq!(request.model.as_deref(), Some("claude-fixture"));
        assert_eq!(request.approval_mode, ApprovalMode::Full);
        assert_eq!(request.worktree_id.as_deref(), Some("fixture-tree"));
        assert_eq!(request.cwd, PathBuf::from("/fixture/tree"));
        assert_eq!(request.codex_account_id, None);
    }

    #[gpui::test]
    fn cancel_emits_no_creation_request(cx: &mut TestAppContext) {
        let (handle, choice) = fixture(cx);
        let events = Rc::new(RefCell::new((0usize, 0usize)));
        let observed = events.clone();
        let _subscription = cx.update(|app| {
            app.subscribe(&choice, move |_, event: &ChoiceEvent, _| match event {
                ChoiceEvent::Closed => observed.borrow_mut().0 += 1,
                ChoiceEvent::Confirmed(_) => observed.borrow_mut().1 += 1,
            })
        });
        test_turn(cx, handle, |window, app| {
            window.click("new-chat-model-input", app);
            window.input("typed-fixture", app);
            window.press("enter", app);
        });
        assert_eq!(*events.borrow(), (0, 0));
        test_turn(cx, handle, |window, app| window.press("escape", app));
        assert_eq!(*events.borrow(), (1, 0));
        test_turn(cx, handle, |window, app| {
            choice.update(app, |choice, cx| choice.confirm(window, cx));
        });
        assert_eq!(
            *events.borrow(),
            (1, 0),
            "a queued confirmation after cancellation is inert"
        );
    }

    #[gpui::test]
    fn typed_model_and_ime_preserve_draft_and_drop_unsupported_effort_and_fast(
        cx: &mut TestAppContext,
    ) {
        let (handle, choice) = fixture(cx);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let observed = requests.clone();
        let _subscription = cx.update(|app| {
            app.subscribe(&choice, move |_, event: &ChoiceEvent, _| {
                if let ChoiceEvent::Confirmed(request) = event {
                    observed.borrow_mut().push(request.clone());
                }
            })
        });
        test_turn(cx, handle, |window, app| {
            choice.update(app, |choice, _| {
                choice.effort = Some("high".into());
                choice.fast = true;
            });
            window.click("new-chat-model-input", app);
            let state = choice.read(app).model.clone();
            let mut handler =
                ElementInputHandler::new(window.find("new-chat-model-input").bounds(), state);
            handler.replace_and_mark_text_in_range(
                None,
                "typed-fixture",
                Some(13..13),
                window,
                app,
            );
            choice.update(app, |choice, cx| choice.confirm(window, cx));
        });
        assert!(requests.borrow().is_empty());
        test_turn(cx, handle, |window, app| {
            let state = choice.read(app).model.clone();
            let mut handler =
                ElementInputHandler::new(window.find("new-chat-model-input").bounds(), state);
            handler.unmark_text(window, app);
            choice.update(app, |choice, cx| choice.confirm(window, cx));
        });
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model.as_deref(), Some("typed-fixture"));
        assert_eq!(requests[0].effort, None);
        assert!(!requests[0].fast);
        assert_eq!(
            requests[0].codex_account_id.as_deref(),
            Some(codex_accounts::SYSTEM_DEFAULT_ID)
        );
    }

    #[test]
    fn saved_model_metadata_requires_the_exact_uuid_and_a_complete_line() {
        let id = uuid::Uuid::from_u128(0xabcdef).to_string();
        let other = uuid::Uuid::from_u128(2).to_string();
        let mut line = serde_json::to_vec(&crate::chat::wire::Envelope {
            chat_id: id.clone(),
            seq: 1,
            event: crate::chat::model::ChatEvent::Models {
                models: vec![ModelOption {
                    id: "fixture".into(),
                    name: "Fixture".into(),
                    ..Default::default()
                }],
            },
        })
        .unwrap();
        assert_eq!(decode_models(&id, &line), None);
        line.push(b'\n');
        assert_eq!(decode_models(&id, &line).unwrap()[0].id, "fixture");
        assert_eq!(decode_models(&other, &line), None);
        assert!(log::chat_dir(Path::new("/fixture"), "../escape").is_none());
        assert!(log::chat_dir(Path::new("/fixture"), &id.to_uppercase()).is_none());
    }

    #[test]
    fn confirmation_rejects_changed_project_removed_worktree_and_relocated_path() {
        let request: NewChat = serde_json::from_value(serde_json::json!({
            "provider":"claude", "project_id":"fixture-project", "worktree_id":"fixture-tree",
            "cwd":"/fixture/tree", "approval_mode":"supervised"
        }))
        .unwrap();
        let mut locations = vec![Location {
            worktree_id: Some("fixture-tree".into()),
            label: "Worktree".into(),
            path: "/fixture/tree".into(),
        }];
        assert!(context_matches(&request, "fixture-project", &locations));
        assert!(!context_matches(&request, "other-project", &locations));
        assert!(!context_matches(&request, "fixture-project", &[]));
        locations[0].path = "/fixture/relocated".into();
        assert!(!context_matches(&request, "fixture-project", &locations));
        let mut other_tree = request.clone();
        other_tree.worktree_id = Some("other-tree".into());
        assert!(!context_matches(&other_tree, "fixture-project", &locations));
    }
}
