//! Compatibility exports for the shared P1/P3a tab adapters.
//! The process service owns one project hub map and the sole home watcher.
pub use crate::Tabs;
pub use crate::backend::{EngineTabsBackend, TabsBackend};
pub use crate::dto::{TabEntryDto, TabUpdate, TabsSnapshot};
