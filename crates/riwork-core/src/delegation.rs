//! How orchestrators spread work over worker shells and which models they prefer for it:
//! Settings' policy for every orchestrator, and a project's override for its own.
//!
//! The consumers are the orchestrator agents, which are told the policy of their scope
//! when they start or load their skill (`riwork_engine::orchestrators::delegation`).
//! RiWork itself never starts a worker because of it.
//!
//! Every field is optional, in Settings (`settings.json`'s `delegation_policy`) and in a
//! project (its `delegation_policy` in the store) alike: one that is not chosen falls
//! through, the project's to Settings' and Settings' to `Delegation::BUILT_IN`. So a file
//! from before the policy reads unchanged, and a default that is never chosen can improve
//! in a later build.

use crate::{
    provider::Provider,
    settings::{provider_label, recommended_model, valid_model_id},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use std::ops::RangeInclusive;

/// The soft target of worker shells an orchestrator runs at once, when nothing chooses it.
pub const DEFAULT_PARALLEL_SHELLS: u8 = 4;
/// The soft targets Settings and Project Settings offer and this build reads.
pub const PARALLEL_SHELLS: RangeInclusive<u8> = 1..=10;

/// The kinds of worker work the policy names a model for, as the skill's Model routing
/// table does: design and the hardest reasoning, and the workhorse implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    Design,
    Workhorse,
}

impl Work {
    pub const ALL: [Self; 2] = [Self::Design, Self::Workhorse];

    /// Its key in a saved policy.
    pub fn key(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Workhorse => "workhorse",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Design => "Design work",
            Self::Workhorse => "Workhorse work",
        }
    }

    /// The route the skill's Model routing table prefers for it.
    pub fn built_in(self) -> Route {
        match self {
            Self::Design => Route::recommended(Provider::Claude),
            Self::Workhorse => Route::recommended(Provider::Codex),
        }
    }
}

/// A harness and the model id it is started on, as `riwork shell create --harness H
/// --model M` and `riwork chat new --provider H --model M` take them. Saved as
/// `{"provider":"claude","model":"opus"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Route {
    pub provider: Provider,
    pub model: String,
}

impl Route {
    /// The provider on the model RiWork's routing prefers for it
    /// (`settings::recommended_model`): Claude on `opus`, Codex on `gpt-6.1-sol`, Muse on
    /// `muse-wonder-full-1.0`.
    pub fn recommended(provider: Provider) -> Self {
        Self {
            provider,
            model: recommended_model(provider).to_owned(),
        }
    }

    /// The routes Settings and Project Settings offer: each provider on its recommended
    /// model.
    pub fn offered() -> [Self; 3] {
        [Provider::Claude, Provider::Codex, Provider::Muse].map(Self::recommended)
    }

    /// "claude, model opus": the harness and model id as the CLI takes them.
    pub fn describe(&self) -> String {
        format!("{}, model {}", harness_word(self.provider), self.model)
    }

    /// "Claude · opus", for a chip.
    pub fn label(&self) -> String {
        format!("{} · {}", provider_label(self.provider), self.model)
    }
}

impl<'de> Deserialize<'de> for Route {
    /// Strict: a field this build does not know, a provider it lacks or a model id a
    /// session would refuse makes the route unreadable, so the policy keeps it as saved.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Shape {
            provider: Provider,
            model: String,
        }
        let Shape { provider, model } = Shape::deserialize(deserializer)?;
        if !valid_model_id(&model) {
            return Err(serde::de::Error::custom(format!(
                "invalid model id {model:?}"
            )));
        }
        Ok(Self { provider, model })
    }
}

fn harness_word(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "codex",
        Provider::Claude => "claude",
        Provider::Muse => "muse",
    }
}

/// A saved delegation policy: Settings' (every orchestrator's) or a project's override
/// (only its own orchestrator's). `None` is "not chosen here".
///
/// Reading never fails: a value this build cannot read (a soft target out of
/// `PARALLEL_SHELLS`, a provider it lacks, another build's key) reads as not chosen and
/// is kept as saved in `unreadable` until that field is chosen here. A saved policy that
/// is not an object at all reads as nothing chosen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DelegationPolicy {
    /// Fan independent work out across worker shells at once.
    pub spread_work: Option<bool>,
    /// About how many worker shells to run at once, within `PARALLEL_SHELLS`.
    pub parallel_shells: Option<u8>,
    pub design: Option<Route>,
    pub workhorse: Option<Route>,
    /// Saved keys this build cannot read, with their values.
    pub unreadable: Map<String, Value>,
}

const SPREAD_WORK: &str = "spread_work";
const PARALLEL: &str = "parallel_shells";

