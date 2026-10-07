//! The models a new chat, or a chat that switches provider, can choose from, read from what
//! earlier chats saved: the newest `Models` list a driver of that provider reported, and the
//! other model ids chats of that provider were set to. Reading it never contacts the host or
//! starts a provider, so the list can be offered before any agent of the provider runs; the
//! provider checks the choice when it starts.

use super::{
    log,
    model::{ChatEvent, ChatInfo, ModelOption, Provider},
    wire::Envelope,
};
use crate::sessions;
use serde::Serialize;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

/// How many of the newest chats of a provider are looked at.
const CHATS: usize = 12;
/// How much of the end of a chat's log is read for its models.
const TAIL: u64 = 2 * 1024 * 1024;

/// What one provider offers, as far as the saved chats tell.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Catalog {
    /// The newest list a driver of the provider reported, in its order.
    pub supported: Vec<ModelOption>,
    /// Model ids chats were set to that the list does not have (typed by hand, or from an
    /// older list), newest first. Never claimed to be supported.
    pub configured: Vec<String>,
    /// The Codex account the list is for (a RiWork account id; none for the system default
    /// and for Claude).
    pub account: Option<String>,
    /// That account as the person knows it.
    pub account_label: Option<String>,
    /// Why the provider cannot be offered (a Codex account that cannot be used).
    pub error: Option<String>,
}

/// Whether `info` is a chat of `provider` whose list is the one a new chat would see: any
/// Claude chat, a Codex chat only under the same account.
fn eligible(info: &ChatInfo, provider: Provider, account: Option<&str>) -> bool {
    info.provider == provider
        && (provider == Provider::Claude || info.codex_account_id.as_deref() == account)
}

/// What one complete line of chat `id`'s log says about its models: a list, or the provider
/// the chat runs from here on (an `Info`).
enum Line {
    Models(Vec<ModelOption>),
    Provider(Provider),
    Nothing,
}

fn decode(id: &str, line: &[u8]) -> Line {
    if line.last() != Some(&b'\n') {
        return Line::Nothing;
    }
    let Ok(envelope) = serde_json::from_slice::<Envelope>(line) else {
        return Line::Nothing;
    };
    if envelope.chat_id != id {
        return Line::Nothing;
    }
    match envelope.event {
        ChatEvent::Models { models } => Line::Models(models),
        ChatEvent::Info { info } => Line::Provider(info.provider),
        _ => Line::Nothing,
    }
}

/// The newest list `provider`'s driver reported in chat `id`, read from a bounded tail of the
/// log without its incomplete last line. A list reported while the chat ran another provider
/// is not taken, nor one from before the chat last moved to `provider` (a tail that starts
/// before any `Info` counts as `provider`'s). Opening a `ChatLog` would repair the log's tail;
/// this reader never writes.
fn reported(home: &Path, id: &str, provider: Provider) -> Option<Vec<ModelOption>> {
    let dir = log::chat_dir(home, id)?;
    let mut file = File::open(dir.join("events.jsonl")).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = BufReader::new(file.take(length - start));
    if start > 0 {
        reader.skip_until(b'\n').ok()?;
    }
    let mut latest = None;
    let mut running: Option<Provider> = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 || line.last() != Some(&b'\n') {
            break;
        }
        match decode(id, &line) {
            Line::Models(models) if running.is_none_or(|running| running == provider) => {
                latest = Some(models)
            }
            Line::Models(_) | Line::Nothing => {}
            Line::Provider(now) => {
                if now != provider || running.is_some_and(|before| before != now) {
                    latest = None;
                }
                running = Some(now);
            }
        }
    }
    latest
}

/// A model id a chat may be set to: one line of at most 100 characters.
pub fn valid_model(id: &str) -> bool {
    !id.trim().is_empty() && id.chars().count() <= 100 && !id.chars().any(char::is_control)
}

