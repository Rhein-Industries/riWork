//! Explicit new-chat choices: one list of every provider's models, where the model chosen
//! decides whether the chat is Codex's or Claude's. Reading saved model metadata
//! (`chat::catalog`) never contacts the host.

use crate::{
    behavior_controls as behavior,
    chat::{
        catalog::{self, Catalog as Models},
        model::{ApprovalMode, ModelOption, NewChat, Provider},
    },
    codex_accounts, controls, form_input,
    text_input::{self, InputEvent, InputState},
    theme, ui_text,
};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    Subscription, Window, div, prelude::*, rgb,
};
use std::{collections::BTreeMap, path::PathBuf};

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
                catalog::saved(&home, Some(&metadata_project), Provider::Codex),
                catalog::saved(&home, Some(&metadata_project), Provider::Claude),
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
        // Base Dialog registers its trap during layout; it does not autofocus.
        // Capture the invoker before moving focus to a retained chooser input.
        let return_focus = window.focused(cx);
        let model = text_input::single_line("", "Or type a model ID", window, cx);
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
            "model-default-0",
            "model-default-1",
            "mode-0",
            "mode-1",
            "mode-2",
            "mode-3",
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
        let entry_focus = search.read(cx).focus_handle(cx);
        entry_focus.focus(window, cx);
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
            return_focus,
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

    /// Choose a model of `provider`; an empty `id` is the provider's own default. The
    /// provider follows the model, and a typed id is the chosen provider's.
    fn pick_model(
        &mut self,
        provider: Provider,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.provider = provider;
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
        if !id.is_empty() && !catalog::valid_model(&id) {
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

    // Preserve the existing painted-button geometry for actual choice controls.
    // Radio/Toggle own their checked semantics and activation; this adds only styles.
    fn choice_style<E: gpui::Styled + gpui::InteractiveElement>(
        control: E,
        selected: bool,
        colors: theme::Palette,
    ) -> E {
        let kind = if selected {
            controls::Button::Primary
        } else {
            controls::Button::Secondary
        };
        let control = control
            .flex()
            .items_center()
            .justify_center()
            .line_height(gpui::relative(1.0))
            .px(ui_text::space(8.0))
            .py(ui_text::space(4.0))
            .border_1()
            .border_color(rgb(colors.divider))
            .rounded(ui_text::space(3.0))
            .bg(rgb(if selected { colors.cyan } else { colors.panel }))
            .text_color(rgb(if selected { colors.bg } else { colors.text }))
            .focus_visible(move |style| style.border_color(rgb(colors.focus)))
            .hover(move |style| style.bg(rgb(kind.hover(colors))))
            .max_w_full();
        controls::native(control, |control| controls::button(control, kind, colors))
    }

    fn radio(
        &self,
        key: String,
        label: String,
        checked: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let id = format!("new-chat-{key}");
        let change = cx.listener(move |choice, checked: &bool, window, cx| {
            if *checked {
                action(choice, window, cx);
            }
        });
        form_input::control_element(
            id.clone(),
            Self::choice_style(
                behavior::radio_content(id, label.clone(), label, checked),
                checked,
                theme::palette(cx),
            )
            .track_focus(&self.controls[&key])
            .on_change(move |checked, _, window, app| change(&checked, window, app)),
        )
    }

    fn fast_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let label = if self.fast { "Fast on" } else { "Fast off" };
        let change = cx.listener(|choice, pressed: &bool, _, cx| {
            choice.fast = *pressed;
            cx.notify();
        });
        form_input::control_element(
            "new-chat-fast",
            Self::choice_style(
                behavior::toggle_content("new-chat-fast", label, label, self.fast),
                self.fast,
                theme::palette(cx),
            )
            .track_focus(&self.controls["fast"])
            .on_change(move |pressed, _, window, app| change(&pressed, window, app)),
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
        let modes = MODES
            .into_iter()
            .enumerate()
            .map(|(index, (mode, label))| {
                self.radio(
                    format!("mode-{index}"),
                    label.into(),
                    self.mode == mode,
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
                self.radio(
                    format!("location-{index}"),
                    location.label.clone(),
                    self.location == index,
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
        let model_groups: Vec<AnyElement> = [Provider::Codex, Provider::Claude]
            .into_iter()
            .map(|provider| {
                let group = provider_index(provider);
                let chosen = self.provider == provider;
                let mut rows = vec![self.radio(
                    format!("model-default-{group}"),
                    format!("{} default", provider_label(provider)),
                    chosen && current.is_empty(),
                    cx,
                    move |choice, window, cx| {
                        choice.pick_model(provider, String::new(), window, cx)
                    },
                )];
                for (row, model) in self.models[group].supported.iter().enumerate() {
                    if !format!("{} {} {}", model.name, model.id, model.description)
                        .to_lowercase()
                        .contains(&query)
                    {
                        continue;
                    }
                    let id = model.id.clone();
                    rows.push(self.radio(
                        format!("model-{group}-{row}"),
                        format!(
                            "{}{}",
                            model.name,
                            if model.is_default { " · default" } else { "" }
                        ),
                        chosen && current == id,
                        cx,
                        move |choice, window, cx| {
                            choice.pick_model(provider, id.clone(), window, cx)
                        },
                    ));
                }
                for (row, id) in self.models[group].configured.iter().enumerate() {
                    if !id.to_lowercase().contains(&query) {
                        continue;
                    }
                    let id = id.clone();
                    rows.push(self.radio(
                        format!("configured-{group}-{row}"),
                        format!("{id} · previously configured"),
                        chosen && current == id,
                        cx,
                        move |choice, window, cx| {
                            choice.pick_model(provider, id.clone(), window, cx)
                        },
                    ));
                }
                Self::group(provider_label(provider), rows)
            })
            .collect();
        let selected = self.selected_model(cx);
        let efforts = selected
            .map(|model| model.efforts.clone())
            .unwrap_or_default();
        let supports_fast = selected.is_some_and(|model| model.supports_fast);
        let mut effort_rows = Vec::new();
        if !efforts.is_empty() {
            effort_rows.push(self.radio(
                "effort-default".into(),
                "Default effort".into(),
                self.effort.is_none(),
                cx,
                |choice, _, cx| {
                    choice.effort = None;
                    cx.notify();
                },
            ));
            for effort in efforts {
                effort_rows.push(self.radio(
                    format!("effort-{effort}"),
                    effort.clone(),
                    self.effort.as_ref() == Some(&effort),
                    cx,
                    move |choice, _, cx| {
                        choice.effort = Some(effort.clone());
                        cx.notify();
                    },
                ));
            }
        }
        let note = if self.loading {
            "Reading saved model choices…".to_owned()
        } else if self.models[index].supported.is_empty() {
            format!(
                "No saved {} model list. Use its default or type an ID; the chat lists its models once it runs.",
                provider_label(self.provider)
            )
        } else {
            format!(
                "A typed ID is a {} model. Models come from saved provider metadata; availability is checked when the chat starts.",
                provider_label(self.provider)
            )
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
                    .max_h(ui_text::space(220.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(10.0))
                    .children(model_groups),
            )
            .children(
                (self.provider == Provider::Codex)
                    .then(|| self.models[index].account_label.as_ref())
                    .flatten()
                    .map(|label| {
                        div()
                            .text_color(rgb(colors.muted))
                            .child(format!("Codex account · {label}"))
                    }),
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
            .children(supports_fast.then(|| self.fast_toggle(cx)))
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
        let assert_radio = |window: &mut Window, app: &mut gpui::App, id: &str, checked: bool| {
            let node = form_input::test_ax_node(window, app, id.to_owned());
            assert_eq!(node.role(), gpui::Role::RadioButton, "{id}");
            assert_eq!(node.is_selected(), Some(checked), "{id}");
            assert_eq!(
                node.toggled(),
                Some(if checked {
                    gpui::accesskit::Toggled::True
                } else {
                    gpui::accesskit::Toggled::False
                }),
                "{id}"
            );
        };
        let assert_fast = |window: &mut Window, app: &mut gpui::App, pressed: bool| {
            let node = form_input::test_ax_node(window, app, "new-chat-fast");
            assert_eq!(node.role(), gpui::Role::Button);
            assert_eq!(
                node.toggled(),
                Some(if pressed {
                    gpui::accesskit::Toggled::True
                } else {
                    gpui::accesskit::Toggled::False
                })
            );
        };
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
            // One list of both providers' models; there is no provider to pick first.
            for (id, checked) in [
                ("new-chat-location-0", false),
                ("new-chat-location-1", true),
                ("new-chat-mode-0", true),
                ("new-chat-mode-2", false),
                ("new-chat-model-default-0", true),
                ("new-chat-model-0-0", false),
                ("new-chat-model-default-1", false),
                ("new-chat-model-1-0", false),
                ("new-chat-effort-default", true),
                ("new-chat-effort-high", false),
            ] {
                assert_radio(window, app, id, checked);
            }
            assert!(window.try_find("new-chat-provider-0").is_none());
            assert_fast(window, app, false);
            let cancel = form_input::test_ax_node(window, app, "new-chat-cancel");
            assert_eq!(cancel.role(), gpui::Role::Button);
            assert_eq!(cancel.is_selected(), None);
            assert_eq!(cancel.toggled(), None);
            window.click("new-chat-location-0", app);
            window.click("new-chat-model-search", app);
            window.input("Claude", app);
        });
        assert!(requests.borrow().is_empty());
        test_turn(cx, handle, |window, app| {
            assert_radio(window, app, "new-chat-location-0", true);
            assert_radio(window, app, "new-chat-location-1", false);
            assert_radio(window, app, "new-chat-model-default-0", true);
            assert_radio(window, app, "new-chat-model-1-0", false);
            assert!(
                window.try_find("new-chat-model-0-0").is_none(),
                "the search leaves out the Codex model"
            );
            window.click("new-chat-location-1", app);
            window.click("new-chat-model-1-0", app);
            window.click("new-chat-mode-2", app);
        });
        test_turn(cx, handle, |window, app| {
            assert_radio(window, app, "new-chat-location-0", false);
            assert_radio(window, app, "new-chat-location-1", true);
            assert_radio(window, app, "new-chat-mode-0", false);
            assert_radio(window, app, "new-chat-mode-2", true);
            // The Claude model is chosen, and with it Claude.
            assert_radio(window, app, "new-chat-model-default-0", false);
            assert_radio(window, app, "new-chat-model-default-1", false);
            assert_radio(window, app, "new-chat-model-1-0", true);
            window.click("new-chat-effort-high", app);
            window.click("new-chat-fast", app);
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
            assert_radio(window, app, "new-chat-effort-default", false);
            assert_radio(window, app, "new-chat-effort-high", true);
            assert_fast(window, app, true);
            let node = form_input::test_ax_node(window, app, "new-chat-confirm");
            assert_eq!(node.role(), gpui::Role::Button);
            assert_eq!(node.label(), Some("Create chat"));
            assert_eq!(node.is_selected(), None);
            assert_eq!(node.toggled(), None);
            window.click("new-chat-confirm", app);
        });
        assert_eq!(requests.borrow().len(), 1, "actual pointer confirmation");
        test_turn(cx, handle, |window, app| {
            // A queued duplicate after actual pointer confirmation stays inert.
            choice.update(app, |choice, cx| choice.confirm(window, cx));
        });
        {
            let requests = requests.borrow();
            assert_eq!(requests.len(), 1);
            let request = &requests[0];
            assert_eq!(request.provider, Provider::Claude);
            assert_eq!(request.model.as_deref(), Some("claude-fixture"));
            assert_eq!(request.approval_mode, ApprovalMode::Full);
            assert_eq!(request.worktree_id.as_deref(), Some("fixture-tree"));
            assert_eq!(request.cwd, PathBuf::from("/fixture/tree"));
            assert_eq!(request.effort.as_deref(), Some("high"));
            assert!(request.fast);
            assert_eq!(request.codex_account_id, None);
        }

        // A fresh invocation exercises Base keyboard activation independently of
        // pointer focus/confirmation; the same named test covers both entry paths.
        let (keyboard_handle, keyboard_choice) = fixture(cx);
        let keyboard_requests = Rc::new(RefCell::new(Vec::new()));
        let observed = keyboard_requests.clone();
        let _keyboard_subscription = cx.update(|app| {
            app.subscribe(&keyboard_choice, move |_, event: &ChoiceEvent, _| {
                if let ChoiceEvent::Confirmed(request) = event {
                    observed.borrow_mut().push(request.clone());
                }
            })
        });
        test_turn(cx, keyboard_handle, |window, app| {
            assert!(
                keyboard_choice
                    .read(app)
                    .search
                    .read(app)
                    .focus_handle(app)
                    .is_focused(window)
            );
            window.press("enter", app);
        });
        assert!(keyboard_requests.borrow().is_empty());
        for (key, previous, activation) in [
            ("location-0", "location-1", "enter"),
            ("mode-1", "mode-0", "space"),
            ("model-1-0", "model-default-0", "enter"),
            ("effort-high", "effort-default", "space"),
        ] {
            test_turn(cx, keyboard_handle, |window, app| {
                assert_radio(window, app, &format!("new-chat-{key}"), false);
                assert_radio(window, app, &format!("new-chat-{previous}"), true);
                let focus = keyboard_choice.read(app).controls[key].clone();
                focus.focus(window, app);
                window.press(activation, app);
            });
            test_turn(cx, keyboard_handle, |window, app| {
                assert_radio(window, app, &format!("new-chat-{key}"), true);
                assert_radio(window, app, &format!("new-chat-{previous}"), false);
            });
            assert!(keyboard_requests.borrow().is_empty());
        }
        test_turn(cx, keyboard_handle, |window, app| {
            assert_fast(window, app, false);
            let focus = keyboard_choice.read(app).controls["fast"].clone();
            focus.focus(window, app);
            window.press("space", app);
        });
        assert!(keyboard_requests.borrow().is_empty());
        test_turn(cx, keyboard_handle, |window, app| {
            assert_fast(window, app, true);
            let focus = keyboard_choice.read(app).controls["confirm"].clone();
            focus.focus(window, app);
            window.press("enter", app);
        });
        assert_eq!(
            keyboard_requests.borrow().len(),
            1,
            "actual keyboard confirmation"
        );
        test_turn(cx, keyboard_handle, |window, app| {
            keyboard_choice.update(app, |choice, cx| choice.confirm(window, cx));
        });
        let requests = keyboard_requests.borrow();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.provider, Provider::Claude);
        assert_eq!(request.model.as_deref(), Some("claude-fixture"));
        assert_eq!(request.approval_mode, ApprovalMode::AutoEdit);
        assert_eq!(request.worktree_id, None);
        assert_eq!(request.cwd, PathBuf::from("/fixture/root"));
        assert_eq!(request.effort.as_deref(), Some("high"));
        assert!(request.fast);
        assert_eq!(request.codex_account_id, None);
    }

    #[gpui::test]
    fn cancel_emits_no_creation_request(cx: &mut TestAppContext) {
        // Keep a real invoker input mounted while the chooser opens and retires,
        // matching Workspace's subscribed-close lifecycle without a backend.
        struct Owner {
            original: Entity<InputState>,
            choice: Option<Entity<ChatChoice>>,
            subscription: Option<Subscription>,
            closes: usize,
            creates: usize,
        }
        impl Owner {
            fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ChatChoice> {
                let choice = cx.new(|cx| {
                    let mut choice = ChatChoice::draft(
                        "fixture-project".into(),
                        vec![Location {
                            worktree_id: None,
                            label: "Project root".into(),
                            path: "/fixture/root".into(),
                        }],
                        None,
                        None,
                        false,
                        window,
                        cx,
                    );
                    choice.apply_models(Default::default(), cx);
                    choice
                });
                self.subscription = Some(cx.subscribe_in(
                    &choice,
                    window,
                    |owner, _, event: &ChoiceEvent, _, cx| {
                        match event {
                            ChoiceEvent::Closed => {
                                owner.closes += 1;
                                owner.choice = None;
                            }
                            ChoiceEvent::Confirmed(_) => owner.creates += 1,
                        }
                        cx.notify();
                    },
                ));
                self.choice = Some(choice.clone());
                cx.notify();
                choice
            }
        }
        impl Render for Owner {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .child(form_input::frame(
                        "original-draft",
                        &self.original,
                        false,
                        window,
                        cx,
                    ))
                    .children(self.choice.clone())
            }
        }
        let (handle, owner) = crate::form_input::test_window(cx, |window, cx| Owner {
            original: text_input::single_line("original draft", "Original", window, cx),
            choice: None,
            subscription: None,
            closes: 0,
            creates: 0,
        });
        let original = owner.read_with(cx, |owner, _| owner.original.clone());
        let identity = original.entity_id();
        let mut closed_choice = None;
        test_turn(cx, handle, |window, app| {
            let original_focus = original.read(app).focus_handle(app);
            original_focus.focus(window, app);
            original.update(app, |input, cx| input.set_selected_range(2..7, cx));
            closed_choice = Some(owner.update(app, |owner, cx| owner.open(window, cx)));
        });
        test_turn(cx, handle, |window, app| {
            let choice = owner.read(app).choice.as_ref().unwrap().read(app);
            assert!(choice.search.read(app).focus_handle(app).is_focused(window));
            assert!(choice.focus.contains_focused(window, app));
            // The first key after opening: no field click or manual chooser focus.
            window.press("escape", app);
        });
        test_turn(cx, handle, |window, app| {
            assert!(owner.read(app).choice.is_none());
            assert_eq!((owner.read(app).closes, owner.read(app).creates), (1, 0));
            assert!(original.read(app).focus_handle(app).is_focused(window));
            assert_eq!(original.read(app).value(), "original draft");
            assert_eq!(original.read(app).selected_range(), 2..7);
            assert_eq!(owner.read(app).original.entity_id(), identity);
            closed_choice
                .as_ref()
                .unwrap()
                .update(app, |choice, cx| choice.confirm(window, cx));
        });
        test_turn(cx, handle, |window, app| {
            assert_eq!(
                (owner.read(app).closes, owner.read(app).creates),
                (1, 0),
                "a queued confirmation after cancellation is inert"
            );
            owner.update(app, |owner, cx| {
                owner.open(window, cx);
            });
        });
        test_turn(cx, handle, |window, app| {
            let choice = owner.read(app).choice.as_ref().unwrap().read(app);
            assert!(choice.focus.contains_focused(window, app));
            window.input("typed-fixture", app);
            window.press("enter", app);
        });
        test_turn(cx, handle, |window, app| {
            let choice = owner.read(app).choice.as_ref().unwrap().read(app);
            assert_eq!(choice.search.read(app).value(), "typed-fixture");
            assert_eq!(owner.read(app).creates, 0);
            assert_eq!(original.read(app).value(), "original draft");
            window.press("escape", app);
        });
        test_turn(cx, handle, |window, app| {
            assert!(owner.read(app).choice.is_none());
            assert_eq!((owner.read(app).closes, owner.read(app).creates), (2, 0));
            assert!(original.read(app).focus_handle(app).is_focused(window));
            assert_eq!(original.read(app).value(), "original draft");
            assert_eq!(original.read(app).selected_range(), 2..7);
            assert_eq!(owner.read(app).original.entity_id(), identity);
        });
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
