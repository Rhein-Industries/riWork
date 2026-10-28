//! Remote project dispatch only; transport and pairing stay in the engine/native service.
use crate::session_io::Data;
use riwork_core::{layouts::SavedTab, settings::SettingsStore};
use std::path::Path;

pub fn is_project(project: &str) -> bool {
    riwork_engine::remote_tree::parse_project_key(project).is_some()
}

pub(crate) fn validate_project(project: &str) -> Result<(), String> {
    if is_project(project) {
        Ok(())
    } else {
        crate::validate_id(project)
    }
}

pub(crate) fn validate_selection(project: &str, id: &str) -> Result<(), String> {
    if is_project(project) {
        identifier(id)
    } else {
        crate::validate_id(id)
    }
}

pub(crate) fn require_local(project: &str) -> Result<(), String> {
    if is_project(project) {
        Err("refused: this action requires a project on this Mac".into())
    } else if project.starts_with("remote:") {
        Err("invalid: remote project key".into())
    } else {
        Ok(())
    }
}

/// Read only this Mac's display preferences, never its project/session inventories.
pub(crate) fn load(home: &Path, project: &str) -> Result<Option<Data>, String> {
    if !is_project(project) {
        require_local(project)?;
        return Ok(None);
    }
    let settings = SettingsStore::open(home)?.load()?;
    Ok(Some(Data {
        project_id: project.into(),
        root: None,
        shells: None,
        tabs: None,
        close_behavior: settings.tab_close_behavior,
        theme: settings.theme,
        ui_text_size: settings.ui_text_size.points(),
        panel_tab_icons: settings.panel_tab_icons,
        notice: None,
        global_chats: Default::default(),
        owners: Default::default(),
    }))
}

pub(crate) fn shell(desktop_id: String, shell_id: String) -> Result<SavedTab, String> {
    for id in [&desktop_id, &shell_id] {
        identifier(id)?;
    }
    Ok(SavedTab::RemoteShell {
        desktop_id,
        shell_id,
    })
}

fn identifier(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 256
        || id
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == ':')
    {
        Err("invalid: remote identifier".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
