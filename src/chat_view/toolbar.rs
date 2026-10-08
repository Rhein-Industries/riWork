//! The words and numbers of the chat toolbar: the approval modes, the choices for model and
//! effort, the context meter and the thread id.

use crate::chat::model::{ApprovalMode, ModelOption, Provider, Usage};

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
    }
}

/// Models offered as buttons beside the text field, when the driver has not listed its
/// models. Claude's aliases keep working as models change; Codex model names change often
/// enough that only the text field is offered.
pub fn model_suggestions(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Codex => &[],
        Provider::Claude => &["opus", "sonnet", "haiku"],
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
    fn token_counts_stay_short() {
        assert_eq!(
            [
                0, 842, 1_000, 1_234, 9_999, 12_345, 200_000, 999_999, 1_234_567
            ]
            .map(tokens),
            [
                "0", "842", "1.0k", "1.2k", "10.0k", "12k", "200k", "999k", "1.2M"
            ]
        );
    }

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
    fn the_hover_hint_lists_what_the_provider_reported() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cached_input_tokens: 7,
            context_used: Some(15),
            context_window: Some(100),
            cost_usd: None,
        };
        assert_eq!(
            usage_details(&usage),
            "Input 10 · Output 5 · Cached 7 · Context 15 of 100"
        );
        assert_eq!(usage_details(&Usage::default()), "Input 0 · Output 0");
    }

    #[test]
    fn modes_efforts_and_models_are_listed_per_provider() {
        assert_eq!(
            MODES.map(|(mode, ..)| mode),
            [
                ApprovalMode::Supervised,
                ApprovalMode::AutoEdit,
                ApprovalMode::Full,
                ApprovalMode::Plan
            ]
        );
        assert_eq!(mode_label(ApprovalMode::Full), "Full");
        assert_eq!(mode_label(ApprovalMode::AutoEdit), "Auto-edit");
        assert!(efforts(Provider::Codex).contains(&"high"));
        assert!(efforts(Provider::Claude).contains(&"max"));
        assert!(model_suggestions(Provider::Codex).is_empty());
        assert_eq!(
            model_suggestions(Provider::Claude),
            ["opus", "sonnet", "haiku"]
        );
        assert_eq!(short_thread_id("0123456789abcdef"), "01234567");
        assert_eq!(short_thread_id("abc"), "abc");
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
    fn the_model_picker_lists_names_marks_the_default_and_the_one_in_use() {
        let models = list();
        let rows = model_rows(&models, Some("luna"));
        assert_eq!(
            rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
            ["Sol (default)", "Luna", "Haiku"]
        );
        assert_eq!(
            rows.iter().map(|r| r.current).collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(rows[1].id, "luna");
        assert_eq!(rows[1].detail.as_deref(), Some("About Luna"));
        // A chat that chose nothing runs the default.
        let rows = model_rows(&models, None);
        assert_eq!(
            rows.iter().map(|r| r.current).collect::<Vec<_>>(),
            [true, false, false]
        );
        // A default whose name says it is the default is not marked twice.
        let mut named = list();
        named[0].name = "Default (recommended)".into();
        assert_eq!(model_rows(&named, None)[0].label, "Default (recommended)");
        // A description that is empty is no second line.
        let mut bare = list();
        bare[1].description.clear();
        assert_eq!(model_rows(&bare, None)[1].detail, None);
        // Nothing is current for a model the list does not know.
        assert!(model_rows(&models, Some("x")).iter().all(|r| !r.current));
        assert!(model_rows(&[], None).is_empty());
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
    fn the_current_effort_is_the_chosen_one_or_else_the_models_default() {
        let models = list();
        let rows = effort_rows(&models, Some("luna"), None, Provider::Codex);
        assert_eq!(
            rows.iter()
                .map(|r| (r.label.as_str(), r.current))
                .collect::<Vec<_>>(),
            [("medium (default)", true), ("high", false)]
        );
        let rows = effort_rows(&models, Some("luna"), Some("high"), Provider::Codex);
        assert_eq!(
            rows.iter().map(|r| r.current).collect::<Vec<_>>(),
            [false, true]
        );
        assert_eq!(effort_label(&models, Some("luna"), None), "medium");
        assert_eq!(effort_label(&models, Some("luna"), Some("high")), "high");
        assert_eq!(effort_label(&models, Some("haiku"), None), "effort");
        assert_eq!(effort_label(&[], None, None), "effort");
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
