//! The host and headless NSH runner load the same apps/riwork entry bundle.
use nsh_platform_macos::Bundle;
use std::path::{Path, PathBuf};

pub fn load() -> Result<(Bundle, Option<PathBuf>), String> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/riwork");
    let override_path = std::env::var_os("RIWORK_NSH_BUNDLE").map(PathBuf::from);
    let path = resolve(&source, override_path.as_deref())?;
    let bundle =
        Bundle::read(&path).map_err(|error| format!("NSH bundle {}: {error}", path.display()))?;
    Ok((bundle, path.parent().map(Path::to_path_buf)))
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
