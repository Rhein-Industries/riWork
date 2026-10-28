//! Claude billing presentation and explicit changes. The bridge carries presence
//! and provider-reported source names only; API key contents never reach the UI.
use std::{sync::mpsc, time::{Duration, SystemTime, UNIX_EPOCH}};

use nsh_core::prelude::*;
use riwork_engine::chat::{
    billing::{self, LimitBanner},
    model::{ChatCommand, ChatState, ClaudeBilling, Item, ItemBody, Provider, Transcript},
};
use serde::{Deserialize, Serialize};

use super::{Parts, query_view, worker::WorkerMsg};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitDto {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub limits: Vec<LimitDto>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct BillingActionDto {
    /// `settings`, `changed` or `restarted`.
    pub action: String,
    pub message: String,
}

fn name(billing: ClaudeBilling) -> &'static str {
    match billing { ClaudeBilling::Subscription => "subscription", ClaudeBilling::ApiKey => "api_key" }
}
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |value| value.as_secs()) }
fn expired(body: &ItemBody) -> bool {
    matches!(body, ItemBody::Notice { kind: Some(kind), resets_at: Some(at), .. } if kind.starts_with("rate_limit:") && *at <= now())
}

pub fn hide_notice(transcript: &Transcript, body: &ItemBody) -> bool {
    expired(body) || transcript.info.as_ref().is_some_and(|info| {
        billing::limit_banner(info.provider, body, transcript.account.as_ref(), false) == LimitBanner::Hidden
    })
}

/// Provider/account and notice replacements refresh this projection. Streaming
/// panel deliveries only clone the current result; expiry visits eligible
/// notices when a reset time is reached, not the entire conversation per token.
#[derive(Default)]
pub struct Projection {
    value: Option<BillingDto>,
    offers: Vec<LimitDto>,
    next_expiry: Option<u64>,
    checked_at: Option<u64>,
}

impl Projection {
    pub fn refresh<'a>(&mut self, transcript: &Transcript, notices: impl Iterator<Item = &'a Item>) {
        self.offers.clear();
        let Some(info) = transcript.info.as_ref().filter(|info| info.provider == Provider::Claude) else {
            self.value = None;
            self.refresh_expiry(now());
            return;
        };
        self.offers = notices.filter_map(|item| {
            if !matches!(billing::limit_banner(info.provider, &item.body, transcript.account.as_ref(), false), LimitBanner::Offer(_)) { return None; }
            let resets_at = match &item.body { ItemBody::Notice { resets_at, .. } => *resets_at, _ => None };
            Some(LimitDto { id: item.id.clone(), resets_at })
        }).collect();
        self.value = Some(BillingDto {
            selected: info.claude_billing.map(|value| name(value).into()),
            reported: transcript.account.as_ref().map(|account| if account.api_key_source.is_some() { "api_key" } else { "subscription" }.into()),
            source: transcript.account.as_ref().and_then(|account| account.api_key_source.clone()),
            limits: Vec::new(),
        });
        self.refresh_expiry(now());
    }

    fn refresh_expiry(&mut self, at: u64) {
        self.next_expiry = None;
        if let Some(value) = &mut self.value {
            value.limits = self.offers.iter().filter(|offer| offer.resets_at.is_none_or(|reset| reset > at)).cloned().collect();
            self.next_expiry = value.limits.iter().filter_map(|offer| offer.resets_at).min();
        }
        self.checked_at = Some(at);
    }

    pub fn snapshot(&mut self) -> Option<BillingDto> {
        let at = now();
        if self.checked_at.is_none_or(|last| at < last) || self.next_expiry.is_some_and(|reset| reset <= at) {
            self.refresh_expiry(at);
        }
        self.checked_at = Some(at);
        self.value.clone()
    }
}

enum Change { Settings, Configure(ClaudeBilling), Restart }

