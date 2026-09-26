//! Default location for newly created projects.

use std::{env, fs, path::PathBuf};

pub fn default_projects_directory() -> Result<PathBuf, String> {
    let home = env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or("Cannot locate the home directory: HOME is not set")?;
    Ok(PathBuf::from(home).join("Documents/riwork"))
}

pub fn ensure_default_projects_directory() -> Result<PathBuf, String> {
    let directory = default_projects_directory()?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Cannot create {}: {error}", directory.display()))?;
    Ok(directory)
}

pub fn default_new_project_path(name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\', '\0']) {
        return Err(
            "A default project name must be one folder name; use PATH for another location"
                .to_owned(),
        );
    }
    Ok(default_projects_directory()?.join(name))
}
