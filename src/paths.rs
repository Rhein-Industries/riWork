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
    // Hidden names are reserved for tools: `.git` here would make every later
    // project in the default directory look like it lives inside a repository.
    if name.starts_with('.') {
        return Err(
            "A default project name cannot start with a dot; use PATH for another location"
                .to_owned(),
        );
    }
    Ok(default_projects_directory()?.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_names_are_one_visible_folder_name() {
        for name in [
            "", "  ", ".", "..", ".git", ".GIT", ".Git", " .git ", ".hidden", "a/b", "a\\b", "a\0b",
        ] {
            assert!(default_new_project_path(name).is_err(), "{name:?}");
        }
        for name in ["project", "my.project", "git", "gitignore.d", " padded "] {
            let path = default_new_project_path(name).unwrap();
            assert_eq!(path.file_name().unwrap(), name.trim());
        }
    }
}
