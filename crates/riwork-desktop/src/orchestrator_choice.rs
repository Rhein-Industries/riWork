//! How a new orchestrator runs, as Settings > Agent sessions and Project Settings offer
//! it: a mode, and for a chat its provider and model. The choice itself and how it is
//! resolved live in core (`settings::OrchestratorRuns`) and the engine
//! (`orchestrators::plan`); this is what both panels show.

use crate::{
    chat::model::Provider,
    settings::{AUTOMATIC_ORCHESTRATORS, OrchestratorRuns, recommended_model, valid_model_id},
};
use riwork_core::settings::Settings;

/// The chat providers in the order the panels list them.
pub const PROVIDERS: [Provider; 3] = [Provider::Codex, Provider::Claude, Provider::Muse];

/// A provider's chip id word: `codex`, `claude`, `muse`.
pub fn provider_word(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "codex",
        Provider::Claude => "claude",
        Provider::Muse => "muse",
    }
}

/// One model a chat orchestrator can be set to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    /// `None`: the provider's own default.
    pub model: Option<String>,
    pub label: String,
    /// The last word of the chip's id: `default`, `recommended` or `saved`.
    pub id: &'static str,
}

/// The models offered for `provider`: its own default, the model RiWork's routing prefers
/// for it, and `saved` when it is another one (typed earlier, or written by another
/// build), so a saved choice is always shown as chosen.
pub fn model_choices(provider: Provider, saved: Option<&str>) -> Vec<ModelChoice> {
    let recommended = recommended_model(provider);
    let mut choices = vec![
        ModelChoice {
            model: None,
            label: "Provider default".to_owned(),
            id: "default",
        },
        ModelChoice {
            model: Some(recommended.to_owned()),
            label: format!("{recommended} · recommended"),
            id: "recommended",
        },
    ];
    if let Some(saved) = saved.filter(|saved| *saved != recommended) {
        choices.push(ModelChoice {
            model: Some(saved.to_owned()),
            label: format!("{saved} · saved"),
            id: "saved",
        });
    }
    choices
}

/// A model id typed into "Other model ID", as it is saved, or why it cannot be.
pub fn typed_model(text: &str) -> Result<String, String> {
    let model = text.trim();
    if model.is_empty() {
        return Err("Type a model ID first.".to_owned());
    }
    if !valid_model_id(model) {
        return Err("A model ID has at most 100 characters and no control characters.".to_owned());
    }
    Ok(model.to_owned())
}

/// Settings > Agent sessions' "Chat provider": another provider starts on its own
/// default model, as a new chat does when its provider changes.
pub fn choose_provider(settings: &mut Settings, provider: Provider) {
    settings.choose_orchestrator_provider(provider);
}

/// Settings > Agent sessions' "Chat model" and "Other model".
pub fn choose_model(settings: &mut Settings, model: Option<String>) {
    settings.choose_orchestrator_model(model);
}

/// What Automatic does, in a sentence.
pub fn automatic_note() -> String {
    let [(first, first_model), (second, second_model)] = AUTOMATIC_ORCHESTRATORS;
    let label = crate::session_catalog::provider_label;
    format!(
        "Automatic starts a {} chat on {first_model}, the best delegator, or a {} chat on {second_model} when {} cannot start, and says so when it falls back.",
        label(first),
        label(second),
        label(first),
    )
}

/// A choice in a few words, for "Inherit app default · …".
pub fn short(runs: &OrchestratorRuns) -> String {
    match runs {
        OrchestratorRuns::Automatic => "Automatic".to_owned(),
        OrchestratorRuns::Terminal => "Terminal".to_owned(),
        OrchestratorRuns::Chat { provider, model } => format!(
            "{} chat{}",
            crate::session_catalog::provider_label(*provider),
            model
                .as_deref()
                .map(|model| format!(" · {model}"))
                .unwrap_or_default()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_provider_offers_its_default_its_routed_model_and_a_saved_other() {
        let ids = |choices: Vec<ModelChoice>| {
            choices
                .into_iter()
                .map(|choice| (choice.id, choice.model))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(model_choices(Provider::Muse, None)),
            [
                ("default", None),
                ("recommended", Some("muse-wonder-full-1.0".to_owned()))
            ]
        );
        assert_eq!(
            model_choices(Provider::Claude, Some("opus"))
                .into_iter()
                .map(|choice| choice.label)
                .collect::<Vec<_>>(),
            ["Provider default", "opus · recommended"]
        );
        assert_eq!(
            ids(model_choices(Provider::Codex, Some("gpt-6-luna"))),
            [
                ("default", None),
                ("recommended", Some("gpt-6.1-sol".to_owned())),
                ("saved", Some("gpt-6-luna".to_owned()))
            ]
        );
        assert_eq!(typed_model("  fable "), Ok("fable".to_owned()));
        assert!(typed_model("   ").is_err());
        assert!(typed_model(&"x".repeat(101)).is_err());
        assert!(automatic_note().contains("muse-wonder-full-1.0"));
        assert!(automatic_note().contains("gpt-6.1-sol"));
    }
}
