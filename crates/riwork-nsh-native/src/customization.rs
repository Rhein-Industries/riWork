//! Process-wide UI customization topic. File reads and mutation run away from
//! engine/AppKit threads; every engine receives the same last-good snapshot.
use crate::{
    Subscription,
    hub::{Hub, TopicSubscription},
    service,
};
use notify::{EventKind, RecursiveMode, Watcher};
use nsh_core::prelude::*;
use riwork_core::customization::{CustomizationSnapshot, CustomizationStore};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock, mpsc},
    time::{Duration, Instant},
};

pub struct CustomizationService {
    store: Mutex<CustomizationStore>,
    latest: RwLock<CustomizationSnapshot>,
    topic: Hub<CustomizationSnapshot>,
}
impl CustomizationService {
    fn new(home: &Path) -> Result<Self, String> {
        let mut store = CustomizationStore::open(home)?;
        let snapshot = store.snapshot();
        Ok(Self {
            store: Mutex::new(store),
            latest: RwLock::new(snapshot.clone()),
            topic: Hub::new(snapshot),
        })
    }
    /// Cached, with no filesystem work. Safe for a host after observe() setup.
    pub fn snapshot(&self) -> CustomizationSnapshot {
        self.latest.read().unwrap().clone()
    }
    pub fn subscribe(&self) -> TopicSubscription<CustomizationSnapshot> {
        self.topic.subscribe()
    }
    fn publish(&self, snapshot: CustomizationSnapshot) -> CustomizationSnapshot {
        *self.latest.write().unwrap() = snapshot.clone();
        self.topic.publish(snapshot.clone());
        snapshot
    }
    /// Call refresh/mutations only from a worker, never the native main thread.
    pub fn refresh(&self) -> CustomizationSnapshot {
        let mut store = self.store.lock().unwrap();
        self.publish(store.snapshot())
    }
    pub fn select(&self, name: &str) -> Result<CustomizationSnapshot, String> {
        let mut store = self.store.lock().unwrap();
        Ok(self.publish(store.select(name)?))
    }
    pub fn reset(&self) -> Result<CustomizationSnapshot, String> {
        let mut store = self.store.lock().unwrap();
        Ok(self.publish(store.reset()?))
    }
    pub fn initialize_files(&self) -> Result<CustomizationSnapshot, String> {
        let mut store = self.store.lock().unwrap();
        Ok(self.publish(store.initialize_files()?))
    }
}
static SERVICES: OnceLock<Mutex<BTreeMap<PathBuf, Arc<CustomizationService>>>> = OnceLock::new();
/// Initialize on a worker. All native windows and the host keymap share this
/// service for the same home, including its recovery cache and deduplicated topic.
pub fn observe(home: &Path) -> Result<Arc<CustomizationService>, String> {
    let mut services = SERVICES.get_or_init(Default::default).lock().unwrap();
    if let Some(service) = services.get(home) {
        return Ok(service.clone());
    }
    let service = Arc::new(CustomizationService::new(home)?);
    let weak = Arc::downgrade(&service);
    let home = home.to_owned();
    let key = home.clone();
    std::thread::Builder::new()
        .name("somtus-ui-customization".into())
        .spawn(move || {
            let (tx, rx) = mpsc::sync_channel(1);
            let ui = home.join("ui");
            let events = tx.clone();
            let mut watcher =
                notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                    if event.is_ok_and(|event| {
                        !matches!(event.kind, EventKind::Access(_))
                            && event.paths.iter().any(|path| path.starts_with(&ui))
                    }) {
                        let _ = events.try_send(());
                    }
                })
                .ok();
            // Watching the home also sees ui/ being created, replaced or removed.
            if let Some(watcher) = watcher.as_mut() {
                let _ = watcher.watch(&home, RecursiveMode::Recursive);
            }
            loop {
                if rx.recv_timeout(Duration::from_secs(2)).is_ok() {
                    let deadline = Instant::now() + Duration::from_millis(500);
                    while Instant::now() < deadline
                        && rx.recv_timeout(Duration::from_millis(90)).is_ok()
                    {}
                }
                let Some(service) = weak.upgrade() else {
                    break;
                };
                service.refresh();
            }
        })
        .map_err(|error| error.to_string())?;
    services.insert(key, service.clone());
    Ok(service)
}
fn current() -> Result<Arc<CustomizationService>, String> {
    observe(&service::process_home()?)
}
fn value(snapshot: CustomizationSnapshot) -> Result<Value, String> {
    serde_json::to_value(snapshot).map_err(|error| error.to_string())
}

