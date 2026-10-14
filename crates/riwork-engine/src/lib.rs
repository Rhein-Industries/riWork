//! Headless session, agent, chat, scheduling, and remote backends for RiWork.

// Keep the shared data namespaces identical for backend callers.
pub use riwork_core::{
    appearance_file, codex_accounts, delegation, global_window, layouts, orca_import, paths,
    project_recency, provider, recency_file, settings, status_bar, store, theme, ui_text,
};

pub mod activity;
pub mod agent_hooks;
pub mod anthropic_key;
pub mod chat;
pub mod cli_agents;
pub mod cua;
pub mod file_preview;
mod folder_trust;
pub mod global_overview;
pub mod handoff;
pub mod notifications;
pub mod orchestrator_wakes;
pub mod orchestrators;
pub mod project_sort;
pub mod project_tabs;
pub mod remote_cache;
pub mod remote_cli;
pub mod remote_hosts;
pub mod remote_tree;
pub mod runtime;
pub mod schedule_chat;
pub mod schedule_service;
pub mod schedules;
pub mod session_catalog;
pub mod session_input;
pub mod session_keys;
pub mod session_reload;
pub mod session_viewport;
pub mod sessions;
pub mod sgr;
pub mod terminal_drop;
pub mod terminal_links;
pub mod upload_inbox;
pub mod usage;