/// What `provider` offers to a chat of `project` (or of no project): the newest saved list of
/// a chat that runs under the account a new chat there would get, and the other ids such chats
/// were set to.
pub fn saved(home: &Path, project: Option<&str>, provider: Provider) -> Catalog {
    let binding = match provider {
        Provider::Codex => match sessions::selected_codex_binding(home, project) {
            Ok(binding) => Some(binding),
            Err(error) => {
                return Catalog {
                    error: Some(error),
                    ..Default::default()
                };
            }
        },
        Provider::Claude => None,
    };
    let mut result = Catalog {
        account: binding.as_ref().and_then(|binding| binding.id.clone()),
        account_label: binding
            .map(|binding| binding.label.unwrap_or_else(|| "System default".into())),
        ..Default::default()
    };
    let mut found_list = false;
    let infos = log::read_infos(home);
    for info in infos
        .iter()
        .rev()
        .filter(|info| eligible(info, provider, result.account.as_deref()))
        .take(CHATS)
    {
        if let Some(id) = &info.model
            && valid_model(id)
            && !result.configured.contains(id)
        {
            result.configured.push(id.clone());
        }
        if !found_list && let Some(models) = reported(home, &info.id, provider) {
            found_list = true;
            result.supported = models
                .into_iter()
                .filter(|model| valid_model(&model.id))
                .collect();
        }
    }
    result
        .configured
        .retain(|id| !result.supported.iter().any(|model| &model.id == id));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ApprovalMode, ChatState};

    fn line(id: &str, event: ChatEvent) -> Vec<u8> {
        let mut line = serde_json::to_vec(&Envelope {
            chat_id: id.into(),
            seq: 1,
            event,
        })
        .unwrap();
        line.push(b'\n');
        line
    }

    fn option(id: &str) -> ModelOption {
        ModelOption {
            id: id.into(),
            name: id.to_uppercase(),
            ..Default::default()
        }
    }

    fn info(id: &str, provider: Provider) -> ChatInfo {
        ChatInfo {
            id: id.into(),
            provider,
            project_id: None,
            worktree_id: None,
            cwd: "/w".into(),
            title: "t".into(),
            created_at_unix: 1,
            provider_thread_id: None,
            model: None,
            effort: None,
            fast: false,
            approval_mode: ApprovalMode::Supervised,
            codex_account_id: None,
            state: ChatState::Stopped,
            orchestrator: None,
            carried_over: None,
        }
    }

    #[test]
    fn saved_model_metadata_requires_the_exact_uuid_and_a_complete_line() {
        let id = uuid::Uuid::from_u128(0xabcdef).to_string();
        let other = uuid::Uuid::from_u128(2).to_string();
        let mut whole = line(
            &id,
            ChatEvent::Models {
                models: vec![option("fixture")],
            },
        );
        assert!(matches!(
            decode(&id, &whole),
            Line::Models(models) if models[0].id == "fixture"
        ));
        assert!(matches!(decode(&other, &whole), Line::Nothing));
        whole.pop();
        assert!(matches!(decode(&id, &whole), Line::Nothing));
        assert!(log::chat_dir(Path::new("/fixture"), "../escape").is_none());
        assert!(log::chat_dir(Path::new("/fixture"), &id.to_uppercase()).is_none());
    }

    #[test]
    fn a_chat_that_switched_provider_offers_only_the_list_of_the_provider_it_has_now() {
        let home = std::env::temp_dir().join(format!("riwork-catalog-{}", uuid::Uuid::new_v4()));
        let id = uuid::Uuid::new_v4().to_string();
        let dir = log::chat_dir(&home, &id).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let claude = info(&id, Provider::Claude);
        std::fs::write(dir.join("info.json"), serde_json::to_vec(&claude).unwrap()).unwrap();
        let mut events = line(
            &id,
            ChatEvent::Info {
                info: info(&id, Provider::Codex),
            },
        );
        events.extend(line(
            &id,
            ChatEvent::Models {
                models: vec![option("gpt-5.5")],
            },
        ));
        // Switched to Claude, which has not listed its models yet.
        events.extend(line(
            &id,
            ChatEvent::Info {
                info: claude.clone(),
            },
        ));
        std::fs::write(dir.join("events.jsonl"), &events).unwrap();
        assert_eq!(saved(&home, Some("p"), Provider::Claude).supported, []);
        assert_eq!(
            reported(&home, &id, Provider::Claude),
            None,
            "Codex's list is not Claude's"
        );

        events.extend(line(
            &id,
            ChatEvent::Models {
                models: vec![option("opus")],
            },
        ));
        std::fs::write(dir.join("events.jsonl"), &events).unwrap();
        assert_eq!(
            saved(&home, Some("p"), Provider::Claude).supported,
            [option("opus")]
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
