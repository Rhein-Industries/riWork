//! Default location for newly created projects.

use std::{
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

/// The shared RiWork data directory. An empty `RIWORK_HOME` or `HOME` counts as
/// unset: `create_dir_all("")` succeeds, which would put state in the working
/// directory and fail later at the first fsync.
pub fn riwork_home() -> Result<PathBuf, String> {
    resolve_riwork_home(env::var_os("RIWORK_HOME"), env::var_os("HOME"))
}

fn resolve_riwork_home(
    riwork_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, String> {
    if let Some(dir) = riwork_home.filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = home
        .filter(|home| !home.is_empty())
        .ok_or("HOME is unset; set RIWORK_HOME")?;
    Ok(PathBuf::from(home).join(".local/share/riwork"))
}

/// Create the data directory owner-only when it is new. An existing directory
/// keeps its mode, and parents above it are created normally.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the directory path is empty",
        ));
    }
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        result => result,
    }
}

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
    fn empty_home_variables_count_as_unset() {
        let os = |value: &str| Some(OsString::from(value));
        assert_eq!(
            resolve_riwork_home(os("/data"), os("/home/me")).unwrap(),
            PathBuf::from("/data")
        );
        assert_eq!(
            resolve_riwork_home(None, os("/home/me")).unwrap(),
            PathBuf::from("/home/me/.local/share/riwork")
        );
        assert_eq!(
            resolve_riwork_home(os(""), os("/home/me")).unwrap(),
            PathBuf::from("/home/me/.local/share/riwork")
        );
        assert!(resolve_riwork_home(os(""), os("")).is_err());
        assert!(resolve_riwork_home(None, None).is_err());
    }

    #[test]
    fn private_directories_are_created_once_and_never_tightened_afterwards() {
        let root = env::temp_dir().join(format!("riwork-paths-{}", uuid::Uuid::new_v4()));
        let nested = root.join("a/b");
        create_private_dir(&nested).unwrap();
        create_private_dir(&nested).unwrap();
        assert!(nested.is_dir());
        assert!(create_private_dir(Path::new("")).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            let control = root.join("control");
            fs::create_dir_all(&control).unwrap();
            assert_eq!(mode(&nested), 0o700);
            assert_eq!(
                mode(&root.join("a")),
                mode(&control),
                "parents are created normally"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

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
