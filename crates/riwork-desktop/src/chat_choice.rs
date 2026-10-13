//! Explicit new-chat choices: a visible provider selector and its model catalog.
//! Reading saved model metadata
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
    /// Muse only: start the chat with Muse's contributor mode off. Kept while another
    /// provider is picked, and sent only with Muse.
    contributor_opt_out: bool,
    models: [Models; 3],
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
    match provider {
        Provider::Codex => 0,
        Provider::Claude => 1,
        Provider::Muse => 2,
    }
}
fn provider_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
        Provider::Muse => "Muse",
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
        let discovery_home = home.clone();
        let discovery_project = project.clone();
        let discovery = cx.background_executor().spawn(async move {
            catalog::available(&discovery_home, Some(&discovery_project), Provider::Muse)
        });
        let work = cx.background_executor().spawn(async move {
            [
                catalog::saved(&home, Some(&metadata_project), Provider::Codex),
                catalog::saved(&home, Some(&metadata_project), Provider::Claude),
                catalog::saved(&home, Some(&metadata_project), Provider::Muse),
            ]
        });
        cx.spawn(async move |this, cx| {
            let mut models = work.await;
            let _ = this.update(cx, |choice, cx| {
                choice.apply_models(models.clone(), cx);
                cx.notify();
            });
            models[provider_index(Provider::Muse)] = discovery.await;
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
        let search = text_input::single_line("", "Search models", window, cx);
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
            "provider-2",
            "model-default-0",
            "model-default-1",
            "model-default-2",
            "mode-0",
            "mode-1",
            "mode-2",
            "mode-3",
            "effort-default",
            "fast",
            "contributor",
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
            contributor_opt_out: false,
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

    fn apply_models(&mut self, models: [Models; 3], cx: &mut Context<Self>) {
        self.models = models;
        self.loading = false;
        for (provider, models) in self.models.iter().enumerate() {
            for (index, model) in models.supported.iter().enumerate() {
                self.controls
                    .entry(format!("model-{provider}-{index}"))
                    .or_insert_with(|| cx.focus_handle());
                for effort in &model.efforts {
                    self.controls
                        .entry(format!("effort-{effort}"))
                        .or_insert_with(|| cx.focus_handle());
                }
            }
            for index in 0..models.configured.len() {
                self.controls
                    .entry(format!("configured-{provider}-{index}"))
                    .or_insert_with(|| cx.focus_handle());
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

    fn pick_provider(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        if self.provider == provider {
            return;
        }
        self.pick_model(provider, String::new(), window, cx);
        form_input::set_value(&self.search, String::new(), window, cx);
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
            parent_id: None,
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
            contributor_opt_out: self.provider == Provider::Muse && self.contributor_opt_out,
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

    /// Muse's contributor mode for this chat. Muse says that with it off, content won't be
    /// used for product improvement; the chat passes `--contributor-mode off` to every
    /// `muse serve` it starts, which Muse never saves. Left alone, Muse's settings decide.
    fn contributor_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let label = if self.contributor_opt_out {
            "Contributor mode off"
        } else {
            "Contributor mode: Muse settings"
        };
        let change = cx.listener(|choice, pressed: &bool, _, cx| {
            choice.contributor_opt_out = *pressed;
            cx.notify();
        });
        div()
            .flex()
            .flex_col()
            .gap(ui_text::space(6.0))
            .child(form_input::control_element(
                "new-chat-contributor",
                Self::choice_style(
                    behavior::toggle_content(
                        "new-chat-contributor",
                        label,
                        label,
                        self.contributor_opt_out,
                    ),
                    self.contributor_opt_out,
                    colors,
                )
                .track_focus(&self.controls["contributor"])
                .on_change(move |pressed, _, window, app| change(&pressed, window, app)),
            ))
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .child(if self.contributor_opt_out {
                        "Muse contributor mode is off for this chat, so your content in it won't be used for product improvement. Your Muse settings are not changed."
                    } else {
                        "Contributor mode follows your Muse settings. Turn it off here for this chat only."
                    }),
            )
            .into_any_element()
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
        let providers = [Provider::Codex, Provider::Claude, Provider::Muse]
            .into_iter()
            .map(|provider| {
                self.radio(
                    format!("provider-{}", provider_index(provider)),
                    provider_label(provider).into(),
                    self.provider == provider,
                    cx,
                    move |choice, window, cx| choice.pick_provider(provider, window, cx),
                )
            })
            .collect();
        let model_group = {
            let provider = self.provider;
            let group = provider_index(provider);
            let mut rows = vec![self.radio(
                format!("model-default-{group}"),
                format!("{} default", provider_label(provider)),
                current.is_empty(),
                cx,
                move |choice, window, cx| choice.pick_model(provider, String::new(), window, cx),
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
                    current == id,
                    cx,
                    move |choice, window, cx| choice.pick_model(provider, id.clone(), window, cx),
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
                    current == id,
                    cx,
                    move |choice, window, cx| choice.pick_model(provider, id.clone(), window, cx),
                ));
            }
            Self::group("Model", rows)
        };
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
        } else if let Some(warning) = &self.models[index].warning {
            warning.clone()
        } else if self.models[index].supported.is_empty() {
            format!(
                "No saved {} model list. Use its default or type an ID; the chat lists its models once it runs.",
                provider_label(self.provider)
            )
        } else {
            format!(
                "A typed ID is a {} model. Models come from provider metadata; availability is checked when the chat starts.",
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
            .child(Self::group("Provider", providers))
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
                    .child(model_group),
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
            .children((self.provider == Provider::Muse).then(|| self.contributor_toggle(cx)))
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
                    models("muse-fixture", "Muse fixture"),
                ],
                cx,
            );
            choice
        })
    }

    #[gpui::test]
    fn muse_is_reachable_without_scrolling_past_other_providers(cx: &mut TestAppContext) {
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
            choice.update(app, |choice, cx| {
                let mut models = choice.models.clone();
                for provider in [0, 1] {
                    let template = models[provider].supported[0].clone();
                    models[provider]
                        .supported
                        .extend((0..40).map(|index| ModelOption {
                            id: format!("long-catalog-{provider}-{index}"),
                            name: format!("Long catalog model {index}"),
                            ..template.clone()
                        }));
                }
                models[2].supported[0].supports_fast = false;
                choice.apply_models(models, cx);
                form_input::set_value(&choice.search, "codex".into(), window, cx);
                form_input::set_value(&choice.model, "codex-fixture".into(), window, cx);
                choice.effort = Some("high".into());
                choice.fast = true;
            });
        });
        test_turn(cx, handle, |window, app| {
            assert!(
                window.find("new-chat-provider-2").bounds().bottom()
                    <= window.find("new-chat-model-default-0").bounds().top(),
                "Muse's provider control is outside the clipped model catalog"
            );
            window.click("new-chat-provider-2", app);
        });
        test_turn(cx, handle, |window, app| {
            let draft = choice.read(app);
            assert_eq!(draft.provider, Provider::Muse);
            assert!(draft.model.read(app).value().is_empty());
            assert!(draft.search.read(app).value().is_empty());
            assert_eq!(draft.effort, None);
            assert!(!draft.fast);
            let model = window.find("new-chat-model-2-0").bounds();
            assert!(model.top() >= window.find("new-chat-model-default-2").bounds().top());
            assert!(model.bottom() <= window.find("new-chat-model-input").bounds().top());
            window.click("new-chat-model-2-0", app);
        });
        assert!(requests.borrow().is_empty(), "browsing creates no chat");
        test_turn(cx, handle, |window, app| {
            choice.update(app, |choice, cx| {
                let handle = choice.controls["model-2-0"].clone();
                choice.apply_models(choice.models.clone(), cx);
                assert_eq!(choice.controls["model-2-0"], handle);
                assert_eq!(choice.provider, Provider::Muse);
                assert_eq!(choice.model.read(cx).value(), "muse-fixture");
            });
            window.click("new-chat-effort-high", app);
        });
        test_turn(cx, handle, |window, app| {
            window.click("new-chat-confirm", app)
        });
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].provider, Provider::Muse);
        assert_eq!(requests[0].model.as_deref(), Some("muse-fixture"));
        assert_eq!(requests[0].effort.as_deref(), Some("high"));
        assert!(!requests[0].fast);
        assert!(!requests[0].contributor_opt_out, "Muse settings decide");
        assert_eq!(requests[0].codex_account_id, None);
        assert_eq!(requests[0].worktree_id.as_deref(), Some("fixture-tree"));
        assert_eq!(requests[0].approval_mode, ApprovalMode::Supervised);
    }

    #[gpui::test]
    fn contributor_opt_out_is_offered_for_muse_only_and_rides_only_its_request(
        cx: &mut TestAppContext,
    ) {
        let confirmed = |cx: &mut TestAppContext, choice: &Entity<ChatChoice>| {
            let requests = Rc::new(RefCell::new(Vec::new()));
            let observed = requests.clone();
            let subscription = cx.update(|app| {
                app.subscribe(choice, move |_, event: &ChoiceEvent, _| {
                    if let ChoiceEvent::Confirmed(request) = event {
                        observed.borrow_mut().push(request.clone());
                    }
                })
            });
            (requests, subscription)
        };
        let (handle, choice) = fixture(cx);
        let (requests, _subscription) = confirmed(cx, &choice);
        test_turn(cx, handle, |window, app| {
            assert_eq!(choice.read(app).provider, Provider::Codex);
            assert!(window.try_find("new-chat-contributor").is_none());
            window.click("new-chat-provider-1", app);
        });
        test_turn(cx, handle, |window, app| {
            assert!(window.try_find("new-chat-contributor").is_none(), "Claude");
            window.click("new-chat-provider-2", app);
        });
        test_turn(cx, handle, |window, app| {
            assert!(!choice.read(app).contributor_opt_out);
            window.click("new-chat-contributor", app);
        });
        test_turn(cx, handle, |window, app| {
            assert!(choice.read(app).contributor_opt_out);
            window.click("new-chat-confirm", app);
        });
        {
            let requests = requests.borrow();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].provider, Provider::Muse);
            assert!(requests[0].contributor_opt_out);
        }

        // Turned off for Muse, then another provider chosen: that chat has no such mode.
        let (handle, choice) = fixture(cx);
        let (requests, _subscription) = confirmed(cx, &choice);
        test_turn(cx, handle, |window, app| {
            window.click("new-chat-provider-2", app)
        });
        test_turn(cx, handle, |window, app| {
            window.click("new-chat-contributor", app);
        });
        test_turn(cx, handle, |window, app| {
            window.click("new-chat-provider-0", app)
        });
        test_turn(cx, handle, |window, app| {
            assert!(window.try_find("new-chat-contributor").is_none());
            window.click("new-chat-confirm", app);
        });
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].provider, Provider::Codex);
        assert!(!requests[0].contributor_opt_out);
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
            // A queued second confirmation (a click or key) is inert.
            choice.update(app, |choice, cx| choice.confirm(window, cx));
        });
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1, "confirmation is single use");
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
