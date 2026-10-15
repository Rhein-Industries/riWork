//! RiWork's command dispatcher, workspace MCP server, and local-source updater.
//!
//! The desktop `riwork` executable calls [`cli::run_cli`] before it initializes
//! its UI. The `riwork-cli` executable uses [`cli::run_headless`] and delegates
//! graphical invocations to the installed desktop executable.

pub mod cli;
pub mod mcp;
pub mod update;

// Preserve the existing module paths while all workspace state and process
// ownership live in the headless core and engine libraries.
pub use riwork_core::{
    appearance_file, codex_accounts, layouts, orca_import, paths, project_memo, project_recency,
    provider, recency_file, settings, status_bar, store, theme, ui_text,
};
pub use riwork_engine::{
    activity, agent_hooks, chat, cli_agents, cua, file_preview, global_overview, handoff,
    notifications, orchestrators, project_sort, project_tabs, remote_cache, remote_cli,
    remote_hosts, remote_tree, runtime, schedule_chat, schedule_service, schedules,
    session_catalog, session_input, session_keys, session_reload, session_viewport, sessions, sgr,
    terminal_drop, terminal_links, upload_inbox, usage,
};

pub use cli::{run_cli, run_headless};