impl DelegationPolicy {
    /// Whether it chooses nothing and keeps nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn route(&self, work: Work) -> Option<&Route> {
        match work {
            Work::Design => self.design.as_ref(),
            Work::Workhorse => self.workhorse.as_ref(),
        }
    }

    /// Choose whether to spread work (`None`: follow what is beneath). Choosing replaces
    /// a saved value this build could not read.
    pub fn set_spread_work(&mut self, spread: Option<bool>) {
        self.spread_work = spread;
        self.unreadable.remove(SPREAD_WORK);
    }

    /// Choose the soft target, pulled into `PARALLEL_SHELLS`.
    pub fn set_parallel_shells(&mut self, shells: Option<u8>) {
        self.parallel_shells =
            shells.map(|shells| shells.clamp(*PARALLEL_SHELLS.start(), *PARALLEL_SHELLS.end()));
        self.unreadable.remove(PARALLEL);
    }

    pub fn set_route(&mut self, work: Work, route: Option<Route>) {
        match work {
            Work::Design => self.design = route,
            Work::Workhorse => self.workhorse = route,
        }
        self.unreadable.remove(work.key());
    }

    /// This policy over `beneath`: each field it chooses wins, the rest are `beneath`'s.
    pub fn over(&self, beneath: &Delegation) -> Delegation {
        Delegation {
            spread_work: self.spread_work.unwrap_or(beneath.spread_work),
            parallel_shells: self.parallel_shells.unwrap_or(beneath.parallel_shells),
            design: self
                .design
                .clone()
                .unwrap_or_else(|| beneath.design.clone()),
            workhorse: self
                .workhorse
                .clone()
                .unwrap_or_else(|| beneath.workhorse.clone()),
        }
    }

    /// What Settings or Project Settings say about saved values this build cannot read.
    /// `whose` names the file: "settings.json", "This project's".
    pub fn unreadable_note(&self, whose: &str) -> Option<String> {
        if self.unreadable.is_empty() {
            return None;
        }
        let values = self
            .unreadable
            .iter()
            .map(|(key, value)| format!("{key} = {value}"))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "{whose} delegation policy has {values}, which this RiWork cannot read. It is kept as saved, and what is beneath stands in for it until you choose here."
        ))
    }
}

impl Serialize for DelegationPolicy {
    /// What was kept unreadable, with what is chosen on top; a field not chosen is left
    /// out.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = self.unreadable.clone();
        let mut chosen = |key: &str, value: Option<Value>| {
            if let Some(value) = value {
                object.insert(key.to_owned(), value);
            }
        };
        chosen(SPREAD_WORK, self.spread_work.map(Value::from));
        chosen(PARALLEL, self.parallel_shells.map(Value::from));
        for work in Work::ALL {
            chosen(
                work.key(),
                self.route(work).map(|route| serde_json::json!(route)),
            );
        }
        object.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DelegationPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let Value::Object(object) = Value::deserialize(deserializer)? else {
            return Ok(Self::default());
        };
        let mut policy = Self::default();
        for (key, value) in object {
            let read = match key.as_str() {
                SPREAD_WORK => bool::deserialize(&value)
                    .ok()
                    .map(|spread| policy.spread_work = Some(spread)),
                PARALLEL => u8::deserialize(&value)
                    .ok()
                    .filter(|shells| PARALLEL_SHELLS.contains(shells))
                    .map(|shells| policy.parallel_shells = Some(shells)),
                "design" => Route::deserialize(&value)
                    .ok()
                    .map(|route| policy.design = Some(route)),
                "workhorse" => Route::deserialize(&value)
                    .ok()
                    .map(|route| policy.workhorse = Some(route)),
                _ => None,
            };
            if read.is_none() {
                policy.unreadable.insert(key, value);
            }
        }
        Ok(policy)
    }
}

/// A policy with every field decided: what an orchestrator is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delegation {
    pub spread_work: bool,
    pub parallel_shells: u8,
    pub design: Route,
    pub workhorse: Route,
}

impl Delegation {
    /// RiWork's own policy, beneath Settings: spread work, about four worker shells at
    /// once, and the skill's Model routing (Claude Opus 5.5 for design, Codex
    /// GPT-6.1-Sol as the workhorse).
    pub fn built_in() -> Self {
        Self {
            spread_work: true,
            parallel_shells: DEFAULT_PARALLEL_SHELLS,
            design: Work::Design.built_in(),
            workhorse: Work::Workhorse.built_in(),
        }
    }

    pub fn route(&self, work: Work) -> &Route {
        match work {
            Work::Design => &self.design,
            Work::Workhorse => &self.workhorse,
        }
    }
}