fn planned(transcript: &Transcript, default: ClaudeBilling, saved: bool, notice: Option<&str>) -> Result<(Change, bool), String> {
    let info = transcript.info.as_ref().ok_or("unavailable: the chat has not connected yet")?;
    if info.provider != Provider::Claude { return Err("refused: only Claude chats have a billing choice".into()); }
    if matches!(transcript.state, ChatState::Starting | ChatState::Running | ChatState::Waiting) {
        return Err("refused: wait for Claude to finish before changing billing".into());
    }
    let selected = info.claude_billing_or(default);
    if let Some(id) = notice {
        let item = transcript.items.iter().find(|item| item.id == id).ok_or("refused: the usage-limit notice is no longer available")?;
        if expired(&item.body) || !matches!(billing::limit_banner(Provider::Claude, &item.body, transcript.account.as_ref(), saved), LimitBanner::Offer(_)) {
            return Err("refused: this subscription-limit offer is no longer current".into());
        }
        return Ok((if !saved { Change::Settings } else if selected == ClaudeBilling::ApiKey { Change::Restart } else { Change::Configure(ClaudeBilling::ApiKey) }, info.state == ChatState::Idle));
    }
    let reported = transcript.account.as_ref().map(|account| if account.api_key_source.is_some() { ClaudeBilling::ApiKey } else { ClaudeBilling::Subscription }).unwrap_or(selected);
    let change = match (reported, selected, saved) {
        (ClaudeBilling::ApiKey, _, _) => Change::Configure(ClaudeBilling::Subscription),
        (ClaudeBilling::Subscription, _, false) => Change::Settings,
        (ClaudeBilling::Subscription, ClaudeBilling::ApiKey, true) => Change::Restart,
        (ClaudeBilling::Subscription, ClaudeBilling::Subscription, true) => Change::Configure(ClaudeBilling::ApiKey),
    };
    Ok((change, info.state == ChatState::Idle))
}

pub async fn action(parts: Parts, inbox: Option<mpsc::Sender<WorkerMsg>>, action: String, notice_id: Option<String>) -> Result<BillingActionDto, String> {
    if action != "toggle" && action != "use_api_key" { return Err("invalid: unknown billing action".into()); }
    let notice = if action == "use_api_key" { Some(notice_id.ok_or("invalid: the limit notice is required")?) } else { None };
    let saved = crate::settings_aux::ApiKey::refresh().await?.saved == Some(true);
    let backend = parts.backend.clone();
    let default = crate::service::blocking(move || Ok(riwork_core::settings::claude_chat_billing(&backend.home()?))).await?;
    let target = notice.clone();
    let (change, was_idle) = query_view(inbox.clone(), move |view| planned(&view.fold.transcript, default, saved, target.as_deref())).await??;
    let (action, message) = match change {
        Change::Settings => return Ok(BillingActionDto { action: "settings".into(), message: String::new() }),
        Change::Restart => {
            parts.exchange(ChatCommand::Stop, false).await?;
            ("restarted", "Stopped: this chat's next message starts Claude on the saved API key.")
        }
        Change::Configure(billing) => {
            parts.exchange(ChatCommand::Configure {
                model: None, effort: None, approval_mode: None, fast: None,
                contributor_mode: None, contributor_opt_out: None, claude_billing: Some(billing),
            }, false).await?;
            ("changed", if billing == ClaudeBilling::Subscription { "This chat is set to your Claude subscription." } else { "This chat is set to your Anthropic API key." })
        }
    };
    if let Some(notice_id) = notice {
        // An idle Claude restarts after Configure. Wait for its event, then inspect
        // the current transcript again so a message sent meanwhile is never replayed.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let ready = query_view(inbox.clone(), move |view| {
                let transcript = &view.fold.transcript;
                transcript.info.as_ref().is_some_and(|info| info.claude_billing_or(default) == ClaudeBilling::ApiKey
                    && (!was_idle || info.state == ChatState::Idle || info.state == ChatState::Stopped))
            }).await?;
            if ready { break; }
            if tokio::time::Instant::now() >= deadline {
                return Ok(BillingActionDto { action: action.into(), message: "API key billing is selected. Claude is still starting; your message was not sent again.".into() });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let prompt = query_view(inbox, move |view| billing::rejected_prompt(&view.fold.transcript.items, &notice_id)).await?;
        if let Some(text) = prompt {
            parts.exchange(ChatCommand::Send { text }, true).await?;
            return Ok(BillingActionDto { action: action.into(), message: "This chat now bills your API key; your message was sent again.".into() });
        }
        return Ok(BillingActionDto { action: action.into(), message: "API key billing is selected. Nothing was sent again because the conversation had already moved on. Send your message when you are ready.".into() });
    }
    Ok(BillingActionDto { action: action.into(), message: message.into() })
}
