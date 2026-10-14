//! The words and numbers of the chat toolbar: the approval modes, the choices for model and
//! effort, the context meter and the thread id.

use crate::chat::model::{Account, ApprovalMode, ClaudeBilling, ModelOption, Provider, Usage};

/// The modes in the order the picker lists them, with what each lets the agent do.
pub const MODES: [(ApprovalMode, &str, &str); 4] = [
    (
        ApprovalMode::Supervised,
        "Supervised",
        "Asks before commands and edits",
    ),
    (
        ApprovalMode::AutoEdit,
        "Auto-edit",
        "Edits the workspace freely, asks for the rest",
    ),
    (ApprovalMode::Full, "Full", "Never asks"),
    (
        ApprovalMode::Plan,
        "Plan",
        "Plans first and changes nothing",
    ),
];

pub fn mode_label(mode: ApprovalMode) -> &'static str {
    MODES
        .iter()
        .find(|(candidate, ..)| *candidate == mode)
        .map_or("Supervised", |(_, label, _)| label)
}

/// The reasoning efforts offered for a provider when its driver has not said which models
/// there are. The host passes the name on as it is, so a level the installed CLI does not
/// know is the provider's to refuse.
pub fn efforts(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Codex => &["low", "medium", "high", "xhigh"],
        Provider::Claude => &["low", "medium", "high", "xhigh", "max"],
        Provider::Muse => &["low", "medium", "high", "xhigh"],
    }
}

/// Models offered as buttons beside the text field, when the driver has not listed its
/// models. Claude's aliases keep working as models change; Codex model names change often
/// enough that only the text field is offered.
pub fn model_suggestions(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Codex => &[],
        Provider::Claude => &["opus", "sonnet", "haiku"],
        Provider::Muse => &[],
    }
}

/// The model the chat runs, as far as the driver's list says: the chosen one, or the
/// provider's own choice for a chat that has chosen none. A chosen model the list does not
/// know (typed by hand, or from an older list) is `None`.
pub fn selected_model<'a>(
    models: &'a [ModelOption],
    chosen: Option<&str>,
) -> Option<&'a ModelOption> {
    match chosen.filter(|id| !id.is_empty()) {
        Some(id) => models.iter().find(|model| model.id == id),
        None => models.iter().find(|model| model.is_default),
    }
}

/// What the model button says: the model's name, the chosen name as it is when the list does
/// not know it, or a prompt when nothing is chosen and nothing is known.
pub fn model_label(models: &[ModelOption], chosen: Option<&str>) -> String {
    match (selected_model(models, chosen), chosen) {
        (Some(model), _) => model.name.clone(),
        (None, Some(chosen)) if !chosen.is_empty() => chosen.to_owned(),
        _ => "model".to_owned(),
    }
}

/// One line of the model picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRow {
    /// What a click chooses.
    pub id: String,
    /// The model's name, with a mark on the provider's default.
    pub label: String,
    pub detail: Option<String>,
    pub current: bool,
}

/// The model picker's lines, in the driver's order.
pub fn model_rows(models: &[ModelOption], chosen: Option<&str>) -> Vec<ModelRow> {
    let current = selected_model(models, chosen).map(|model| model.id.as_str());
    models
        .iter()
        .map(|model| ModelRow {
            id: model.id.clone(),
            // A name that already says so ("Default (recommended)") gets no second mark.
            label: if model.is_default && !model.name.to_lowercase().contains("default") {
                format!("{} (default)", model.name)
            } else {
                model.name.clone()
            },
            detail: Some(model.description.clone()).filter(|text| !text.is_empty()),
            current: current == Some(model.id.as_str()),
        })
        .collect()
}

