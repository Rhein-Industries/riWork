//! Shared project membership, refreshed only after home-watcher events or writes.
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

use anyhow::Result;
use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, JsValue, PropError, TsTypes},
    prelude::*,
};
use riwork_engine::project_tabs as engine;
use serde::{Deserialize, Serialize};

use crate::{
    Subscription, deliver,
    hub::{Hub, Snapshot, TopicSubscription},
    service,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct TabsSnapshot {
    pub epoch: String,
    pub revision: u64,
    pub entries: Vec<TabEntryDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct TabEntryDto {
    pub key: String,
    pub kind: String,
    pub title: String,
    pub base_title: String,
    pub title_priority: u16,
    pub status: String,
    pub hidden: bool,
    pub worker: bool,
    pub order: usize,
    pub parent: Option<String>,
    pub children: Vec<TabEntryDto>,
    pub child_count: usize,
    pub rename: Option<String>,
    pub created: u64,
}

impl From<engine::Entry> for TabEntryDto {
    fn from(value: engine::Entry) -> Self {
        Self {
            key: value.key,
            kind: match value.kind {
                engine::Kind::Chat => "chat",
                engine::Kind::Shell => "shell",
            }
            .into(),
            title: value.title,
            base_title: value.base_title,
            title_priority: value.title_priority.into(),
            status: match value.status {
                engine::Status::Working => "working",
                engine::Status::Waiting => "waiting",
                engine::Status::Error => "error",
                engine::Status::Done => "done",
                engine::Status::Idle => "idle",
                engine::Status::Stopped => "stopped",
            }
            .into(),
            hidden: value.hidden,
            worker: value.worker,
            order: value.order,
            parent: value.parent,
            children: value.children.into_iter().map(Into::into).collect(),
            child_count: value.child_count,
            rename: value.rename,
            created: value.created,
        }
    }
}

impl From<engine::Snapshot> for TabsSnapshot {
    fn from(value: engine::Snapshot) -> Self {
        Self {
            epoch: value.epoch,
            revision: value.revision,
            entries: value.entries.into_iter().map(Into::into).collect(),
        }
    }
}

impl JsValue for Snapshot<TabsSnapshot> {
    fn ts_type(types: &mut TsTypes) -> String {
        types.declare("TabsEvent", |types| format!("export interface TabsEvent {{ kind: 'snapshot'; rev: number; data: {}; resync?: boolean; }}", TabsSnapshot::ts_type(types)))
    }
    fn from_js(value: JsIn<'_>) -> std::result::Result<Self, PropError> {
        serde_json::from_value(JsonValue::from_js(value)?).map_err(|e| PropError(e.to_string()))
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> std::result::Result<JsOwned<'a>, PropError> {
        serde_json::to_value(self)
            .map_err(|e| PropError(e.to_string()))?
            .to_js(host)
    }
}

/// Blocking backend; the service invokes it exclusively on background threads.
pub trait TabsBackend: Send + Sync + 'static {
    fn snapshot(&self, project_id: &str) -> Result<TabsSnapshot>;
    fn update(&self, project_id: &str, update: &engine::Update) -> Result<TabsSnapshot>;
}

pub struct EngineTabsBackend {
    home: PathBuf,
}
impl EngineTabsBackend {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }
}
impl TabsBackend for EngineTabsBackend {
    fn snapshot(&self, project_id: &str) -> Result<TabsSnapshot> {
        engine::refresh_snapshot_with_activity(&self.home, project_id, &Default::default())
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }
    fn update(&self, project_id: &str, update: &engine::Update) -> Result<TabsSnapshot> {
        let store = engine::TabStore::at(&self.home, project_id).map_err(anyhow::Error::msg)?;
        store.update(update).map_err(anyhow::Error::msg)?;
        store.snapshot().map(Into::into).map_err(anyhow::Error::msg)
    }
}

pub struct TabsService {
    backend: Arc<dyn TabsBackend>,
    hubs: Mutex<HashMap<String, Arc<Hub<TabsSnapshot>>>>,
}
impl TabsService {
    pub fn new(backend: Arc<dyn TabsBackend>) -> Arc<Self> {
        Arc::new(Self {
            backend,
            hubs: Mutex::new(HashMap::new()),
        })
    }
    pub fn subscribe(&self, project_id: &str) -> Result<TopicSubscription<TabsSnapshot>, String> {
        service::validate_uuid(project_id)?;
        // Only called from the blocking pool. Serializing initial reads with
        // watcher refreshes closes the read/register gap for a new topic.
        let mut hubs = self.hubs.lock().unwrap_or_else(PoisonError::into_inner);
        let hub = match hubs.get(project_id) {
            Some(hub) => hub.clone(),
            None => {
                let hub = Arc::new(Hub::new(
                    self.backend
                        .snapshot(project_id)
                        .map_err(service::backend_error)?,
                ));
                hubs.insert(project_id.into(), hub.clone());
                hub
            }
        };
        Ok(hub.subscribe())
    }
    pub fn update(&self, project_id: &str, update: JsonValue) -> Result<TabsSnapshot, String> {
        service::validate_uuid(project_id)?;
        let update: engine::Update =
            serde_json::from_value(update).map_err(|e| format!("invalid: {e}"))?;
        // Delegate to the exact CLI validators, including move targets and title.
        update.validate().map_err(|e| format!("invalid: {e}"))?;
        let hubs = self.hubs.lock().unwrap_or_else(PoisonError::into_inner);
        let snapshot = self
            .backend
            .update(project_id, &update)
            .map_err(service::backend_error)?;
        if let Some(hub) = hubs.get(project_id) {
            hub.publish(snapshot.clone());
        }
        Ok(snapshot)
    }
    pub fn refresh(&self) -> Result<(), String> {
        let hubs = self.hubs.lock().unwrap_or_else(PoisonError::into_inner);
        for (project_id, hub) in hubs.iter() {
            hub.publish(
                self.backend
                    .snapshot(project_id)
                    .map_err(service::backend_error)?,
            );
        }
        Ok(())
    }
}

#[module]
pub struct Tabs;
#[module]
impl Tabs {
    pub fn subscribe(
        project_id: String,
        listener: Callback<Snapshot<TabsSnapshot>>,
        cx: CallCx<'_>,
    ) -> Result<Subscription, String> {
        service::validate_uuid(&project_id)?;
        Ok(Subscription::start(cx, async move {
            if let Ok(service) = service::initialize().await {
                let tabs = service.tabs.clone();
                if let Ok(subscription) =
                    service::blocking(move || tabs.subscribe(&project_id)).await
                {
                    deliver(subscription, listener).await;
                }
            }
        }))
    }
    pub async fn update(project_id: String, update: JsonValue) -> Result<TabsSnapshot, String> {
        let tabs = service::initialize().await?.tabs.clone();
        service::blocking(move || tabs.update(&project_id, update)).await
    }
}
