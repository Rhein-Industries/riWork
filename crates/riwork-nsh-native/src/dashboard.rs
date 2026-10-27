//! P1 global-window preferences only; dashboard activity remains deferred.
use crate::service;
use nsh_core::prelude::*;
use riwork_core::{
    global_window::{GlobalWindowState, GlobalWindowStore},
    layouts::WindowSize,
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct WindowSizeDto {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct GlobalWindowDto {
    pub schema: u32,
    pub view: String,
    pub show_quiet: bool,
    pub dashboard_filter: Option<String>,
    pub dashboard_sort: String,
    pub window_size: Option<WindowSizeDto>,
}
impl From<GlobalWindowState> for GlobalWindowDto {
    fn from(state: GlobalWindowState) -> Self {
        Self {
            schema: 2,
            view: state.view.name().into(),
            show_quiet: state.show_quiet,
            dashboard_filter: state.dashboard_filter.map(|filter| filter.name().into()),
            dashboard_sort: state.dashboard_sort.name().into(),
            window_size: state.window_size.map(|size| WindowSizeDto {
                width: size.width,
                height: size.height,
            }),
        }
    }
}
impl GlobalWindowDto {
    fn validate(self) -> Result<GlobalWindowState, String> {
        use riwork_core::global_window::{DashboardFilter, DashboardSort, GlobalView};
        if self.schema != 2 || !matches!(self.view.as_str(), "dashboard" | "chat") {
            return Err("invalid: global window schema or view".into());
        }
        let filter = self
            .dashboard_filter
            .map(|name| {
                DashboardFilter::ALL
                    .into_iter()
                    .find(|filter| filter.name() == name)
                    .ok_or_else(|| "invalid: dashboard filter".to_owned())
            })
            .transpose()?;
        let sort = DashboardSort::ALL
            .into_iter()
            .find(|sort| sort.name() == self.dashboard_sort)
            .ok_or("invalid: dashboard sort")?;
        let size = self
            .window_size
            .map(|size| {
                WindowSize::new(size.width, size.height)
                    .ok_or_else(|| "invalid: global window size".to_owned())
            })
            .transpose()?;
        Ok(GlobalWindowState {
            view: GlobalView::from(self.view),
            show_quiet: self.show_quiet,
            dashboard_filter: filter,
            dashboard_sort: sort,
            window_size: size,
        })
    }
}

pub trait DashboardBackend: Send + Sync {
    fn load(&self) -> Result<GlobalWindowState, String>;
    fn save(&self, state: &GlobalWindowState) -> Result<(), String>;
}
pub struct CoreDashboardBackend(GlobalWindowStore);
impl CoreDashboardBackend {
    pub fn new(home: PathBuf) -> Self {
        Self(GlobalWindowStore::at(home))
    }
}
impl DashboardBackend for CoreDashboardBackend {
    fn load(&self) -> Result<GlobalWindowState, String> {
        Ok(self.0.load())
    }
    fn save(&self, state: &GlobalWindowState) -> Result<(), String> {
        self.0.save(state).map_err(service::io_error)
    }
}
static BACKEND: OnceLock<Arc<dyn DashboardBackend>> = OnceLock::new();
pub fn install_backend(backend: Arc<dyn DashboardBackend>) -> Result<(), String> {
    BACKEND
        .set(backend)
        .map_err(|_| "conflict: dashboard backend is already initialized".into())
}
fn backend() -> Result<&'static Arc<dyn DashboardBackend>, String> {
    if let Some(backend) = BACKEND.get() {
        return Ok(backend);
    }
    let production = Arc::new(CoreDashboardBackend::new(service::process_home()?));
    Ok(BACKEND.get_or_init(|| production))
}

#[module]
pub struct Dashboard;
#[module]
impl Dashboard {
    pub async fn get_global_window() -> Result<GlobalWindowDto, String> {
        service::blocking(|| backend()?.load().map(Into::into)).await
    }
    pub async fn save_global_window(state: GlobalWindowDto) -> Result<GlobalWindowDto, String> {
        service::blocking(move || {
            let state = state.validate()?;
            backend()?.save(&state)?;
            Ok(state.into())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    #[derive(Default)]
    struct FakeDashboard(Mutex<GlobalWindowState>);
    impl DashboardBackend for FakeDashboard {
        fn load(&self) -> Result<GlobalWindowState, String> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(&self, state: &GlobalWindowState) -> Result<(), String> {
            *self.0.lock().unwrap() = state.clone();
            Ok(())
        }
    }
    #[tokio::test]
    async fn global_window_module_uses_an_injected_backend_and_validates_before_writing() {
        let fake = Arc::new(FakeDashboard::default());
        install_backend(fake.clone()).unwrap();
        let mut state = Dashboard::get_global_window().await.unwrap();
        state.view = "chat".into();
        let saved = Dashboard::save_global_window(state.clone()).await.unwrap();
        assert_eq!(saved, state);
        state.view = "unrecognized".into();
        assert!(
            Dashboard::save_global_window(state)
                .await
                .unwrap_err()
                .starts_with("invalid:")
        );
        assert_eq!(fake.load().unwrap().view.name(), "chat");
    }
}