/// Search the installed provider's metadata without inventing models or changing ordering.
pub fn filtered_model_rows(
    models: &[ModelOption],
    chosen: Option<&str>,
    query: &str,
) -> Vec<ModelRow> {
    let query = query.trim().to_lowercase();
    model_rows(models, chosen)
        .into_iter()
        .filter(|row| {
            query.is_empty()
                || row.id.to_lowercase().contains(&query)
                || row.label.to_lowercase().contains(&query)
                || row
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.to_lowercase().contains(&query))
        })
        .collect()
}

/// The efforts to offer for the chosen model: the ones its driver listed for it, none at
/// all for a model that takes none, and the provider's usual ones when the model is not
/// described.
pub fn effort_choices(
    models: &[ModelOption],
    chosen: Option<&str>,
    provider: Provider,
) -> Vec<String> {
    match selected_model(models, chosen) {
        Some(model) => model.efforts.clone(),
        None => efforts(provider).iter().map(|e| (*e).to_owned()).collect(),
    }
}

/// One line of the effort picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffortRow {
    /// What a click chooses.
    pub effort: String,
    /// The effort, with a mark on the one the model uses when none is chosen.
    pub label: String,
    pub current: bool,
}

/// The effort picker's lines. With no effort chosen, the model's default is the current one.
pub fn effort_rows(
    models: &[ModelOption],
    chosen_model: Option<&str>,
    chosen_effort: Option<&str>,
    provider: Provider,
) -> Vec<EffortRow> {
    let default = selected_model(models, chosen_model).and_then(|m| m.default_effort.as_deref());
    let current = chosen_effort.or(default);
    effort_choices(models, chosen_model, provider)
        .into_iter()
        .map(|effort| EffortRow {
            label: if Some(effort.as_str()) == default {
                format!("{effort} (default)")
            } else {
                effort.clone()
            },
            current: Some(effort.as_str()) == current,
            effort,
        })
        .collect()
}

/// What the effort button says: the chosen effort, else the model's default, else a prompt.
pub fn effort_label(
    models: &[ModelOption],
    chosen_model: Option<&str>,
    chosen_effort: Option<&str>,
) -> String {
    chosen_effort
        .or_else(|| selected_model(models, chosen_model).and_then(|m| m.default_effort.as_deref()))
        .unwrap_or("effort")
        .to_owned()
}

/// Whether the effort button is there: not for a model its driver says takes no effort.
pub fn effort_available(models: &[ModelOption], chosen: Option<&str>, provider: Provider) -> bool {
    !effort_choices(models, chosen, provider).is_empty()
}

/// Whether the Fast toggle is there: only for a model that has a fast mode.
pub fn fast_available(models: &[ModelOption], chosen: Option<&str>) -> bool {
    selected_model(models, chosen).is_some_and(|model| model.supports_fast)
}

/// What the Fast toggle says.
pub fn fast_label(on: bool) -> &'static str {
    if on { "Fast on" } else { "Fast off" }
}

/// What a Muse chat's contributor toggle says, as the new-chat dialog does: on or off.
pub fn contributor_label(on: bool) -> &'static str {
    if on {
        "Contributor on"
    } else {
        "Contributor off"
    }
}

/// What the toggle says of a chat a chat host from before RiWork's off by default reports with
/// neither (`ChatInfo::reported_contributor_mode`): the user's Muse settings decide there,
/// until the chat is turned off or that host is replaced.
pub const CONTRIBUTOR_SETTINGS_LABEL: &str = "Contributor: Muse settings";

/// The model and effort to send when `model` is picked. An effort the new model does not take
/// moves to the new model's own default (the driver would leave it out otherwise, and the chat
/// would show one it does not use); an effort it takes stays, and none stays none.
pub fn model_choice(
    models: &[ModelOption],
    model: &str,
    current_effort: Option<&str>,
) -> (String, Option<String>) {
    let effort = current_effort.and_then(|effort| {
        let new = models.iter().find(|candidate| candidate.id == model)?;
        if new.efforts.is_empty() || new.efforts.iter().any(|e| e == effort) {
            None
        } else {
            new.default_effort.clone()
        }
    });
    (model.to_owned(), effort)
}

