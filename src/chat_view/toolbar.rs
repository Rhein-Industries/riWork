//! The words and numbers of the chat toolbar: the approval modes, the choices for model and
//! effort, the context meter and the thread id.

use crate::chat::model::{ApprovalMode, Provider, Usage};

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

/// The reasoning efforts offered for a provider. The host passes the name on as it is, so
/// a level the installed CLI does not know is the provider's to refuse.
pub fn efforts(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Codex => &["low", "medium", "high", "xhigh"],
        Provider::Claude => &["low", "medium", "high", "xhigh", "max"],
    }
}

/// Models offered as buttons beside the text field. Claude's aliases keep working as models
/// change; Codex model names change often enough that only the text field is offered.
pub fn model_suggestions(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Codex => &[],
        Provider::Claude => &["opus", "sonnet", "haiku"],
    }
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

/// The meter's words: the context in use, or what was spent when the window is not known.
pub fn usage_text(usage: &Usage) -> String {
    match (
        context_fraction(usage),
        usage.context_used,
        usage.context_window,
    ) {
        (Some(fraction), Some(used), Some(window)) => format!(
            "{}% · {} / {}",
            (fraction * 100.0).round() as u32,
            tokens(used),
            tokens(window)
        ),
        _ => format!(
            "{} in · {} out",
            tokens(usage.input_tokens),
            tokens(usage.output_tokens)
        ),
    }
}

/// Claude's own figure for what a chat cost. It is an estimate, never a bill.
pub fn cost_text(cost_usd: f64) -> String {
    if cost_usd >= 0.01 {
        format!("≈ ${cost_usd:.2} (estimate)")
    } else {
        format!("≈ ${cost_usd:.3} (estimate)")
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
        assert_eq!(usage_text(&known), "42% · 84k / 200k");
        let unknown = Usage {
            input_tokens: 12_300,
            output_tokens: 850,
            ..Usage::default()
        };
        assert_eq!(context_fraction(&unknown), None);
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
    fn cost_is_always_called_an_estimate() {
        assert_eq!(cost_text(0.4213), "≈ $0.42 (estimate)");
        assert_eq!(cost_text(12.0), "≈ $12.00 (estimate)");
        assert_eq!(cost_text(0.004), "≈ $0.004 (estimate)");
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
}
