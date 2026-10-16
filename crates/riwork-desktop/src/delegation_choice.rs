//! How orchestrators spread work over worker shells and which models they prefer for it,
//! as Settings > Agent sessions and Project Settings offer it. The policy itself and how
//! it is resolved live in core (`delegation`); this is what both panels show.

pub use riwork_core::delegation::{
    Delegation, DelegationPolicy, PARALLEL_SHELLS, Route, Work, resolve,
};

use crate::orchestrator_choice::provider_word;

pub const SPREAD_TITLE: &str = "Spread work";
pub const SPREAD_DESCRIPTION: &str = "Orchestrators fan independent tasks out to several worker shells at once instead of working through them one by one. Off, they keep to one worker at a time unless asked.";
pub const PARALLEL_TITLE: &str = "Parallel worker shells";
pub const PARALLEL_DESCRIPTION: &str = "A soft target for how many worker shells an orchestrator runs at once when work can be spread. RiWork starts none by itself; the orchestrator decides.";

/// What the harness and model of a kind of work are preferred for.
pub fn route_description(work: Work) -> &'static str {
    match work {
        Work::Design => {
            "The harness and model orchestrators prefer for architecture, the hardest reasoning, review and design."
        }
        Work::Workhorse => {
            "The harness and model orchestrators prefer for high-volume implementation, tests and mechanical work."
        }
    }
}

/// Settings' policy with every field decided: what the global orchestrator is told, and
/// what a project's Inherit stands for.
pub fn settings_policy(settings: &riwork_core::settings::Settings) -> Delegation {
    resolve(&settings.delegation_policy, None)
}

pub fn on_off(spread: bool) -> &'static str {
    if spread { "On" } else { "Off" }
}

/// One route a kind of work can be set to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteChoice {
    pub route: Route,
    pub label: String,
    /// The last word of the chip's id: `claude`, `codex`, `muse` or `saved`.
    pub id: &'static str,
}

/// The routes offered (`Route::offered`), and `saved` when it is another one (written by
/// hand or by another build), so a saved choice is always shown as chosen.
pub fn route_choices(saved: Option<&Route>) -> Vec<RouteChoice> {
    let mut choices = Route::offered()
        .into_iter()
        .map(|route| RouteChoice {
            label: route.label(),
            id: provider_word(route.provider),
            route,
        })
        .collect::<Vec<_>>();
    if let Some(saved) = saved.filter(|saved| !choices.iter().any(|choice| &choice.route == *saved))
    {
        choices.push(RouteChoice {
            route: saved.clone(),
            label: format!("{} · saved", saved.label()),
            id: "saved",
        });
    }
    choices
}

/// The soft target one fewer or one more, or `None` past the end of `PARALLEL_SHELLS`.
pub fn step_shells(shells: u8, more: bool) -> Option<u8> {
    let next = if more {
        shells.checked_add(1)?
    } else {
        shells.checked_sub(1)?
    };
    PARALLEL_SHELLS.contains(&next).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::Provider;

    #[test]
    fn the_offered_routes_come_first_and_a_saved_other_one_is_added_to_show_it() {
        let offered = route_choices(None);
        assert_eq!(
            offered.iter().map(|choice| choice.id).collect::<Vec<_>>(),
            ["claude", "codex", "muse"]
        );
        assert_eq!(offered[0].label, "Claude · opus");
        assert_eq!(
            route_choices(Some(&Route::recommended(Provider::Muse))),
            offered
        );
        let fable = Route {
            provider: Provider::Claude,
            model: "fable".into(),
        };
        let with_saved = route_choices(Some(&fable));
        assert_eq!(with_saved.len(), 4);
        assert_eq!(with_saved[3].id, "saved");
        assert_eq!(with_saved[3].label, "Claude · fable · saved");
        assert_eq!(with_saved[3].route, fable);
    }

    #[test]
    fn the_soft_target_steps_within_its_range() {
        assert_eq!(step_shells(4, true), Some(5));
        assert_eq!(step_shells(4, false), Some(3));
        assert_eq!(step_shells(*PARALLEL_SHELLS.start(), false), None);
        assert_eq!(step_shells(*PARALLEL_SHELLS.end(), true), None);
        assert_eq!(step_shells(0, false), None);
        assert_eq!(step_shells(u8::MAX, true), None);
    }
}