/// A token count in a few characters: `842`, `12.3k`, `1.2M`.
pub fn tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => format!("{:.1}k", count as f64 / 1_000.0),
        10_000..=999_999 => format!("{}k", count / 1_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

/// How much of the context window is in use, 0 to 1, when both numbers are known.
pub fn context_fraction(usage: &Usage) -> Option<f32> {
    let (used, window) = (usage.context_used?, usage.context_window?);
    (window > 0).then(|| (used as f64 / window as f64).clamp(0.0, 1.0) as f32)
}

/// The number beside the meter's ring: how much of the context is in use, when it is known.
pub fn percent_text(usage: &Usage) -> Option<String> {
    context_fraction(usage).map(|fraction| format!("{}%", (fraction * 100.0).round() as u32))
}

/// The meter's counts: the context in use, or what was spent when the window is not known.
pub fn usage_text(usage: &Usage) -> String {
    match (
        context_fraction(usage),
        usage.context_used,
        usage.context_window,
    ) {
        (Some(_), Some(used), Some(window)) => format!("{} / {}", tokens(used), tokens(window)),
        _ => format!(
            "{} in · {} out",
            tokens(usage.input_tokens),
            tokens(usage.output_tokens)
        ),
    }
}

/// Claude's own figure for what a chat cost. It is an estimate, never a bill: the "≈" says
/// so in the row, the hover hint in words.
pub fn cost_text(cost_usd: f64) -> String {
    if cost_usd >= 0.01 {
        format!("≈ ${cost_usd:.2}")
    } else {
        format!("≈ ${cost_usd:.3}")
    }
}

/// How the ring warns: the text color below 70 % in use, the warning tint from 70 %, red
/// from 90 %. The iPhone's ring uses the same thresholds, so both agree on a chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingTone {
    Normal,
    Warning,
    Critical,
}

pub fn ring_tone(fraction: f32) -> RingTone {
    if fraction >= 0.9 {
        RingTone::Critical
    } else if fraction >= 0.7 {
        RingTone::Warning
    } else {
        RingTone::Normal
    }
}

/// The points of the ring's filled arc, starting at the top and running clockwise for
/// `fraction` of the circle, around `center` (y grows downward). No points for nothing in use.
pub fn ring_arc(fraction: f32, center: (f32, f32), radius: f32) -> Vec<(f32, f32)> {
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction <= 0.0 {
        return Vec::new();
    }
    // Enough segments for a smooth curve at any size the row draws it.
    let steps = ((fraction * 48.0).ceil() as usize).max(2);
    (0..=steps)
        .map(|step| {
            let angle = std::f32::consts::TAU * fraction * step as f32 / steps as f32;
            (
                center.0 + radius * angle.sin(),
                center.1 - radius * angle.cos(),
            )
        })
        .collect()
}

/// The toolbar badge of a chat that bills an API key, and its hover hint; `None` while the
/// agent runs on a subscription or has not said how it signed in.
pub fn api_key_badge(account: Option<&Account>) -> Option<(&'static str, String)> {
    let source = account?.api_key_source.as_deref()?;
    let hint = if source == "ANTHROPIC_API_KEY" {
        "Billed to the Anthropic API key in ANTHROPIC_API_KEY, not a Claude subscription. To use your subscription, switch this chat to Subscription beside its message box.".to_owned()
    } else {
        format!("Billed to an Anthropic API key ({source}), not a Claude subscription.")
    };
    Some(("API key", hint))
}

/// What a Claude chat's billing switch shows and does. It shows what the running Claude says
/// it bills (`account`, once its handshake has said), else the chat's choice (`billing`), which
/// is what the chat's next start uses; `mismatch` when the two disagree in a way the user did
/// not ask for. No key is ever part of it.
pub struct BillingControl {
    pub label: &'static str,
    pub short: &'static str,
    pub hint: String,
    /// Shown as on: the chat bills an API key.
    pub on: bool,
    pub mismatch: bool,
    pub click: BillingClick,
}

