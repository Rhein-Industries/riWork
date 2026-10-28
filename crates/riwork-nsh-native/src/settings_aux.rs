//! Settings actions kept off the engine thread. Secrets never enter a snapshot.
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, PropError, TsTypes},
    prelude::*,
};
use riwork_core::orca_import::{ImportManager, ImportPreview, ImportReceipt};
use riwork_engine::anthropic_key::{self, KeyStore};
use serde::{Deserialize, Serialize};

use crate::{
    hub::{Hub, Snapshot, TopicSubscription},
    service::{self, blocking},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyDto {
    pub saved: Option<bool>,
    pub error: Option<String>,
}

pub struct ApiKeyService {
    store: Arc<dyn KeyStore>,
    state: Mutex<ApiKeyDto>,
    hub: Arc<Hub<ApiKeyDto>>,
}

impl ApiKeyService {
    pub fn with_store(store: Arc<dyn KeyStore>) -> Self {
        let initial = ApiKeyDto {
            saved: None,
            error: None,
        };
        Self {
            store,
            state: Mutex::new(initial.clone()),
            hub: Arc::new(Hub::new(initial)),
        }
    }
    pub fn refresh(&self) -> ApiKeyDto {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        *state = match self.store.is_saved() {
            Ok(saved) => ApiKeyDto {
                saved: Some(saved),
                error: None,
            },
            Err(error) => ApiKeyDto {
                saved: None,
                error: Some(error),
            },
        };
        self.hub.publish(state.clone());
        state.clone()
    }
    pub fn status(&self) -> ApiKeyDto {
        let state = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if state.saved.is_none() && state.error.is_none() {
            self.refresh()
        } else {
            state
        }
    }
    pub fn save(&self, pasted: &str) -> Result<ApiKeyDto, String> {
        // Validation errors never contain the paste; the engine owns these rules.
        let key = anthropic_key::normalized(pasted)?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.store.save(&key)?;
        *state = ApiKeyDto {
            saved: Some(true),
            error: None,
        };
        self.hub.publish(state.clone());
        Ok(state.clone())
    }
    pub fn remove(&self) -> Result<ApiKeyDto, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.store.remove()?;
        *state = ApiKeyDto {
            saved: Some(false),
            error: None,
        };
        self.hub.publish(state.clone());
        Ok(state.clone())
    }
    pub fn subscribe(&self) -> TopicSubscription<ApiKeyDto> {
        self.hub.subscribe()
    }
}

static API_KEY: OnceLock<Arc<ApiKeyService>> = OnceLock::new();
fn fixture_mode() -> bool {
    if cfg!(any(test, feature = "test-support")) {
        return true;
    }
    // Plain `nsh test` builds dependencies without test-support. Like Usage,
    // debug adapters for a private temporary home must never reach live services.
    #[cfg(debug_assertions)]
    {
        let Ok(home) = service::process_home() else {
            return true;
        };
        let home = home.canonicalize().unwrap_or(home);
        home.starts_with("/tmp") || home.starts_with("/private/tmp")
    }
    #[cfg(not(debug_assertions))]
    false
}
fn api_key() -> Arc<ApiKeyService> {
    API_KEY
        .get_or_init(|| {
            let store: Arc<dyn KeyStore> = if fixture_mode() {
                Arc::new(anthropic_key::MemoryStore::default())
            } else {
                Arc::new(anthropic_key::Keychain)
            };
            Arc::new(ApiKeyService::with_store(store))
        })
        .clone()
}

