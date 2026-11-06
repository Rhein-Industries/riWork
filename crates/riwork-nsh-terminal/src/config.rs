use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
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
/// isolates both Ghostty preferences and Somtus state without global installs.
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: SystemTime,
    // Detect atomic replacements and edits that restore the previous mtime.
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileStamp {
    fn metadata(metadata: &std::fs::Metadata) -> Option<Self> {
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        })
    }

    fn at(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
        Self::metadata(&metadata)
    }
}

type Stamp = Vec<(PathBuf, Option<FileStamp>)>;
const RESCAN_AFTER: Duration = Duration::from_secs(300);

/// The dependency graph changes only when a source file or resolver environment
/// changes. Idle checks stat the known graph without reading/parsing every config
/// and theme again. A periodic rescan bounds unobservable filesystem changes.
pub(crate) struct Watch {
    roots: Vec<PathBuf>,
    snapshot: Stamp,
    environment: (Option<OsString>, Option<OsString>),
    scanned_at: Instant,
    cacheable: bool,
}

fn resolver_environment() -> (Option<OsString>, Option<OsString>) {
    (
        std::env::var_os("HOME"),
        std::env::var_os("GHOSTTY_RESOURCES_DIR"),
    )
}

impl Watch {
    pub(crate) fn new(roots: Vec<PathBuf>) -> Self {
        let (snapshot, cacheable) = stamp(&roots);
        Self {
            snapshot,
            roots,
            environment: resolver_environment(),
            scanned_at: Instant::now(),
            cacheable,
        }
    }

    pub(crate) fn changed(&mut self) -> bool {
        self.changed_at(Instant::now())
    }

    fn changed_at(&mut self, now: Instant) -> bool {
        let environment = resolver_environment();
        if self.cacheable
            && now.duration_since(self.scanned_at) < RESCAN_AFTER
            && environment == self.environment
            && self
                .snapshot
                .iter()
                .all(|(path, previous)| FileStamp::at(path) == *previous)
        {
            return false;
        }
        let (next, cacheable) = stamp(&self.roots);
        let changed = self.snapshot != next;
        self.snapshot = next;
        self.environment = environment;
        self.scanned_at = now;
        self.cacheable = cacheable;
        changed
    }
}

#[cfg(test)]
thread_local! {
    static SOURCE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn stamp(roots: &[PathBuf]) -> (Stamp, bool) {
    let mut paths = roots.to_vec();
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    let mut cacheable = true;
    while let Some(path) = paths.pop() {
        if seen.len() >= 64 || !seen.insert(path.clone()) {
            continue;
        }
        let meta = std::fs::metadata(&path).ok().filter(|m| m.is_file());
        let signature = meta.as_ref().and_then(FileStamp::metadata);
        cacheable &= meta.is_none() || signature.is_some();
        result.push((path.clone(), signature));
        if meta.is_some_and(|m| m.len() <= 1024 * 1024) {
            #[cfg(test)]
            SOURCE_READS.with(|reads| reads.set(reads.get() + 1));
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
            } else {
                // A transient read/UTF-8 failure must be retried next poll,
                // even when the file's metadata has not changed.
                cacheable = false;
            }
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    (result, cacheable)
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
    let mut text = format!("# Generated Somtus terminal configuration; revision {revision}.\n");
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

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("terminal-config-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, text: impl AsRef<[u8]>) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, text).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unchanged_graph_skips_reads_and_reloads_includes_themes_and_optional_files() {
        let fixture = Fixture::new();
        let theme = fixture.write("theme", "background = #112233\n");
        let child = fixture.write(
            "child",
            format!("theme = {}\nconfig-file = config\n", theme.display()),
        );
        let root = fixture.write("config", "config-file = child\nconfig-file = ?later\n");
        let roots = vec![root.clone()];
        let mut watch = Watch::new(roots.clone());
        assert_eq!(watch.snapshot, stamp(&roots).0);
        SOURCE_READS.with(|reads| reads.set(0));
        for _ in 0..100 {
            assert!(!watch.changed());
        }
        assert_eq!(SOURCE_READS.with(|reads| reads.get()), 0);

        // Same-sized atomic replacement with the previous mtime still changes
        // identity; this includes editors that preserve timestamps on save.
        let previous_mtime = std::fs::metadata(&theme).unwrap().modified().unwrap();
        let replacement = fixture.write("replacement", "background = #445566\n");
        std::fs::File::open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(previous_mtime))
            .unwrap();
        std::fs::rename(replacement, &theme).unwrap();
        assert!(watch.changed());
        assert_eq!(watch.snapshot, stamp(&roots).0);
        let later = fixture.write("later", "foreground = #ffffff\n");
        assert!(watch.changed());
        std::fs::remove_file(&later).unwrap();
        assert!(watch.changed());
        fixture.write("config", "font-size = 14\n");
        assert!(watch.changed());
        assert!(
            !watch
                .snapshot
                .iter()
                .any(|(path, _)| path == &child || path == &theme)
        );
        SOURCE_READS.with(|reads| reads.set(0));
        assert!(!watch.changed_at(watch.scanned_at + RESCAN_AFTER));
        assert_eq!(SOURCE_READS.with(|reads| reads.get()), 1);
    }

    #[test]
    fn failed_source_reads_are_retried_without_waiting_for_metadata_to_change() {
        let fixture = Fixture::new();
        let root = fixture.write("config", [0xff, 0xfe]);
        let mut watch = Watch::new(vec![root]);
        SOURCE_READS.with(|reads| reads.set(0));
        assert!(!watch.changed());
        assert!(!watch.changed());
        assert_eq!(SOURCE_READS.with(|reads| reads.get()), 2);
    }

    #[test]
    #[ignore = "slow: manual terminal config polling benchmark"]
    fn config_polling_cost() {
        let fixture = Fixture::new();
        for index in 0..16 {
            fixture.write(
                &format!("config-{index}"),
                format!(
                    "config-file = config-{}\n# {}\n",
                    (index + 1) % 16,
                    "x".repeat(64 * 1024)
                ),
            );
        }
        let roots = vec![fixture.0.join("config-0")];
        let mut watch = Watch::new(roots.clone());
        let expected = watch.snapshot.clone();
        SOURCE_READS.with(|reads| reads.set(0));
        let started = Instant::now();
        for _ in 0..100 {
            assert_eq!(stamp(&roots).0, expected);
        }
        let before = started.elapsed() / 100;
        let before_reads = SOURCE_READS.with(|reads| reads.replace(0));
        let started = Instant::now();
        for _ in 0..100 {
            assert!(!watch.changed());
        }
        eprintln!(
            "terminal config poll: {:?} -> {:?}; reads {} -> {} (100 polls)",
            before,
            started.elapsed() / 100,
            before_reads,
            SOURCE_READS.with(|reads| reads.get())
        );
        assert_eq!(SOURCE_READS.with(|reads| reads.get()), 0);
    }

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