/// What one click on the billing switch does: move to the other billing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BillingClick {
    /// Change the chat's choice; the host restarts Claude on the same conversation.
    Configure(ClaudeBilling),
    /// The chat is set to API key already, but its Claude started before a key was saved:
    /// stop it, and its next start has the key.
    Restart,
    /// No key is saved to bill: Settings, at the key field.
    EnterKey,
}

pub fn billing_control(
    billing: ClaudeBilling,
    account: Option<&Account>,
    key_saved: bool,
) -> BillingControl {
    use ClaudeBilling::{ApiKey, Subscription};
    let source = account.and_then(|account| account.api_key_source.as_deref());
    let shown = match account {
        Some(_) if source.is_some() => ApiKey,
        Some(_) => Subscription,
        None => billing,
    };
    let to_subscription =
        "Switch to your Claude subscription; Claude restarts on the same conversation.";
    let (hint, mismatch, click) = match (shown, billing) {
        (ApiKey, ApiKey) => (
            match (source, key_saved) {
                (Some(source), _) => format!(
                    "This chat bills an Anthropic API key ({source}), not your Claude subscription. {to_subscription}"
                ),
                (None, true) => format!(
                    "This chat bills the Anthropic API key saved in Settings. {to_subscription}"
                ),
                (None, false) => format!(
                    "No API key is saved in Settings, so this chat runs Claude as installed: a key your shell exports, or else your Claude sign-in. {to_subscription}"
                ),
            },
            false,
            BillingClick::Configure(Subscription),
        ),
        (ApiKey, Subscription) => (
            format!(
                "Set to Subscription, but Claude still bills an API key ({}) that RiWork does not remove, such as an apiKeyHelper in Claude's own settings.",
                source.unwrap_or("of its own")
            ),
            true,
            BillingClick::Configure(Subscription),
        ),
        (Subscription, ApiKey) if key_saved => (
            "Set to API key, but this Claude started before a key was saved and bills your Claude subscription. Switch to stop it; its next message starts on the key.".to_owned(),
            true,
            BillingClick::Restart,
        ),
        (Subscription, ApiKey) => (
            "This chat bills your Claude subscription: no API key is saved, so Claude uses your sign-in. Switch to enter a key in Settings and bill it instead.".to_owned(),
            false,
            BillingClick::EnterKey,
        ),
        (Subscription, Subscription) if key_saved => (
            "This chat bills your Claude subscription. Switch to your Anthropic API key; Claude restarts on the same conversation.".to_owned(),
            false,
            BillingClick::Configure(ApiKey),
        ),
        (Subscription, Subscription) => (
            "This chat bills your Claude subscription. Switch to enter an Anthropic API key in Settings and bill it instead.".to_owned(),
            false,
            BillingClick::EnterKey,
        ),
    };
    BillingControl {
        label: match shown {
            Subscription => "Billing: Subscription",
            ApiKey => "Billing: API key",
        },
        short: shown.label(),
        hint,
        on: shown == ApiKey,
        mismatch,
        click,
    }
}

/// Everything the meter knows, for its hover hint.
pub fn usage_details(usage: &Usage) -> String {
    let mut parts = vec![
        format!("Input {}", usage.input_tokens),
        format!("Output {}", usage.output_tokens),
    ];
    if usage.cached_input_tokens > 0 {
        parts.push(format!("Cached {}", usage.cached_input_tokens));
    }
    if let (Some(used), Some(window)) = (usage.context_used, usage.context_window) {
        parts.push(format!("Context {used} of {window}"));
    }
    if let Some(cost) = usage.cost_usd {
        parts.push(format!("{} (estimate)", cost_text(cost)));
    }
    parts.join(" · ")
}

