//! Composer suggestions from the existing engine catalog. A request comes only
//! from an explicitly typed sigil; the caller supplies no path or provider.
//! File walks stay under the live chat's cwd and share the engine's bounded,
//! ignore-aware listing cache. Model reads use saved metadata, never discovery.
use std::{path::PathBuf, sync::{Arc, Mutex, OnceLock}, time::Instant};

use nsh_core::prelude::*;
use riwork_engine::chat::{catalog, log, model::{ChatInfo, ModelOption}, suggest::{self, Sigil}};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ChatSuggestion {
    pub title: String,
    pub detail: String,
    pub insert: String,
    pub body: Vec<String>,
    pub effect: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ChatSuggestionResult {
    /// All ranges use the input's UTF-16 coordinates, not Rust byte offsets.
    pub start: u32,
    pub end: u32,
    pub sigil: String,
    pub root: String,
    pub items: Vec<ChatSuggestion>,
    pub truncated: bool,
}

struct ModelSources {
    home: PathBuf,
    project: Option<String>,
    read: Instant,
    lists: Vec<(riwork_engine::chat::model::Provider, Vec<ModelOption>)>,
    usage: suggest::ModelUsage,
}

#[derive(Default)]
struct Caches {
    files: suggest::FileCache,
    models: Vec<ModelSources>,
}

static CACHES: OnceLock<Mutex<Caches>> = OnceLock::new();

/// Reject a caret in the middle of a surrogate pair, as the engine rejects one
/// inside a UTF-8 character. No lossy conversion can replace adjacent text.
fn byte_at_utf16(text: &str, caret: u32) -> Option<usize> {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units == caret { return Some(byte); }
        units += character.len_utf16() as u32;
        if units > caret { return None; }
    }
    (units == caret).then_some(text.len())
}

fn item(preview: suggest::Preview, detail: String, kind: &str) -> ChatSuggestion {
    ChatSuggestion {
        title: preview.title, detail, insert: preview.inserts,
        body: preview.body, effect: preview.effect, kind: kind.into(),
    }
}

pub fn read(home: PathBuf, info: ChatInfo, own_models: Vec<ModelOption>, text: String, caret: u32) -> Result<Option<ChatSuggestionResult>, String> {
    let Some(caret) = byte_at_utf16(&text, caret) else { return Ok(None); };
    let Some(trigger) = suggest::trigger(&text, caret) else { return Ok(None); };
    let mut truncated = false;
    let items = match trigger.sigil {
        Sigil::Slash => suggest::rank_commands(&trigger.query, &suggest::commands(info.provider))
            .into_iter().map(|command| item(suggest::command_preview(command, info.provider), command.summary.clone(), "command")).collect(),
        Sigil::Mention => {
            let mut caches = CACHES.get_or_init(Default::default).lock().map_err(|_| "broken: suggestion cache unavailable")?;
            let listing = caches.files.fresh(&info.cwd, Instant::now()).unwrap_or_else(|| {
                let listing = Arc::new(suggest::list_files(&info.cwd));
                caches.files.put(info.cwd.clone(), Instant::now(), listing.clone());
                listing
            });
            truncated = listing.truncated;
            suggest::rank_files(&trigger.query, &listing.entries, suggest::SHOWN).into_iter().map(|file| {
                let preview = suggest::file_preview(file, None);
                let detail = preview.kind.clone();
                item(preview, detail, if file.dir { "folder" } else { "file" })
            }).collect()
        }
        Sigil::Model => {
            let mut caches = CACHES.get_or_init(Default::default).lock().map_err(|_| "broken: suggestion cache unavailable")?;
            let now = Instant::now();
            caches.models.retain(|source| now.saturating_duration_since(source.read) < suggest::FILE_TTL);
            if !caches.models.iter().any(|source| source.home == home && source.project == info.project_id) {
                let infos = log::read_infos(&home);
                let lists = catalog::mention_lists(&home, info.project_id.as_deref(), &infos);
                let usage = suggest::ModelUsage::of(&infos);
                caches.models.push(ModelSources { home: home.clone(), project: info.project_id.clone(), read: Instant::now(), lists, usage });
            }
            let source = caches.models.iter().find(|source| source.home == home && source.project == info.project_id)
                .ok_or("broken: model suggestions unavailable")?;
            let entries = suggest::mention_entries(info.provider, &own_models, &source.lists, &source.usage);
            let current = suggest::current_model(&entries, info.provider, info.model.as_deref());
            suggest::rank_models(&trigger.query, &entries).into_iter().take(suggest::SHOWN).map(|model| {
                let current = model.provider == Some(info.provider) && current == Some(model.id.as_str());
                item(suggest::model_preview(model, current, info.provider), suggest::model_detail(model, current), "model")
            }).collect()
        }
    };
    Ok(Some(ChatSuggestionResult {
        start: text[..trigger.start].encode_utf16().count() as u32,
        end: text[..trigger.end].encode_utf16().count() as u32,
        sigil: trigger.sigil.char().to_string(), root: info.cwd.to_string_lossy().into_owned(), items, truncated,
    }))
}
