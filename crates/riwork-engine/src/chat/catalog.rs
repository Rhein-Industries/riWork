//! The models a new chat, or a chat that switches provider, can choose from, read from what
//! earlier chats saved: the newest `Models` list a driver of that provider reported, and the
//! other model ids chats of that provider were set to. Muse also keeps catalogs discovered
//! before its first chat. Reading saved metadata never contacts the host or
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
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
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
    /// A discovery failure keeps default/manual choices usable. UI-only; it is
    /// not a provider/account eligibility error or a new protocol field.
    #[serde(skip)]
    pub warning: Option<String>,
}

/// Whether `info` is a chat of `provider` whose list is the one a new chat would see: any
/// Claude or Muse chat, a Codex chat only under the same account.
fn eligible(info: &ChatInfo, provider: Provider, account: Option<&str>) -> bool {
    info.provider == provider
        && (provider != Provider::Codex || info.codex_account_id.as_deref() == account)
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

/// The models chat `info`'s provider offers it, as its `&` list shows them: the newest list its
/// own driver reported, as its model menu shows it, else what a new chat of its project would
/// be offered (`saved`, as `riwork chat models` reads it). Saved metadata only: no host, no
/// provider.
pub fn chat_models(home: &Path, info: &ChatInfo) -> Vec<ModelOption> {
    let models = reported(home, &info.id, info.provider)
        .unwrap_or_else(|| saved(home, info.project_id.as_deref(), info.provider).supported);
    models
        .into_iter()
        .filter(|model| valid_model(&model.id))
        .collect()
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
        Provider::Muse => None,
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
    if provider == Provider::Muse && !found_list {
        result.supported = cached_muse(home);
    }
    result
        .configured
        .retain(|id| !result.supported.iter().any(|model| &model.id == id));
    result
}

/// A picker can query Muse before a chat exists. Keep the passive saved reader
/// for CLI/remote callers; a successful query is also available to them later.
/// This runs on a background executor, never the UI thread.
pub fn available(home: &Path, project: Option<&str>, provider: Provider) -> Catalog {
    let saved = saved(home, project, provider);
    if provider != Provider::Muse {
        return saved;
    }
    let query = super::launch::muse_discovery_config(home).and_then(super::muse::discover_models);
    merge_discovery(home, saved, query)
}

fn cache_path(home: &Path) -> std::path::PathBuf {
    home.join("chat-models").join("muse.json")
}

fn cached_muse(home: &Path) -> Vec<ModelOption> {
    let Ok(file) = File::open(cache_path(home)) else {
        return Vec::new();
    };
    serde_json::from_reader::<_, Vec<ModelOption>>(file.take(TAIL))
        .unwrap_or_default()
        .into_iter()
        .filter(|model| valid_model(&model.id))
        .collect()
}

fn save_muse(home: &Path, models: &[ModelOption]) -> Result<(), String> {
    let path = cache_path(home);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        serde_json::to_writer(&mut file, models).map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, &path).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn merge_discovery(
    home: &Path,
    mut catalog: Catalog,
    result: Result<Vec<ModelOption>, String>,
) -> Catalog {
    match result {
        Ok(models) => {
            catalog.warning = None;
            catalog.supported = models
                .into_iter()
                .filter(|model| valid_model(&model.id))
                .collect();
            let _ = save_muse(home, &catalog.supported);
            catalog
                .configured
                .retain(|id| !catalog.supported.iter().any(|model| &model.id == id));
        }
        Err(error) => {
            catalog.warning = Some(format!(
                "Could not load Muse models: {error}. Use the default or enter a model ID."
            ))
        }
    }
    catalog
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
            parent_id: None,
            user_title: None,
            first_user_message: None,
            provider_title: None,
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
            contributor_mode: Default::default(),
            contributor_opt_out: false,
            claude_billing: None,
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

#[cfg(test)]
mod discovery_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_muse_catalog_is_available_before_any_chat_and_failure_keeps_it_usable() {
        let home =
            std::env::temp_dir().join(format!("riwork-muse-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let model = ModelOption {
            id: "muse-wonder-1.0".into(),
            name: "Wonder".into(),
            efforts: vec!["high".into(), "max".into()],
            ..Default::default()
        };
        let mut draft = Catalog::default();
        draft.configured = vec![model.id.clone(), "custom-model".into()];
        let discovered = merge_discovery(&home, draft, Ok(vec![model.clone()]));
        assert_eq!(discovered.supported, [model.clone()]);
        assert_eq!(discovered.configured, ["custom-model"]);
        assert!(
            !home.join("chats").exists(),
            "discovery must not create a chat or history"
        );
        assert_eq!(
            std::fs::metadata(cache_path(&home))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let saved = saved(&home, None, Provider::Muse);
        assert_eq!(
            saved.supported,
            [model.clone()],
            "passive CLI/remote readers see discovery results"
        );
        let fallback = merge_discovery(&home, saved, Err("provider unavailable".into()));
        assert_eq!(fallback.supported, [model]);
        assert_eq!(
            fallback.error, None,
            "catalog failure does not block default/manual chat creation"
        );
        assert!(
            fallback
                .warning
                .as_deref()
                .unwrap()
                .contains("provider unavailable")
        );
        assert!(
            serde_json::to_value(&fallback)
                .unwrap()
                .get("warning")
                .is_none(),
            "no new public wire field"
        );
        let empty = merge_discovery(&home, fallback, Ok(vec![]));
        assert!(empty.supported.is_empty());
        assert!(
            empty.warning.is_none(),
            "a successful refresh settles the old warning"
        );
        assert!(
            cached_muse(&home).is_empty(),
            "an authoritative empty catalog clears the cache"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