/// The start of a thread id, enough to tell chats apart; the button copies the whole id.
pub fn short_thread_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meter_shows_the_context_when_it_is_known_and_the_spend_when_it_is_not() {
        let known = Usage {
            input_tokens: 90_000,
            output_tokens: 4_000,
            context_used: Some(84_000),
            context_window: Some(200_000),
            ..Usage::default()
        };
        assert_eq!(context_fraction(&known), Some(0.42));
        assert_eq!(percent_text(&known).as_deref(), Some("42%"));
        assert_eq!(usage_text(&known), "84k / 200k");
        let unknown = Usage {
            input_tokens: 12_300,
            output_tokens: 850,
            ..Usage::default()
        };
        assert_eq!(context_fraction(&unknown), None);
        assert_eq!(percent_text(&unknown), None);
        assert_eq!(usage_text(&unknown), "12k in · 850 out");
        // A window that was overrun still fills the meter exactly once.
        let over = Usage {
            context_used: Some(300),
            context_window: Some(200),
            ..Usage::default()
        };
        assert_eq!(context_fraction(&over), Some(1.0));
        let empty_window = Usage {
            context_used: Some(1),
            context_window: Some(0),
            ..Usage::default()
        };
        assert_eq!(context_fraction(&empty_window), None);
    }

    #[test]
    fn cost_is_approximate_in_the_row_and_called_an_estimate_in_the_hint() {
        assert_eq!(cost_text(0.4213), "≈ $0.42");
        assert_eq!(cost_text(344.31), "≈ $344.31");
        assert_eq!(cost_text(0.004), "≈ $0.004");
        let usage = Usage {
            cost_usd: Some(344.31),
            ..Usage::default()
        };
        assert!(usage_details(&usage).ends_with("≈ $344.31 (estimate)"));
    }

    #[test]
    fn the_ring_warns_from_70_and_turns_red_from_90_percent() {
        assert_eq!(
            [0.0, 0.56, 0.699, 0.7, 0.89, 0.9, 1.0].map(ring_tone),
            [
                RingTone::Normal,
                RingTone::Normal,
                RingTone::Normal,
                RingTone::Warning,
                RingTone::Warning,
                RingTone::Critical,
                RingTone::Critical
            ]
        );
    }

    #[test]
    fn the_ring_fills_clockwise_from_the_top_by_the_fraction_in_use() {
        assert!(ring_arc(0.0, (7.0, 7.0), 5.0).is_empty());
        let close = |(x, y): (f32, f32), (ex, ey): (f32, f32)| {
            (x - ex).abs() < 1e-4 && (y - ey).abs() < 1e-4
        };
        let quarter = ring_arc(0.25, (7.0, 7.0), 5.0);
        assert!(close(quarter[0], (7.0, 2.0)), "starts at the top");
        assert!(
            close(*quarter.last().unwrap(), (12.0, 7.0)),
            "a quarter ends on the right"
        );
        let half = ring_arc(0.5, (7.0, 7.0), 5.0);
        assert!(close(*half.last().unwrap(), (7.0, 12.0)));
        // Overrun clamps to one full turn, back at the top.
        let full = ring_arc(1.7, (7.0, 7.0), 5.0);
        assert!(close(*full.last().unwrap(), (7.0, 2.0)));
        assert!(full.len() > half.len());
        // Every point lies on the circle.
        assert!(
            full.iter().all(|(x, y)| {
                (((x - 7.0).powi(2) + (y - 7.0).powi(2)).sqrt() - 5.0).abs() < 1e-4
            })
        );
    }

    #[test]
    fn the_billing_switch_shows_what_claude_bills_and_its_click_moves_to_the_other() {
        use BillingClick::{Configure, EnterKey, Restart};
        use ClaudeBilling::{ApiKey, Subscription};
        let subscription = Account {
            api_key_source: None,
            plan: Some("Claude Max".into()),
        };
        let helper = Account {
            api_key_source: Some("apiKeyHelper".into()),
            plan: None,
        };
        let key = Account {
            api_key_source: Some("ANTHROPIC_API_KEY".into()),
            plan: None,
        };
        // (choice, what Claude said, key saved) -> (shown on, gold, click, hint says)
        for (billing, account, saved, on, mismatch, click, says) in [
            // Before Claude says: the choice.
            (
                Subscription,
                None,
                true,
                false,
                false,
                Configure(ApiKey),
                "Switch to your Anthropic API key",
            ),
            (
                Subscription,
                None,
                false,
                false,
                false,
                EnterKey,
                "enter an Anthropic API key in Settings",
            ),
            (
                ApiKey,
                None,
                true,
                true,
                false,
                Configure(Subscription),
                "saved in Settings",
            ),
            (
                ApiKey,
                None,
                false,
                true,
                false,
                Configure(Subscription),
                "runs Claude as installed",
            ),
            // What Claude says it bills.
            (
                Subscription,
                Some(&subscription),
                true,
                false,
                false,
                Configure(ApiKey),
                "bills your Claude subscription",
            ),
            (
                Subscription,
                Some(&helper),
                true,
                true,
                true,
                Configure(Subscription),
                "apiKeyHelper",
            ),
            (
                ApiKey,
                Some(&key),
                true,
                true,
                false,
                Configure(Subscription),
                "ANTHROPIC_API_KEY",
            ),
            (
                ApiKey,
                Some(&key),
                false,
                true,
                false,
                Configure(Subscription),
                "ANTHROPIC_API_KEY",
            ),
            (
                ApiKey,
                Some(&subscription),
                true,
                false,
                true,
                Restart,
                "started before a key was saved",
            ),
            // A user who never saved a key: the subscription Claude uses, no warning.
            (
                ApiKey,
                Some(&subscription),
                false,
                false,
                false,
                EnterKey,
                "no API key is saved",
            ),
        ] {
            let control = billing_control(billing, account, saved);
            let case = format!("{billing:?} {account:?} {saved}");
            assert_eq!(control.on, on, "{case}");
            assert_eq!(
                control.label,
                if on {
                    "Billing: API key"
                } else {
                    "Billing: Subscription"
                },
                "{case}"
            );
            assert_eq!(control.short, if on { "API key" } else { "Subscription" });
            assert_eq!(control.mismatch, mismatch, "{case}");
            assert_eq!(control.click, click, "{case}");
            assert!(control.hint.contains(says), "{case}: {}", control.hint);
            assert!(!control.hint.contains("sk-ant"));
        }
    }

    #[test]
    fn only_a_chat_that_bills_an_api_key_shows_the_badge() {
        assert_eq!(api_key_badge(None), None);
        let plan = Account {
            api_key_source: None,
            plan: Some("Claude Max".into()),
        };
        assert_eq!(api_key_badge(Some(&plan)), None);
        // An answer that says neither is no reason for a warning.
        assert_eq!(api_key_badge(Some(&Account::default())), None);
        let env = Account {
            api_key_source: Some("ANTHROPIC_API_KEY".into()),
            plan: None,
        };
        let (label, hint) = api_key_badge(Some(&env)).unwrap();
        assert_eq!(label, "API key");
        // The setting that keeps a shell's key out is named only where it helps.
        assert!(hint.contains("switch this chat to Subscription"), "{hint}");
        let helper = Account {
            api_key_source: Some("apiKeyHelper".into()),
            plan: None,
        };
        let (_, hint) = api_key_badge(Some(&helper)).unwrap();
        assert!(hint.contains("(apiKeyHelper)"), "{hint}");
        assert!(!hint.contains("Settings"), "{hint}");
    }

    fn option(id: &str, name: &str, efforts: &[&str], fast: bool, default: bool) -> ModelOption {
        ModelOption {
            id: id.into(),
            name: name.into(),
            description: format!("About {name}"),
            efforts: efforts.iter().map(|e| (*e).to_owned()).collect(),
            default_effort: efforts.first().map(|e| (*e).to_owned()),
            supports_fast: fast,
            is_default: default,
        }
    }

    fn list() -> Vec<ModelOption> {
        vec![
            option("sol", "Sol", &["low", "medium", "high", "max"], true, true),
            option("luna", "Luna", &["medium", "high"], false, false),
            option("haiku", "Haiku", &[], false, false),
        ]
    }

    #[test]
    fn the_selected_model_is_the_chosen_one_or_the_default_and_never_a_guess() {
        let models = list();
        assert_eq!(selected_model(&models, None).unwrap().id, "sol");
        assert_eq!(selected_model(&models, Some("")).unwrap().id, "sol");
        assert_eq!(selected_model(&models, Some("luna")).unwrap().id, "luna");
        // One the list does not know is not turned into the default.
        assert!(selected_model(&models, Some("typed-by-hand")).is_none());
        assert!(selected_model(&[], None).is_none());
        assert_eq!(model_label(&models, None), "Sol");
        assert_eq!(model_label(&models, Some("luna")), "Luna");
        assert_eq!(model_label(&models, Some("typed-by-hand")), "typed-by-hand");
        assert_eq!(model_label(&[], None), "model");
        assert_eq!(model_label(&[], Some("opus")), "opus");
    }

    #[test]
    fn the_effort_picker_offers_only_what_the_selected_model_takes() {
        let models = list();
        let names = |rows: Vec<EffortRow>| rows.into_iter().map(|r| r.effort).collect::<Vec<_>>();
        assert_eq!(
            names(effort_rows(&models, None, None, Provider::Codex)),
            ["low", "medium", "high", "max"]
        );
        assert_eq!(
            names(effort_rows(&models, Some("luna"), None, Provider::Codex)),
            ["medium", "high"]
        );
        // A model that takes none has no picker.
        assert!(effort_rows(&models, Some("haiku"), None, Provider::Claude).is_empty());
        assert!(!effort_available(&models, Some("haiku"), Provider::Claude));
        assert!(effort_available(&models, Some("luna"), Provider::Claude));
        // Without a list, or for a model not in it, the provider's usual ones.
        assert_eq!(
            effort_choices(&[], None, Provider::Codex),
            ["low", "medium", "high", "xhigh"]
        );
        assert!(effort_choices(&models, Some("typed"), Provider::Claude).contains(&"max".into()));
        assert!(effort_available(&[], None, Provider::Claude));
    }

    #[test]
    fn fast_shows_only_for_a_model_with_a_fast_mode() {
        let models = list();
        assert!(fast_available(&models, None), "the default model has it");
        assert!(fast_available(&models, Some("sol")));
        assert!(!fast_available(&models, Some("luna")));
        assert!(!fast_available(&models, Some("typed-by-hand")));
        assert!(!fast_available(&[], None), "no list, no toggle");
        assert_eq!(
            (fast_label(true), fast_label(false)),
            ("Fast on", "Fast off")
        );
    }

    #[test]
    fn choosing_a_model_moves_an_effort_it_does_not_take_to_its_default() {
        let models = list();
        // Taken: stays. None: stays none. Not taken: the new model's default.
        assert_eq!(
            model_choice(&models, "luna", Some("high")),
            ("luna".to_owned(), None)
        );
        assert_eq!(
            model_choice(&models, "luna", None),
            ("luna".to_owned(), None)
        );
        assert_eq!(
            model_choice(&models, "luna", Some("max")),
            ("luna".to_owned(), Some("medium".to_owned()))
        );
        // A model with no efforts leaves the effort alone (the driver ignores it), as does
        // one the list does not know.
        assert_eq!(
            model_choice(&models, "haiku", Some("max")),
            ("haiku".to_owned(), None)
        );
        assert_eq!(
            model_choice(&models, "x", Some("max")),
            ("x".to_owned(), None)
        );
    }
}
