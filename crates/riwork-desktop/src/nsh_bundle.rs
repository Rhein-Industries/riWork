//! Packaged apps load their own NSH resources; unbundled hosts can use an adjacent
//! app, staged UI output or their development checkout. Overrides are authoritative.
use nsh_platform_macos::Bundle;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn load() -> Result<(Bundle, Option<PathBuf>), String> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/riwork");
    let override_path = std::env::var_os("RIWORK_NSH_BUNDLE").map(PathBuf::from);
    let path = if let Some(path) = override_path.as_deref() {
        // Do not inspect other candidates, even if this override is unusable.
        resolve(&source, Some(path))?
    } else {
        let executable = std::env::current_exe()
            .map_err(|error| format!("NSH bundle: could not locate Somtus executable: {error}"))?;
        resolve_for_executable(&source, &executable)?
    };
    let bundle =
        Bundle::read(&path).map_err(|error| format!("NSH bundle {}: {error}", path.display()))?;
    Ok((bundle, path.parent().map(Path::to_path_buf)))
}

fn resolve_for_executable(source: &Path, executable: &Path) -> Result<PathBuf, String> {
    let beside = executable.parent()
        .ok_or_else(|| "NSH bundle: Somtus executable has no parent directory".to_owned())?;

    // A macOS Contents/MacOS layout owns both JS and its assets, including a
    // retained Somtus.app.previous. Missing resources never reach into a checkout.
    if beside.file_name().is_some_and(|name| name == "MacOS")
        && let Some(contents) = beside.parent()
        && contents.file_name().is_some_and(|name| name == "Contents")
    {
        return packaged(&contents.join("Resources/riwork-ui/bundle.js"));
    }

    // Prefer the new bundle, while allowing an existing installation's resources
    // during migration. An incomplete new bundle must never use stale resources.
    for name in ["Somtus.app", "RiWork.app"] {
        let sibling_app = beside.join(name);
        let sibling = sibling_app.join("Contents/Resources/riwork-ui/bundle.js");
        if metadata(&sibling_app)?.is_some() {
            return packaged(&sibling);
        }
    }
    let adjacent = beside.join("riwork-ui/bundle.js");
    if metadata(&adjacent)?.is_some() { return packaged(&adjacent); }
    // Normal unbundled development, including a custom Cargo target directory.
    // To use a manual rebuild beside an existing app, select it explicitly with
    // RIWORK_NSH_BUNDLE rather than guessing which output is newer.
    resolve(source, None)
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("NSH bundle {}: {error}", path.display())),
    }
}

fn packaged(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize().map_err(|error| format!(
        "NSH bundle {}: {error}. Rebuild or reinstall Somtus with its riwork-ui resources; or set RIWORK_NSH_BUNDLE to an absolute built bundle.js path",
        path.display()
    ))
}

fn resolve(source: &Path, override_path: Option<&Path>) -> Result<PathBuf, String> {
    let default_path = source.join(".nsh/out/bundle.js");
    let path = override_path.unwrap_or(&default_path);
    if override_path.is_some() && !path.is_absolute() {
        return Err("NSH bundle: RIWORK_NSH_BUNDLE must be an absolute path".into());
    }
    path.canonicalize().map_err(|error| {
        format!(
            "NSH bundle {}: {error}. Build apps/riwork/App.tsx with NSH 0.3: nsh build apps/riwork --out apps/riwork/.nsh/out; or set RIWORK_NSH_BUNDLE to its built bundle.js",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_bundle_wins_and_an_incomplete_install_does_not_use_legacy_resources() {
        let root = std::env::temp_dir().join(format!("somtus-bundle-{}", uuid::Uuid::new_v4()));
        let legacy = root.join("RiWork.app/Contents/Resources/riwork-ui/bundle.js");
        let current = root.join("Somtus.app/Contents/Resources/riwork-ui/bundle.js");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, "legacy").unwrap();
        let executable = root.join("somtus");
        let missing_source = root.join("missing-source");
        assert_eq!(
            resolve_for_executable(&missing_source, &executable).unwrap(),
            legacy.canonicalize().unwrap()
        );
        fs::create_dir_all(current.parent().unwrap()).unwrap();
        assert!(resolve_for_executable(&missing_source, &executable).is_err());
        fs::write(&current, "current").unwrap();
        assert_eq!(
            resolve_for_executable(&missing_source, &executable).unwrap(),
            current.canonicalize().unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_real_bundle_reports_the_build_recipe_without_a_bootstrap_fallback() {
        let missing =
            std::env::temp_dir().join(format!("riwork-no-bundle-{}", uuid::Uuid::new_v4()));
        let error = resolve(&missing, None).unwrap_err();
        assert!(error.contains(".nsh/out/bundle.js"));
        assert!(error.contains("nsh build apps/riwork"));
    }

    #[test]
    fn an_explicit_bundle_path_is_absolute_and_never_falls_back_to_source() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/riwork");
        assert!(
            resolve(&source, Some(Path::new("relative.js")))
                .unwrap_err()
                .contains("absolute")
        );
        let explicit = source.join("App.tsx").canonicalize().unwrap();
        assert_eq!(
            resolve(Path::new("/missing-source"), Some(&explicit)).unwrap(),
            explicit
        );
    }
}
