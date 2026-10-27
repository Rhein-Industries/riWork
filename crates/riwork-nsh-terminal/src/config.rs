use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "source", rename_all = "camelCase")]
pub(crate) enum Colors {
    #[default]
    Ghostty,
    Override {
        background: String,
        foreground: String,
        palette: [String; 16],
    },
}

impl Colors {
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Override {
            background,
            foreground,
            palette,
        } = self
        {
            if std::iter::once(background)
                .chain(std::iter::once(foreground))
                .chain(palette)
                .any(|v| {
                    v.len() != 7
                        || !v.starts_with('#')
                        || !v[1..].bytes().all(|c| c.is_ascii_hexdigit())
                })
            {
                return Err("invalid: terminal colors must be #RRGGBB".into());
            }
        }
        Ok(())
    }
}

/// One set of source paths per process. Fixture HOME/XDG_CONFIG_HOME therefore
/// isolates both Ghostty preferences and RiWork state without global installs.
pub(crate) fn roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let mut dirs = Vec::new();
    if let Some(xdg) = xdg {
        dirs.push(xdg.join("ghostty"));
    }
    if let Some(home) = home {
        dirs.push(home.join("Library/Application Support/com.mitchellh.ghostty"));
    }
    dirs.into_iter()
        .flat_map(|p| [p.join("config"), p.join("config.ghostty")])
        .collect()
}

pub(crate) fn stamp(roots: &[PathBuf]) -> Vec<(PathBuf, Option<(u64, std::time::SystemTime)>)> {
    let mut paths = roots.to_vec();
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    while let Some(path) = paths.pop() {
        if seen.len() >= 64 || !seen.insert(path.clone()) {
            continue;
        }
        let meta = std::fs::metadata(&path).ok().filter(|m| m.is_file());
        result.push((
            path.clone(),
            meta.as_ref()
                .and_then(|m| Some((m.len(), m.modified().ok()?))),
        ));
        if meta.is_some_and(|m| m.len() <= 1024 * 1024) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                for line in text
                    .lines()
                    .filter(|line| !line.trim_start().starts_with('#'))
                {
                    let Some((key, value)) = line.split_once('=') else {
                        continue;
                    };
                    let value = value.trim().trim_start_matches('?').trim_matches('"');
                    match key.trim() {
                        "config-file" => paths.push(resolve(value, path.parent())),
                        "theme" => {
                            // Both variants are watched even when only one appearance is current.
                            for name in value.split(',').map(|s| {
                                s.trim().split_once(':').map_or(s.trim(), |(_, n)| n.trim())
                            }) {
                                if name.starts_with('/') || name.starts_with("~/") {
                                    paths.push(resolve(name, path.parent()));
                                } else {
                                    for root in roots {
                                        if let Some(dir) = root.parent() {
                                            paths.push(dir.join("themes").join(name));
                                        }
                                    }
                                    if let Some(resources) =
                                        std::env::var_os("GHOSTTY_RESOURCES_DIR")
                                    {
                                        paths.push(
                                            PathBuf::from(resources).join("themes").join(name),
                                        );
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

fn resolve(value: &str, parent: Option<&Path>) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest);
    }
    let p = PathBuf::from(value);
    if p.is_absolute() {
        p
    } else {
        parent.unwrap_or(Path::new(".")).join(p)
    }
}

pub(crate) fn write(
    home: &Path,
    id: &str,
    roots: &[PathBuf],
    colors: &Colors,
    revision: u64,
) -> Result<PathBuf, String> {
    colors.validate()?;
    let dir = home.join("nsh/terminal-config");
    riwork_core::paths::create_private_dir(&dir).map_err(|e| e.to_string())?;
    let mut text = format!("# Generated RiWork terminal configuration; revision {revision}.\n");
    for path in roots.iter().filter(|p| p.is_file()) {
        let path = path
            .to_str()
            .ok_or("Ghostty configuration path is not UTF-8")?;
        if path.contains(['\n', '\r']) {
            return Err("invalid Ghostty config path".into());
        }
        text.push_str(&format!("config-file = {path}\n"));
    }
    let path = dir.join(format!("{id}.conf"));
    std::fs::write(&path, &text).map_err(|e| e.to_string())?;
    // Loaded after recursive user files, so app colors win over a theme.
    text.clear();
    if let Colors::Override {
        background,
        foreground,
        palette,
    } = colors
    {
        text.push_str(&format!(
            "background = {background}\nforeground = {foreground}\n"
        ));
        for (index, color) in palette.iter().enumerate() {
            text.push_str(&format!("palette = {index}={color}\n"));
        }
    }
    std::fs::write(path.with_extension("override.conf"), text).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colors_reject_configuration_injection_and_incomplete_palettes() {
        let good = Colors::Override {
            background: "#102030".into(),
            foreground: "#ffffff".into(),
            palette: std::array::from_fn(|_| "#abcdef".into()),
        };
        assert!(good.validate().is_ok());
        let mut bad = good.clone();
        if let Colors::Override { background, .. } = &mut bad {
            *background = "#000000\ncommand=bad".into();
        }
        assert!(bad.validate().is_err());
        assert!(serde_json::from_value::<Colors>(serde_json::json!({"source":"override","background":"#000000","foreground":"#ffffff","palette":[]})).is_err());
    }
}
