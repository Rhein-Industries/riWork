//! Settings form projections and strict partial policy edits. Core remains the
//! authority for inheritance, recommended routes and forward-compatible storage.
use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, PropError, TsTypes},
    prelude::*,
};
use riwork_core::{
    delegation::{self, DelegationPolicy, Route, Work},
    provider::Provider,
    settings::{self, Settings},
};
use serde::{Deserialize, Deserializer, Serialize};

pub(crate) fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteDto {
    pub provider: String,
    pub model: String,
}
impl From<&Route> for RouteDto {
    fn from(route: &Route) -> Self {
        Self {
            provider: provider_word(route.provider).into(),
            model: route.model.clone(),
        }
    }
}
impl RouteDto {
    pub(crate) fn checked(&self) -> Result<Route, String> {
        let route = Route {
            provider: parse_provider(&self.provider)?,
            model: self.model.clone(),
        };
        route.check().map_err(|error| format!("invalid: {error}"))?;
        Ok(route)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RouteChoiceDto {
    pub id: String,
    pub label: String,
    pub route: RouteDto,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ModelChoiceDto {
    pub id: String,
    pub model: Option<String>,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct DelegationDto {
    pub spread_work: bool,
    pub parallel_shells: u32,
    pub design: RouteDto,
    pub workhorse: RouteDto,
    pub design_choices: Vec<RouteChoiceDto>,
    pub workhorse_choices: Vec<RouteChoiceDto>,
    pub unreadable_note: Option<String>,
}
impl DelegationDto {
    pub fn global(settings: &Settings) -> Self {
        Self::new(&settings.delegation_policy, None)
    }
    pub fn new(global: &DelegationPolicy, project: Option<&DelegationPolicy>) -> Self {
        let effective = delegation::resolve(global, project);
        let selected = project.unwrap_or(global);
        Self {
            spread_work: effective.spread_work,
            parallel_shells: u32::from(effective.parallel_shells),
            design: (&effective.design).into(),
            workhorse: (&effective.workhorse).into(),
            design_choices: route_choices(selected.design.as_ref()),
            workhorse_choices: route_choices(selected.workhorse.as_ref()),
            unreadable_note: selected.unreadable_note(if project.is_some() {
                "This project's"
            } else {
                "settings.json"
            }),
        }
    }
}

pub fn provider_word(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "codex",
        Provider::Claude => "claude",
        Provider::Muse => "muse",
    }
}
pub(crate) fn parse_provider(provider: &str) -> Result<Provider, String> {
    match provider {
        "codex" => Ok(Provider::Codex),
        "claude" => Ok(Provider::Claude),
        "muse" => Ok(Provider::Muse),
        _ => Err(format!("invalid: unknown chat provider {provider:?}")),
    }
}
pub(crate) fn checked_model(model: &str) -> Result<String, String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("invalid: Type a model ID first.".into());
    }
    if !settings::valid_model_id(model) {
        return Err(
            "invalid: A model ID has at most 100 characters and no control characters.".into(),
        );
    }
    Ok(model.to_owned())
}
pub fn model_choices(provider: Provider, saved: Option<&str>) -> Vec<ModelChoiceDto> {
    let recommended = settings::recommended_model(provider);
    let mut choices = vec![
        ModelChoiceDto {
            id: "default".into(),
            model: None,
            label: "Provider default".into(),
        },
        ModelChoiceDto {
            id: "recommended".into(),
            model: Some(recommended.into()),
            label: format!("{recommended} · recommended"),
        },
    ];
    if let Some(saved) = saved.filter(|saved| *saved != recommended) {
        choices.push(ModelChoiceDto {
            id: "saved".into(),
            model: Some(saved.into()),
            label: format!("{saved} · saved"),
        });
    }
    choices
}
pub fn route_choices(saved: Option<&Route>) -> Vec<RouteChoiceDto> {
    let mut choices = Route::offered()
        .iter()
        .map(|route| RouteChoiceDto {
            id: provider_word(route.provider).into(),
            label: route.label(),
            route: route.into(),
        })
        .collect::<Vec<_>>();
    if let Some(saved) = saved.filter(|saved| {
        !choices
            .iter()
            .any(|choice| choice.route == RouteDto::from(*saved))
    }) {
        choices.push(RouteChoiceDto {
            id: "saved".into(),
            label: format!("{} · saved", saved.label()),
            route: saved.into(),
        });
    }
    choices
}

/// A missing field is untouched; explicit null removes just that override.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationPatch {
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub spread_work: Option<Option<bool>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub parallel_shells: Option<Option<u8>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub design: Option<Option<RouteDto>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub workhorse: Option<Option<RouteDto>>,
}
impl DelegationPatch {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self
            .parallel_shells
            .flatten()
            .is_some_and(|n| !delegation::PARALLEL_SHELLS.contains(&n))
        {
            return Err("invalid: Parallel worker shells must be between 1 and 10.".into());
        }
        for route in [self.design.as_ref(), self.workhorse.as_ref()]
            .into_iter()
            .flatten()
            .flatten()
        {
            route.checked()?;
        }
        Ok(())
    }
    pub(crate) fn apply(&self, policy: &mut DelegationPolicy) {
        if let Some(value) = self.spread_work {
            policy.set_spread_work(value);
        }
        if let Some(value) = self.parallel_shells {
            policy.set_parallel_shells(value);
        }
        for (work, value) in [
            (Work::Design, self.design.as_ref()),
            (Work::Workhorse, self.workhorse.as_ref()),
        ] {
            if let Some(value) = value {
                policy
                    .set_route(
                        work,
                        value
                            .as_ref()
                            .map(|route| route.checked().expect("validated route")),
                    )
                    .expect("validated route");
            }
        }
    }
}
impl JsValue for DelegationPatch {
    fn ts_type(types: &mut TsTypes) -> String {
        types.declare("DelegationPatch", |types| format!(
        "export interface DelegationPatch {{ spreadWork?: boolean | null; parallelShells?: number | null; design?: {} | null; workhorse?: {} | null; }}", RouteDto::ts_type(types), RouteDto::ts_type(types)))
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        serde_json::from_value(JsonValue::from_js(value)?)
            .map_err(|error| PropError(format!("invalid: {error}")))
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        serde_json::to_value(self)
            .map_err(|error| PropError(error.to_string()))?
            .to_js(host)
    }
}