#[module]
pub struct Customization;
#[module]
impl Customization {
    pub async fn snapshot() -> Result<Value, String> {
        service::blocking(|| value(current()?.refresh())).await
    }
    pub fn subscribe(listener: Callback<Value>, cx: CallCx<'_>) -> Subscription {
        Subscription::start(cx, async move {
            if let Ok(subscription) = service::blocking(|| Ok(current()?.subscribe())).await {
                while let Ok(snapshot) = subscription.recv_shared().await {
                    listener.post(
                        serde_json::to_value(subscription.latest_shared(snapshot))
                            .expect("customization event serializes"),
                    );
                    next_frame().await;
                }
            }
        })
    }
    pub async fn select(name: String) -> Result<Value, String> {
        service::blocking(move || value(current()?.select(&name)?)).await
    }
    pub async fn reset() -> Result<Value, String> {
        service::blocking(|| value(current()?.reset()?)).await
    }
    pub async fn initialize_files() -> Result<Value, String> {
        service::blocking(|| value(current()?.initialize_files()?)).await
    }
    pub async fn reveal() -> Result<String, String> {
        service::blocking(|| {
            let home = service::process_home()?;
            let directory = home.join("ui");
            std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            if !crate::cua::fixture_home(&home) {
                #[cfg(target_os = "macos")]
                let executable = "/usr/bin/open";
                #[cfg(target_os = "windows")]
                let executable = "explorer.exe";
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                let executable = "xdg-open";
                let status = std::process::Command::new(executable)
                    .arg(&directory)
                    .status()
                    .map_err(|error| error.to_string())?;
                if !status.success() {
                    return Err("Could not open the UI configuration folder".into());
                }
            }
            Ok(directory.to_string_lossy().into_owned())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn customization_watcher_detects_ui_directory_created_after_startup() {
        let temp = tempfile::tempdir().unwrap();
        let service = observe(temp.path()).unwrap();
        let subscription = service.subscribe();
        assert_eq!(subscription.recv().await.unwrap().data.selected, "default");
        assert!(!temp.path().join("ui").exists());
        let mut editor = CustomizationStore::open(temp.path()).unwrap();
        editor.initialize_files().unwrap();
        editor.select("sidebar").unwrap();
        let changed = tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let event = subscription.recv().await.unwrap();
                if event.data.selected == "sidebar" {
                    break event.data;
                }
            }
        })
        .await
        .unwrap();
        assert!(changed.error.is_none());
        assert_eq!(service.snapshot().selected, "sidebar");
        // Let the fixture's watcher exit once this test releases its owner.
        SERVICES.get().unwrap().lock().unwrap().remove(temp.path());
    }
    #[tokio::test]
    async fn customization_topic_deduplicates_and_recovers_invalid_editor_writes() {
        let temp = tempfile::tempdir().unwrap();
        let service = CustomizationService::new(temp.path()).unwrap();
        let subscription = service.subscribe();
        let initial = subscription.recv().await.unwrap();
        service.refresh();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), subscription.recv())
                .await
                .is_err()
        );
        service.initialize_files().unwrap();
        service.select("sidebar").unwrap();
        let selected = subscription.latest(subscription.recv().await.unwrap());
        assert_eq!(selected.data.selected, "sidebar");
        assert!(selected.rev > initial.rev);
        std::fs::write(temp.path().join("ui/layouts/sidebar.json"), "{ incomplete").unwrap();
        service.refresh();
        let invalid = subscription.recv().await.unwrap();
        assert_eq!(invalid.data.layout, selected.data.layout);
        assert!(invalid.data.error.is_some());
        service.refresh();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), subscription.recv())
                .await
                .is_err()
        );
        std::fs::write(
            temp.path().join("ui/layouts/sidebar.json"),
            serde_json::to_vec(&riwork_core::customization::sidebar_layout()).unwrap(),
        )
        .unwrap();
        service.refresh();
        let recovered = subscription.recv().await.unwrap();
        assert!(recovered.data.error.is_none());
        assert_eq!(recovered.data.selected, "sidebar");
    }
}