#[module]
pub struct ApiKey;
#[module]
impl ApiKey {
    pub async fn status() -> Result<ApiKeyDto, String> {
        blocking(|| Ok(api_key().status())).await
    }
    pub async fn refresh() -> Result<ApiKeyDto, String> {
        blocking(|| Ok(api_key().refresh())).await
    }
    pub async fn save(pasted: String) -> Result<ApiKeyDto, String> {
        blocking(move || api_key().save(&pasted)).await
    }
    pub async fn remove() -> Result<ApiKeyDto, String> {
        blocking(|| api_key().remove()).await
    }
    pub fn subscribe(
        listener: Callback<Snapshot<ApiKeyDto>>,
        cx: CallCx<'_>,
    ) -> crate::Subscription {
        crate::Subscription::start(cx, async move {
            if let Ok(subscription) = blocking(|| Ok(api_key().subscribe())).await {
                crate::deliver(subscription, listener).await;
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ImportReceiptDto {
    pub source: String,
    pub project_count: usize,
    pub folder_count: usize,
    pub worktree_count: usize,
    pub completed_at: u64,
}
impl From<ImportReceipt> for ImportReceiptDto {
    fn from(receipt: ImportReceipt) -> Self {
        Self {
            source: receipt.source.to_string_lossy().into_owned(),
            project_count: receipt.project_count,
            folder_count: receipt.folder_count,
            worktree_count: receipt.worktree_count,
            completed_at: receipt.completed_at,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewDto {
    pub id: String,
    pub source: String,
    pub project_count: usize,
    pub folder_count: usize,
    pub worktree_count: usize,
    pub already_imported: Option<ImportReceiptDto>,
    pub recordable: bool,
    pub warnings: Vec<String>,
}
struct PendingImport {
    id: String,
    manager: ImportManager,
    preview: ImportPreview,
}
static IMPORTS: OnceLock<Mutex<Vec<PendingImport>>> = OnceLock::new();
fn imports() -> &'static Mutex<Vec<PendingImport>> {
    IMPORTS.get_or_init(|| Mutex::new(Vec::new()))
}

#[module]
pub struct OrcaImport;
#[module]
impl OrcaImport {
    pub async fn preview() -> Result<ImportPreviewDto, String> {
        blocking(|| {
            if fixture_mode() {
                return Ok(ImportPreviewDto {
                    id: "isolated-no-import".into(),
                    source: "Fixture (Orca is not consulted)".into(),
                    project_count: 0,
                    folder_count: 0,
                    worktree_count: 0,
                    already_imported: None,
                    recordable: false,
                    warnings: vec!["Orca import is not run in headless fixtures.".into()],
                });
            }
            {
                let process = service::initialized()?;
                // Core's public constructor resolves the environment. Refuse a
                // different home rather than importing into another registry.
                let home = riwork_core::paths::riwork_home()?
                    .canonicalize()
                    .map_err(service::io_error)?;
                if home != process.home() {
                    return Err(
                        "conflict: Orca import home differs from the fixed process home".into(),
                    );
                }
                let manager = ImportManager::open_default()?;
                let preview = manager.inspect()?;
                let dto = ImportPreviewDto {
                    id: uuid::Uuid::new_v4().to_string(),
                    source: preview.source.to_string_lossy().into_owned(),
                    project_count: preview.project_count,
                    folder_count: preview.folder_count,
                    worktree_count: preview.worktree_count,
                    already_imported: preview.already_imported.clone().map(Into::into),
                    recordable: preview.recordable(),
                    warnings: preview.warnings.clone(),
                };
                let mut pending = imports().lock().unwrap_or_else(PoisonError::into_inner);
                // Bound abandoned previews without trusting a client-supplied plan.
                if pending.len() >= 32 {
                    pending.remove(0);
                }
                pending.push(PendingImport {
                    id: dto.id.clone(),
                    manager,
                    preview,
                });
                Ok(dto)
            }
        })
        .await
    }
    pub async fn run(id: String) -> Result<ImportReceiptDto, String> {
        blocking(move || {
            if fixture_mode() {
                return Err("refused: Orca import is not run in headless fixtures.".into());
            }
            let mut pending = imports().lock().unwrap_or_else(PoisonError::into_inner);
            let index = pending
                .iter()
                .position(|entry| entry.id == id)
                .ok_or("conflict: Refresh the Orca preview before importing.")?;
            let entry = &pending[index];
            let receipt = entry.manager.import(&entry.preview)?;
            pending.remove(index);
            Ok(receipt.into())
        })
        .await
    }
}

crate::settings::snapshot_js_value!(ApiKeyDto, "ApiKeyEvent");

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn key_validation_and_failures_leave_memory_store_and_status_unchanged() {
        let store = Arc::new(anthropic_key::MemoryStore::default());
        let service = ApiKeyService::with_store(store.clone());
        assert_eq!(service.status().saved, Some(false));
        let subscription = service.subscribe();
        assert_eq!(subscription.recv().await.unwrap().data.saved, Some(false));
        for paste in ["", "  ", "fake secret", "not\nkey", "ž"] {
            assert!(service.save(paste).is_err());
            assert_eq!(store.read().unwrap(), None);
            assert_eq!(service.status().saved, Some(false));
        }
        assert_eq!(
            service.save("  isolated-fixture-value  ").unwrap().saved,
            Some(true)
        );
        assert_eq!(
            store.read().unwrap().as_deref(),
            Some("isolated-fixture-value")
        );
        let event = subscription.recv().await.unwrap();
        assert!(
            !serde_json::to_string(&event)
                .unwrap()
                .contains("isolated-fixture-value")
        );
        store.fail(Some("fixture denied"));
        assert_eq!(service.save("replacement").unwrap_err(), "fixture denied");
        assert_eq!(service.remove().unwrap_err(), "fixture denied");
        assert_eq!(service.status().saved, Some(true));
        assert_eq!(service.refresh().error.as_deref(), Some("fixture denied"));
        store.fail(None);
        assert_eq!(service.refresh().saved, Some(true));
        assert_eq!(service.remove().unwrap().saved, Some(false));
    }
}