/// The policy of an orchestrator's scope: the project's override (none for the global
/// orchestrator), then Settings', then `Delegation::built_in`, field by field.
pub fn resolve(settings: &DelegationPolicy, project: Option<&DelegationPolicy>) -> Delegation {
    let global = settings.over(&Delegation::built_in());
    match project {
        Some(project) => project.over(&global),
        None => global,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(value: Value) -> DelegationPolicy {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn nothing_chosen_is_the_built_in_policy_of_todays_routing() {
        let built_in = Delegation::built_in();
        assert!(built_in.spread_work);
        assert_eq!(built_in.parallel_shells, DEFAULT_PARALLEL_SHELLS);
        assert_eq!(
            built_in.design,
            Route {
                provider: Provider::Claude,
                model: "opus".into()
            }
        );
        assert_eq!(
            built_in.workhorse,
            Route {
                provider: Provider::Codex,
                model: "gpt-6.1-sol".into()
            }
        );
        let nothing = DelegationPolicy::default();
        assert_eq!(resolve(&nothing, None), built_in);
        assert_eq!(resolve(&nothing, Some(&nothing)), built_in);
        assert!(nothing.is_empty());
        assert_eq!(serde_json::to_value(&nothing).unwrap(), json!({}));
    }

    #[test]
    fn a_project_choice_wins_over_settings_which_win_over_the_built_in_field_by_field() {
        let settings = read(json!({
            "spread_work": false,
            "parallel_shells": 6,
            "workhorse": {"provider": "muse", "model": "muse-wonder-full-1.0"}
        }));
        let project = read(json!({
            "parallel_shells": 2,
            "design": {"provider": "codex", "model": "gpt-6.1-sol"}
        }));
        let global = resolve(&settings, None);
        assert!(!global.spread_work);
        assert_eq!(global.parallel_shells, 6);
        assert_eq!(global.design, Work::Design.built_in());
        assert_eq!(global.workhorse, Route::recommended(Provider::Muse));

        let scoped = resolve(&settings, Some(&project));
        assert!(
            !scoped.spread_work,
            "Settings' choice where the project has none"
        );
        assert_eq!(scoped.parallel_shells, 2, "the project's own choice");
        assert_eq!(scoped.design, Route::recommended(Provider::Codex));
        assert_eq!(scoped.workhorse, Route::recommended(Provider::Muse));
        // A project that chooses nothing is Settings' policy.
        assert_eq!(
            resolve(&settings, Some(&DelegationPolicy::default())),
            global
        );
    }

    #[test]
    fn malformed_values_read_as_not_chosen_and_are_kept_until_chosen() {
        let saved = json!({
            "spread_work": "yes",
            "parallel_shells": 0,
            "design": {"provider": "gemini", "model": "pro"},
            "workhorse": {"provider": "codex", "model": "   "},
            "future_field": {"a": 1}
        });
        let mut policy = read(saved.clone());
        assert_eq!(policy.spread_work, None);
        assert_eq!(policy.parallel_shells, None);
        assert_eq!(policy.design, None);
        assert_eq!(policy.workhorse, None);
        assert_eq!(resolve(&policy, None), Delegation::built_in());
        assert_eq!(policy.unreadable.len(), 5);
        let note = policy.unreadable_note("settings.json").unwrap();
        assert!(note.contains(r#"spread_work = "yes""#), "{note}");
        // Written back as it was read.
        assert_eq!(serde_json::to_value(&policy).unwrap(), saved);

        // Too many, too few, a fraction and a negative are all out of range.
        for shells in [json!(11), json!(300), json!(2.5), json!(-1)] {
            let policy = read(json!({ "parallel_shells": shells }));
            assert_eq!(policy.parallel_shells, None, "{shells}");
        }
        // A route with a field this build does not know is unreadable too.
        let extra = read(json!({"design": {"provider": "claude", "model": "opus", "effort": 3}}));
        assert_eq!(extra.design, None);

        // Choosing a field, even "not chosen", replaces just its unreadable value.
        policy.set_spread_work(Some(true));
        policy.set_route(Work::Design, None);
        let written = serde_json::to_value(&policy).unwrap();
        assert_eq!(written["spread_work"], true);
        assert!(written.get("design").is_none());
        assert_eq!(written["parallel_shells"], 0);
        assert_eq!(written["future_field"], json!({"a": 1}));
        assert_eq!(written["workhorse"], saved["workhorse"]);

        // A policy that is not an object reads as nothing chosen.
        for odd in [json!("on"), json!(4), json!(null), json!([true])] {
            assert!(read(odd).is_empty());
        }
    }

    #[test]
    fn chosen_values_round_trip_and_the_soft_target_stays_in_range() {
        let mut policy = DelegationPolicy::default();
        policy.set_spread_work(Some(false));
        policy.set_parallel_shells(Some(99));
        policy.set_route(Work::Workhorse, Some(Route::recommended(Provider::Claude)));
        assert_eq!(policy.parallel_shells, Some(*PARALLEL_SHELLS.end()));
        let written = serde_json::to_value(&policy).unwrap();
        assert_eq!(
            written,
            json!({
                "spread_work": false,
                "parallel_shells": 10,
                "workhorse": {"provider": "claude", "model": "opus"}
            })
        );
        assert_eq!(read(written), policy);
        policy.set_parallel_shells(Some(0));
        assert_eq!(policy.parallel_shells, Some(1));
    }

    #[test]
    fn routes_read_as_the_cli_takes_them() {
        assert_eq!(
            Route::recommended(Provider::Claude).describe(),
            "claude, model opus"
        );
        assert_eq!(
            Route::recommended(Provider::Codex).label(),
            "Codex · gpt-6.1-sol"
        );
        assert_eq!(
            Route::offered().map(|route| route.provider),
            [Provider::Claude, Provider::Codex, Provider::Muse]
        );
    }
}
