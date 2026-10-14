//! Cua Driver's skill pack as the `cua-driver` skill of every agent CLI RiWork
//! launches.
//!
//! Cua Driver keeps one pack (`cua-driver skills path`). RiWork links that
//! directory into the user skill directory each CLI reads. These were checked
//! against the installed CLIs, each listing a linked pack from a throwaway home:
//!
//! - Claude Code reads `$CLAUDE_CONFIG_DIR/skills`, else `~/.claude/skills`,
//!   and not `~/.agents/skills`.
//! - Codex reads `~/.agents/skills` from `HOME` whatever `CODEX_HOME` is, so
//!   one link serves the system home and every Orca account home RiWork binds.
//!   Those homes belong to Orca and are never written.
//! - Grok reads `$GROK_HOME/skills`, else `~/.grok/skills`.
//! - Muse reads `$XDG_CONFIG_HOME/muse/skills`, else `~/.config/muse/skills`.
//!   `~/.local/share/muse/skills` only caches Muse's bundled skills.
//!
//! Grok and Muse also read `~/.agents/skills` and list one skill per name, so
//! a pack they find twice is still one skill.
//!
//! Reading the state never writes. Only explicit setup links: it creates a
//! missing link, repoints a link RiWork made when the pack has moved, and
//! leaves every other file, directory and link alone. A link is RiWork's only
//! while it is the very link RiWork recorded: same path, target and inode.

use std::{
    collections::BTreeMap,
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, symlink},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const SKILL_NAME: &str = "cua-driver";
/// Links RiWork made, under `RIWORK_HOME`, so setup only ever repoints its own.
pub(crate) const MANIFEST: &str = "cua/skill-links.json";
const SKILL_FILE_LIMIT: u64 = 1024 * 1024;
/// A link being swapped into place; one left by an interrupted setup is removed.
const TEMPORARY_PREFIX: &str = ".cua-driver.riwork-";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillCli {
    Claude,
    Codex,
    Grok,
    Muse,
}

impl SkillCli {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Grok => "Grok",
            Self::Muse => "Muse",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillLinkState {
    /// The path resolves to the current pack.
    Linked,
    /// Nothing is there yet; setup links it.
    Missing,
    /// A link RiWork made to where the pack used to be; setup repoints it.
    Stale,
    /// A file, a directory, or a link RiWork did not make, at the path or in
    /// place of its directory; setup leaves it.
    BlockedByForeignPath,
    /// There is no pack to link to.
    PackMissing,
    /// None of the CLI's homes exists, so it is not set up for this user and
    /// setup does not create one.
    HomeAbsent,
    /// RiWork cannot tell where the CLI reads skills: the variable that moves
    /// its home is a relative path, which each session resolves on its own.
    Unsupported,
}

impl SkillLinkState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::Missing => "missing",
            Self::Stale => "stale link",
            Self::BlockedByForeignPath => "blocked by foreign path",
            Self::PackMissing => "pack missing",
            Self::HomeAbsent => "no home",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SkillTarget {
    pub cli: SkillCli,
    /// The CLI, and the variable that moved its home when one did.
    pub name: String,
    /// The `cua-driver` entry in the skills directory the CLI reads.
    pub path: PathBuf,
    pub state: SkillLinkState,
    /// The variable that moved this home, as RiWork's launches inherit it.
    pub variable: Option<&'static str>,
    /// The CLI homes whose sessions read `path`.
    pub homes: Vec<PathBuf>,
    pub detail: Option<String>,
}

/// Whether the skill can be found by sessions RiWork starts. A linked skill is
/// discovered when a session starts; running sessions are not reloaded.
#[derive(Clone, Debug, Serialize)]
pub struct CuaSkills {
    /// Where Cua Driver keeps its pack, as the driver reports it.
    pub pack: Option<PathBuf>,
    pub pack_installed: bool,
    pub message: String,
    pub targets: Vec<SkillTarget>,
}

impl CuaSkills {
    /// Setup would change something: fetch the pack, or create or repoint a link.
    pub fn setup_needed(&self) -> bool {
        (self.pack.is_some() && !self.pack_installed)
            || self.targets.iter().any(|target| {
                matches!(
                    target.state,
                    SkillLinkState::Missing | SkillLinkState::Stale
                )
            })
    }
}

/// The skill homes RiWork's launches use, from the environment they inherit:
/// sessions receive RiWork's `CLAUDE_CONFIG_DIR` and `GROK_HOME`, Muse follows
/// `XDG_CONFIG_HOME`, and each Codex launch gets its bound account's home.
#[derive(Clone, Debug, Default)]
pub(crate) struct LaunchHomes {
    pub(crate) claude_config_dir: Option<PathBuf>,
    pub(crate) grok_home: Option<PathBuf>,
    pub(crate) xdg_config_home: Option<PathBuf>,
    /// `None` reads the Codex homes from RiWork's saved accounts when needed.
    pub(crate) codex_homes: Option<Vec<PathBuf>>,
}

impl LaunchHomes {
    pub(crate) fn from_environment() -> Self {
        let variable = |name: &str| {
            env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Self {
            claude_config_dir: variable("CLAUDE_CONFIG_DIR"),
            grok_home: variable("GROK_HOME"),
            xdg_config_home: variable("XDG_CONFIG_HOME"),
            codex_homes: None,
        }
    }
}

/// Where Cua Driver keeps its pack.
#[derive(Clone, Debug)]
pub(crate) enum Pack {
    /// A directory whose `SKILL.md` names `cua-driver`.
    Installed(PathBuf),
    /// The driver's pack location, without a pack in it.
    Absent(PathBuf),
    /// No location: the driver is missing, refused, or did not answer.
    Unknown(String),
}

impl Pack {
    pub(crate) fn at(path: PathBuf) -> Self {
        if is_pack(&path) {
            Self::Installed(path)
        } else {
            Self::Absent(path)
        }
    }

    fn path(&self) -> Option<&Path> {
        match self {
            Self::Installed(path) | Self::Absent(path) => Some(path),
            Self::Unknown(_) => None,
        }
    }
}

/// The single absolute path `cua-driver skills path` prints, also when no
/// pack is there yet.
pub(crate) fn parse_pack_path(output: &str) -> Result<PathBuf, String> {
    let path = output.trim();
    if path.is_empty() || path.contains('\n') || !Path::new(path).is_absolute() {
        return Err(format!(
            "Cua Driver reported an unexpected skill pack location: {}",
            super::snippet(path)
        ));
    }
    Ok(PathBuf::from(path))
}

fn is_pack(dir: &Path) -> bool {
    let mut bytes = Vec::new();
    fs::File::open(dir.join("SKILL.md"))
        .and_then(|file| file.take(SKILL_FILE_LIMIT).read_to_end(&mut bytes))
        .is_ok()
        && frontmatter_name(&String::from_utf8_lossy(&bytes)) == Some(SKILL_NAME)
}

fn frontmatter_name(text: &str) -> Option<&str> {
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    lines
        .take_while(|line| line.trim() != "---")
        .find_map(|line| line.strip_prefix("name:"))
        .map(|name| name.trim().trim_matches(['"', '\'']))
}

struct Location {
    cli: SkillCli,
    path: PathBuf,
    variable: Option<&'static str>,
    homes: Vec<PathBuf>,
}

/// Each CLI's default home, plus the one its variable names when RiWork's
/// launches inherit it: an app opened from the Dock starts its sessions
/// without the variables a terminal has.
fn locations(user_home: &Path, homes: &LaunchHomes, codex_homes: Vec<PathBuf>) -> Vec<Location> {
    let mut locations: Vec<Location> = Vec::new();
    let mut add =
        |cli: SkillCli, variable: Option<&'static str>, skills: PathBuf, homes: Vec<PathBuf>| {
            let path = skills.join(SKILL_NAME);
            if !locations.iter().any(|location| location.path == path) {
                locations.push(Location {
                    cli,
                    path,
                    variable,
                    homes,
                });
            }
        };
    let claude = user_home.join(".claude");
    add(SkillCli::Claude, None, claude.join("skills"), vec![claude]);
    if let Some(config) = &homes.claude_config_dir {
        let variable = Some("CLAUDE_CONFIG_DIR");
        add(
            SkillCli::Claude,
            variable,
            config.join("skills"),
            vec![config.clone()],
        );
    }
    // `~/.agents` is not Codex's home: Codex is set up when a home it is
    // launched with exists.
    add(
        SkillCli::Codex,
        None,
        user_home.join(".agents/skills"),
        codex_homes,
    );
    let grok = user_home.join(".grok");
    add(SkillCli::Grok, None, grok.join("skills"), vec![grok]);
    if let Some(grok) = &homes.grok_home {
        add(
            SkillCli::Grok,
            Some("GROK_HOME"),
            grok.join("skills"),
            vec![grok.clone()],
        );
    }
    // Muse creates its configuration directory whenever it runs.
    let muse = user_home.join(".config/muse");
    add(SkillCli::Muse, None, muse.join("skills"), vec![muse]);
    if let Some(config) = &homes.xdg_config_home {
        let muse = config.join("muse");
        add(
            SkillCli::Muse,
            Some("XDG_CONFIG_HOME"),
            muse.join("skills"),
            vec![muse],
        );
    }
    locations
}

/// A link RiWork made, as recorded when it made it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedLink {
    target: PathBuf,
    device: u64,
    inode: u64,
}

impl ManagedLink {
    fn of(link: &Path) -> Option<Self> {
        let metadata = fs::symlink_metadata(link).ok()?;
        if !metadata.file_type().is_symlink() {
            return None;
        }
        Some(Self {
            target: fs::read_link(link).ok()?,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    /// The link at `path` is still this one. A link someone removed and made
    /// again, even to the same target, is theirs.
    fn is_at(&self, path: &Path) -> bool {
        Self::of(path).as_ref() == Some(self)
    }
}

type Managed = BTreeMap<PathBuf, ManagedLink>;

fn inspect(
    location: &Location,
    pack: &Pack,
    managed: &Managed,
) -> (SkillLinkState, Option<String>) {
    if let Some(variable) = location.variable
        && location.homes.iter().any(|home| home.is_relative())
    {
        return (
            SkillLinkState::Unsupported,
            Some(format!(
                "{variable} is a relative path, which each session resolves from its own directory"
            )),
        );
    }
    if !location.homes.iter().any(|home| home.is_dir()) {
        return (SkillLinkState::HomeAbsent, None);
    }
    let installed = matches!(pack, Pack::Installed(_));
    let without_pack = |state| {
        if installed {
            state
        } else {
            SkillLinkState::PackMissing
        }
    };
    let metadata = match fs::symlink_metadata(&location.path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return match blocked_directory(&location.path) {
                Some(detail) => (SkillLinkState::BlockedByForeignPath, Some(detail)),
                None => (without_pack(SkillLinkState::Missing), None),
            };
        }
        Err(error) => {
            return (
                SkillLinkState::BlockedByForeignPath,
                Some(format!("cannot be inspected: {error}")),
            );
        }
    };
    let resolves_to_pack = || {
        pack.path().is_some_and(|pack| {
            matches!(
                (fs::canonicalize(&location.path), fs::canonicalize(pack)),
                (Ok(path), Ok(pack)) if path == pack
            )
        })
    };
    if !metadata.file_type().is_symlink() {
        // A skills directory linked to the pack's own parent, for example.
        if resolves_to_pack() {
            return (without_pack(SkillLinkState::Linked), None);
        }
        let kind = if metadata.is_dir() {
            "a directory"
        } else {
            "a file"
        };
        return (
            SkillLinkState::BlockedByForeignPath,
            Some(format!("{kind} RiWork did not create")),
        );
    }
    let target = fs::read_link(&location.path).ok();
    let links_to = || {
        target
            .as_ref()
            .map(|target| format!("links to {}", target.display()))
    };
    let Some(pack_path) = pack.path() else {
        return (SkillLinkState::PackMissing, links_to());
    };
    if target.as_deref() == Some(pack_path) || resolves_to_pack() {
        return (without_pack(SkillLinkState::Linked), None);
    }
    if managed
        .get(&location.path)
        .is_some_and(|link| link.is_at(&location.path))
    {
        return (without_pack(SkillLinkState::Stale), links_to());
    }
    (
        SkillLinkState::BlockedByForeignPath,
        Some(match target {
            Some(target) => format!("a link to {} that RiWork did not create", target.display()),
            None => "a link RiWork did not create".to_owned(),
        }),
    )
}

/// Setup would have to create the link's directory where something else is:
/// a file, or a link to a directory that is gone.
fn blocked_directory(link: &Path) -> Option<String> {
    let parent = link.parent()?;
    let metadata = fs::symlink_metadata(parent).ok()?;
    if parent.is_dir() {
        return None;
    }
    Some(if metadata.file_type().is_symlink() {
        format!(
            "{} links to a directory that does not exist",
            parent.display()
        )
    } else {
        format!("{} is not a directory", parent.display())
    })
}

/// Read-only: what each CLI would find, and what setup would do.
pub(crate) fn report(
    user_home: Option<&Path>,
    homes: &LaunchHomes,
    codex_homes: Vec<PathBuf>,
    pack: &Pack,
    manifest: &Path,
) -> CuaSkills {
    report_with(
        user_home,
        homes,
        codex_homes,
        pack,
        &read_manifest(manifest),
        &BTreeMap::new(),
    )
}

fn report_with(
    user_home: Option<&Path>,
    homes: &LaunchHomes,
    codex_homes: Vec<PathBuf>,
    pack: &Pack,
    managed: &Managed,
    errors: &BTreeMap<PathBuf, String>,
) -> CuaSkills {
    let Some(user_home) = user_home else {
        let no_home = "HOME is not set, so RiWork cannot find the agent CLIs' skill directories.";
        return CuaSkills {
            pack: pack.path().map(Path::to_path_buf),
            pack_installed: matches!(pack, Pack::Installed(_)),
            message: match summary(pack, &[]) {
                pack if pack.is_empty() => no_home.to_owned(),
                pack => format!("{pack} {no_home}"),
            },
            targets: Vec::new(),
        };
    };
    let targets: Vec<SkillTarget> = locations(user_home, homes, codex_homes)
        .into_iter()
        .map(|location| {
            let (state, detail) = inspect(&location, pack, managed);
            let name = match location.variable {
                Some(variable) => format!("{} (${variable})", location.cli.label()),
                None => location.cli.label().to_owned(),
            };
            SkillTarget {
                cli: location.cli,
                name,
                detail: errors.get(&location.path).cloned().or(detail),
                path: location.path,
                state,
                variable: location.variable,
                homes: location.homes,
            }
        })
        .collect();
    CuaSkills {
        pack: pack.path().map(Path::to_path_buf),
        pack_installed: matches!(pack, Pack::Installed(_)),
        message: summary(pack, &targets),
        targets,
    }
}

/// Explicit setup only: create missing links, repoint stale ones RiWork made,
/// and record them. A target that cannot be linked keeps its reason.
pub(crate) fn link(
    user_home: Option<&Path>,
    homes: &LaunchHomes,
    codex_homes: Vec<PathBuf>,
    pack: &Pack,
    manifest: &Path,
) -> CuaSkills {
    let mut managed = read_manifest(manifest);
    let mut errors = BTreeMap::new();
    if let (Some(user_home), Pack::Installed(pack_path)) = (user_home, pack) {
        for location in locations(user_home, homes, codex_homes.clone()) {
            let result = match inspect(&location, pack, &managed).0 {
                SkillLinkState::Missing => create_link(&location.path, pack_path),
                SkillLinkState::Stale => {
                    replace_link(&location.path, pack_path, &managed[&location.path])
                }
                _ => continue,
            };
            match result.and_then(|()| {
                ManagedLink::of(&location.path)
                    .ok_or_else(|| format!("{} vanished after linking", location.path.display()))
            }) {
                Ok(made) => {
                    managed.insert(location.path, made);
                }
                Err(error) => {
                    errors.insert(location.path, error);
                }
            }
        }
    }
    // A recorded link someone has since replaced or made again is not RiWork's.
    managed.retain(|path, link| link.is_at(path));
    if let Err(error) = write_manifest(manifest, &managed) {
        errors.insert(manifest.to_path_buf(), error);
    }
    let mut skills = report_with(user_home, homes, codex_homes, pack, &managed, &errors);
    if let Some(error) = errors.get(manifest) {
        skills.message = format!("{} {error}", skills.message);
    }
    skills
}

fn create_link(link: &Path, pack: &Path) -> Result<(), String> {
    let parent = link
        .parent()
        .ok_or("A skill link needs a parent directory")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
    // Fails rather than replaces when something appeared at the path meanwhile.
    symlink(pack, link).map_err(|error| format!("Cannot link {}: {error}", link.display()))
}

fn replace_link(link: &Path, pack: &Path, recorded: &ManagedLink) -> Result<(), String> {
    let parent = link
        .parent()
        .ok_or("A skill link needs a parent directory")?;
    remove_interrupted_links(parent);
    let temporary = parent.join(format!("{TEMPORARY_PREFIX}{}", uuid::Uuid::new_v4()));
    symlink(pack, &temporary)
        .map_err(|error| format!("Cannot link {}: {error}", link.display()))?;
    swap_in(link, &temporary, recorded)
}

/// Puts the link at `temporary` in place of the recorded link at `link`, and
/// removes `temporary` whatever happens to it.
#[cfg(target_os = "macos")]
fn swap_in(link: &Path, temporary: &Path, recorded: &ManagedLink) -> Result<(), String> {
    if !recorded.is_at(link) {
        let _ = fs::remove_file(temporary);
        return Err(changed(link));
    }
    exchange_recorded(link, temporary, recorded)
}

fn changed(link: &Path) -> String {
    format!(
        "{} changed while setup ran; it was left as it is",
        link.display()
    )
}

/// Exchanges the two atomically and keeps the exchange only if what came out
/// is the recorded link; anything else goes back where it was.
#[cfg(target_os = "macos")]
fn exchange_recorded(link: &Path, temporary: &Path, recorded: &ManagedLink) -> Result<(), String> {
    if let Err(error) = exchange(temporary, link) {
        let _ = fs::remove_file(temporary);
        return Err(format!("Cannot repoint {}: {error}", link.display()));
    }
    // The exchange is atomic, so what came out is exactly what was at the path.
    if recorded.is_at(temporary) {
        let _ = fs::remove_file(temporary);
        return Ok(());
    }
    // Something took the link's place since the check: put it back.
    exchange(temporary, link).map_err(|error| {
        format!(
            "{} changed while setup ran and could not be put back ({error}); it is at {}",
            link.display(),
            temporary.display()
        )
    })?;
    let _ = fs::remove_file(temporary);
    Err(changed(link))
}

#[cfg(target_os = "macos")]
fn exchange(first: &Path, second: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let first = CString::new(first.as_os_str().as_bytes())?;
    let second = CString::new(second.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    if unsafe { libc::renamex_np(first.as_ptr(), second.as_ptr(), libc::RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Without an atomic exchange, the check and the rename are separate steps.
#[cfg(not(target_os = "macos"))]
fn swap_in(link: &Path, temporary: &Path, recorded: &ManagedLink) -> Result<(), String> {
    let result = if recorded.is_at(link) {
        // Renaming replaces the link itself, never what it points to.
        fs::rename(temporary, link)
            .map_err(|error| format!("Cannot repoint {}: {error}", link.display()))
    } else {
        Err(changed(link))
    };
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Links an interrupted setup left while swapping. Only links with RiWork's
/// own prefix are removed, never what they point to.
fn remove_interrupted_links(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let ours = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(TEMPORARY_PREFIX));
        if ours && entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    links: Managed,
}

/// An unreadable manifest proves nothing is RiWork's, so nothing is repointed.
fn read_manifest(path: &Path) -> Managed {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
        .filter(|manifest| manifest.version == 1)
        .map(|manifest| manifest.links)
        .unwrap_or_default()
}

fn write_manifest(path: &Path, links: &Managed) -> Result<(), String> {
    if !path.exists() && links.is_empty() {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&Manifest {
        version: 1,
        links: links.clone(),
    })
    .map_err(|error| error.to_string())?;
    if fs::read(path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or("The skill manifest needs a directory")?;
    fs::create_dir_all(parent).map_err(|error| format!("Cannot record skill links: {error}"))?;
    let temporary = parent.join(format!(".skill-links-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })()
    .map_err(|error| format!("Cannot record skill links: {error}"));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn summary(pack: &Pack, targets: &[SkillTarget]) -> String {
    let path = match pack {
        Pack::Unknown(reason) => {
            return format!("Cua Driver's skill pack was not found: {reason}");
        }
        Pack::Absent(path) => {
            return format!(
                "Cua Driver's skill pack is not installed at {}. Setup fetches it with `cua-driver skills install`.",
                path.display()
            );
        }
        Pack::Installed(path) => path,
    };
    let names = |states: &[SkillLinkState]| {
        let names: Vec<&str> = targets
            .iter()
            .filter(|target| states.contains(&target.state))
            .map(|target| target.name.as_str())
            .collect();
        match names.as_slice() {
            [] => None,
            [one] => Some((*one).to_owned()),
            [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
        }
    };
    let mut parts = Vec::new();
    if let Some(names) = names(&[SkillLinkState::Linked]) {
        parts.push(format!(
            "The cua-driver skill is linked for {names}. New sessions that use those homes find it when they start; running sessions keep the skills they started with."
        ));
    }
    if let Some(names) = names(&[SkillLinkState::Missing, SkillLinkState::Stale]) {
        parts.push(format!(
            "Run `riwork setup` to link {} for {names}.",
            path.display()
        ));
    }
    if let Some(names) = names(&[SkillLinkState::BlockedByForeignPath]) {
        parts.push(format!(
            "Something RiWork did not create is in the way for {names}; RiWork leaves it alone."
        ));
    }
    if let Some(names) = names(&[SkillLinkState::HomeAbsent]) {
        parts.push(format!("No home was found for {names}."));
    }
    if let Some(names) = names(&[SkillLinkState::Unsupported]) {
        parts.push(format!(
            "RiWork cannot tell where {names} reads skills because its home variable is a relative path."
        ));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cua::TemporaryDirectory;

    struct Fixture {
        temp: TemporaryDirectory,
        home: PathBuf,
        manifest: PathBuf,
        homes: LaunchHomes,
    }

    impl Fixture {
        /// A user home where every CLI has run once, and a pack at `pack/cua-driver`.
        fn new() -> Self {
            let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-skills").unwrap();
            let root = temp.path.canonicalize().unwrap();
            let home = root.join("home");
            for cli_home in [".claude", ".codex", ".grok", ".config/muse"] {
                fs::create_dir_all(home.join(cli_home)).unwrap();
            }
            let fixture = Self {
                manifest: root.join("state").join(MANIFEST),
                homes: LaunchHomes {
                    codex_homes: Some(vec![home.join(".codex")]),
                    ..LaunchHomes::default()
                },
                home,
                temp,
            };
            fixture.write_pack("pack/cua-driver");
            fixture
        }

        fn root(&self) -> PathBuf {
            self.temp.path.canonicalize().unwrap()
        }

        fn write_pack(&self, relative: &str) -> PathBuf {
            let pack = self.root().join(relative);
            fs::create_dir_all(&pack).unwrap();
            fs::write(
                pack.join("SKILL.md"),
                "---\nname: cua-driver\ndescription: fixture pack\n---\nUse Cua.\n",
            )
            .unwrap();
            pack
        }

        fn pack(&self) -> Pack {
            Pack::at(self.root().join("pack/cua-driver"))
        }

        fn report(&self, pack: &Pack) -> CuaSkills {
            report(
                Some(&self.home),
                &self.homes,
                self.homes.codex_homes.clone().unwrap(),
                pack,
                &self.manifest,
            )
        }

        fn link(&self, pack: &Pack) -> CuaSkills {
            link(
                Some(&self.home),
                &self.homes,
                self.homes.codex_homes.clone().unwrap(),
                pack,
                &self.manifest,
            )
        }

        fn path(&self, cli: &str) -> PathBuf {
            self.home.join(cli).join("skills").join(SKILL_NAME)
        }
    }

    fn states(skills: &CuaSkills) -> Vec<(String, SkillLinkState)> {
        skills
            .targets
            .iter()
            .map(|target| (target.name.clone(), target.state))
            .collect()
    }

    fn all(state: SkillLinkState) -> Vec<(String, SkillLinkState)> {
        ["Claude Code", "Codex", "Grok", "Muse"]
            .into_iter()
            .map(|name| (name.to_owned(), state))
            .collect()
    }

    /// Every entry under `root`: its kind, a link's target, a file's contents.
    fn tree(root: &Path) -> BTreeMap<PathBuf, String> {
        let mut entries = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                let description = if metadata.file_type().is_symlink() {
                    format!("link {}", fs::read_link(&path).unwrap().display())
                } else if metadata.is_dir() {
                    pending.push(path.clone());
                    "directory".to_owned()
                } else {
                    format!("file {}", fs::read_to_string(&path).unwrap_or_default())
                };
                entries.insert(path, description);
            }
        }
        entries
    }

    #[test]
    fn setup_links_the_pack_for_every_cli_once() {
        let fixture = Fixture::new();
        let pack = fixture.pack();
        let skills = fixture.link(&pack);
        assert_eq!(states(&skills), all(SkillLinkState::Linked), "{skills:?}");
        assert!(skills.pack_installed && !skills.setup_needed());
        let pack_path = fixture.root().join("pack/cua-driver");
        let expected = [
            fixture.path(".claude"),
            fixture.path(".agents"),
            fixture.path(".grok"),
            fixture.path(".config/muse"),
        ];
        for (target, path) in skills.targets.iter().zip(&expected) {
            assert_eq!(&target.path, path);
            assert_eq!(fs::read_link(path).unwrap(), pack_path);
        }
        assert_eq!(read_manifest(&fixture.manifest).len(), 4);
        assert!(
            skills
                .message
                .contains("linked for Claude Code, Codex, Grok and Muse")
        );
        assert!(
            skills
                .message
                .contains("running sessions keep the skills they started with")
        );

        // A second run finds nothing to do and changes nothing.
        let before = tree(&fixture.root());
        let again = fixture.link(&pack);
        assert_eq!(states(&again), all(SkillLinkState::Linked));
        assert_eq!(tree(&fixture.root()), before);
    }

    #[test]
    fn status_only_reads() {
        let fixture = Fixture::new();
        let before = tree(&fixture.root());
        let skills = fixture.report(&fixture.pack());
        assert_eq!(states(&skills), all(SkillLinkState::Missing));
        assert!(skills.setup_needed());
        assert!(
            skills.message.contains("Run `riwork setup`"),
            "{}",
            skills.message
        );
        assert_eq!(tree(&fixture.root()), before);
        for pack in [
            Pack::Absent(fixture.root().join("nowhere")),
            Pack::Unknown("Cua Driver is not installed.".to_owned()),
        ] {
            fixture.report(&pack);
        }
        assert_eq!(tree(&fixture.root()), before);
    }

    #[test]
    fn a_link_to_the_pack_made_by_someone_else_counts_as_linked_and_stays_theirs() {
        let fixture = Fixture::new();
        // Cua Driver's own `skills install` links Claude Code and Codex; a
        // relative link resolves from `home/.claude/skills`.
        for (cli, target) in [
            (".claude", PathBuf::from("../../../pack/cua-driver")),
            (".agents", fixture.root().join("pack/cua-driver")),
        ] {
            fs::create_dir_all(fixture.path(cli).parent().unwrap()).unwrap();
            symlink(target, fixture.path(cli)).unwrap();
        }
        let skills = fixture.link(&fixture.pack());
        assert_eq!(states(&skills), all(SkillLinkState::Linked));
        assert_eq!(
            fs::read_link(fixture.path(".claude")).unwrap(),
            Path::new("../../../pack/cua-driver")
        );
        // Only the links RiWork made are recorded as its own.
        let managed = read_manifest(&fixture.manifest);
        assert_eq!(
            managed.keys().cloned().collect::<Vec<_>>(),
            [fixture.path(".config/muse"), fixture.path(".grok")]
        );
    }

    #[test]
    fn a_stale_riwork_link_is_repointed_and_foreign_paths_are_never_touched() {
        let fixture = Fixture::new();
        fixture.link(&fixture.pack());
        // The pack moves; Claude Code's link is replaced by the user with their
        // own copy, and Muse's is repointed by hand.
        let moved = Pack::at(fixture.write_pack("moved/cua-driver"));
        fs::remove_file(fixture.path(".claude")).unwrap();
        fs::create_dir(fixture.path(".claude")).unwrap();
        fs::write(fixture.path(".claude").join("SKILL.md"), "mine").unwrap();
        let theirs = fixture.root().join("theirs");
        fs::create_dir(&theirs).unwrap();
        fs::remove_file(fixture.path(".config/muse")).unwrap();
        symlink(&theirs, fixture.path(".config/muse")).unwrap();
        // Grok's skills directory holds an unrelated file under the same name.
        fs::remove_file(fixture.path(".grok")).unwrap();
        fs::write(fixture.path(".grok"), "a file").unwrap();

        let untouched = tree(&fixture.root());
        let before = fixture.report(&moved);
        assert_eq!(tree(&fixture.root()), untouched);
        assert_eq!(
            states(&before),
            [
                (
                    "Claude Code".to_owned(),
                    SkillLinkState::BlockedByForeignPath
                ),
                ("Codex".to_owned(), SkillLinkState::Stale),
                ("Grok".to_owned(), SkillLinkState::BlockedByForeignPath),
                ("Muse".to_owned(), SkillLinkState::BlockedByForeignPath),
            ]
        );
        let details: Vec<_> = before
            .targets
            .iter()
            .map(|target| target.detail.clone())
            .collect();
        assert_eq!(
            details[0].as_deref(),
            Some("a directory RiWork did not create")
        );
        assert_eq!(details[2].as_deref(), Some("a file RiWork did not create"));
        assert_eq!(
            details[3],
            Some(format!(
                "a link to {} that RiWork did not create",
                theirs.display()
            ))
        );
        assert!(
            before
                .message
                .contains("in the way for Claude Code, Grok and Muse")
        );

        let after = fixture.link(&moved);
        assert_eq!(after.targets[1].state, SkillLinkState::Linked);
        assert_eq!(
            fs::read_link(fixture.path(".agents")).unwrap(),
            fixture.root().join("moved/cua-driver")
        );
        assert_eq!(
            fs::read_to_string(fixture.path(".claude").join("SKILL.md")).unwrap(),
            "mine"
        );
        assert_eq!(fs::read_to_string(fixture.path(".grok")).unwrap(), "a file");
        assert_eq!(fs::read_link(fixture.path(".config/muse")).unwrap(), theirs);
        // Links that are no longer RiWork's are no longer recorded as its own.
        assert_eq!(
            read_manifest(&fixture.manifest)
                .into_iter()
                .map(|(path, link)| (path, link.target))
                .collect::<Vec<_>>(),
            [(
                fixture.path(".agents"),
                fixture.root().join("moved/cua-driver")
            )]
        );
    }

    #[test]
    fn without_a_manifest_no_link_is_treated_as_riworks() {
        let fixture = Fixture::new();
        fixture.link(&fixture.pack());
        fs::write(&fixture.manifest, "not json").unwrap();
        let moved = Pack::at(fixture.write_pack("moved/cua-driver"));
        let skills = fixture.link(&moved);
        assert_eq!(states(&skills), all(SkillLinkState::BlockedByForeignPath));
        assert_eq!(
            fs::read_link(fixture.path(".grok")).unwrap(),
            fixture.root().join("pack/cua-driver")
        );
    }

    #[test]
    fn a_missing_pack_or_home_is_reported_and_nothing_is_created() {
        let fixture = Fixture::new();
        fs::remove_dir_all(fixture.home.join(".grok")).unwrap();
        fs::remove_dir_all(fixture.home.join(".config")).unwrap();
        let before = tree(&fixture.root());
        let absent = Pack::Absent(fixture.root().join("no-pack/cua-driver"));
        for pack in [
            &absent,
            &Pack::Unknown("Cua Driver is not installed.".to_owned()),
        ] {
            let skills = fixture.link(pack);
            assert_eq!(
                states(&skills),
                [
                    ("Claude Code".to_owned(), SkillLinkState::PackMissing),
                    ("Codex".to_owned(), SkillLinkState::PackMissing),
                    ("Grok".to_owned(), SkillLinkState::HomeAbsent),
                    ("Muse".to_owned(), SkillLinkState::HomeAbsent),
                ]
            );
            assert_eq!(tree(&fixture.root()), before);
        }
        assert!(fixture.report(&absent).setup_needed());
        assert!(fixture.report(&absent).message.contains("not installed at"));
        let skills = fixture.link(&fixture.pack());
        assert_eq!(skills.targets[2].state, SkillLinkState::HomeAbsent);
        assert!(!fixture.home.join(".grok").exists() && !fixture.home.join(".config").exists());
        assert!(
            skills
                .message
                .contains("No home was found for Grok and Muse."),
            "{}",
            skills.message
        );
        // Muse's data directory alone is not a Muse home: Muse makes its
        // configuration directory whenever it runs.
        fs::create_dir_all(fixture.home.join(".local/share/muse")).unwrap();
        let skills = fixture.link(&fixture.pack());
        assert_eq!(skills.targets[3].state, SkillLinkState::HomeAbsent);
        assert!(!fixture.home.join(".config").exists());
        let without_home = report(
            None,
            &fixture.homes,
            vec![],
            &fixture.pack(),
            &fixture.manifest,
        );
        assert!(without_home.targets.is_empty() && without_home.message.contains("HOME"));
    }

    #[test]
    fn relocated_homes_get_their_own_links_and_codex_accounts_share_one() {
        let mut fixture = Fixture::new();
        let root = fixture.root();
        let claude = root.join("profiles/claude");
        let grok = root.join("profiles/grok");
        let config = root.join("profiles/config");
        let accounts = [
            root.join("orca/codex-accounts/account-a/home"),
            root.join("orca/codex-accounts/account-b/home"),
        ];
        for directory in [&claude, &grok, &config.join("muse")]
            .into_iter()
            .chain(&accounts)
        {
            fs::create_dir_all(directory).unwrap();
        }
        fixture.homes = LaunchHomes {
            claude_config_dir: Some(claude.clone()),
            grok_home: Some(grok.clone()),
            xdg_config_home: Some(config.clone()),
            codex_homes: Some(
                std::iter::once(fixture.home.join(".codex"))
                    .chain(accounts.iter().cloned())
                    .collect(),
            ),
        };
        let skills = fixture.link(&fixture.pack());
        let summary: Vec<_> = skills
            .targets
            .iter()
            .map(|target| {
                (
                    target.name.as_str(),
                    target.variable,
                    target.path.clone(),
                    target.state,
                )
            })
            .collect();
        let linked = SkillLinkState::Linked;
        assert_eq!(
            summary,
            [
                ("Claude Code", None, fixture.path(".claude"), linked),
                (
                    "Claude Code ($CLAUDE_CONFIG_DIR)",
                    Some("CLAUDE_CONFIG_DIR"),
                    claude.join("skills/cua-driver"),
                    linked
                ),
                ("Codex", None, fixture.path(".agents"), linked),
                ("Grok", None, fixture.path(".grok"), linked),
                (
                    "Grok ($GROK_HOME)",
                    Some("GROK_HOME"),
                    grok.join("skills/cua-driver"),
                    linked
                ),
                ("Muse", None, fixture.path(".config/muse"), linked),
                (
                    "Muse ($XDG_CONFIG_HOME)",
                    Some("XDG_CONFIG_HOME"),
                    config.join("muse/skills/cua-driver"),
                    linked
                ),
            ]
        );
        // Codex reads `~/.agents/skills` under every account's `CODEX_HOME`,
        // so the account homes, which belong to Orca, are listed and left alone.
        assert_eq!(skills.targets[2].homes.len(), 3);
        assert_eq!(skills.targets[2].homes[1..], accounts);
        for account in &accounts {
            assert_eq!(fs::read_dir(account).unwrap().count(), 0);
        }
        // A variable naming the default home adds nothing.
        fixture.homes.claude_config_dir = Some(fixture.home.join(".claude"));
        assert_eq!(fixture.report(&fixture.pack()).targets.len(), 6);
        // Codex with no home it launches with is not set up.
        fixture.homes.codex_homes = Some(vec![root.join("missing-codex")]);
        let codex = fixture.report(&fixture.pack()).targets[1].clone();
        assert_eq!(
            (codex.cli, codex.state),
            (SkillCli::Codex, SkillLinkState::HomeAbsent)
        );
    }

    #[test]
    fn the_pack_is_the_directory_the_driver_names_with_a_cua_driver_skill() {
        assert_eq!(
            parse_pack_path("/u/.cua-driver/skills/cua-driver\n").unwrap(),
            Path::new("/u/.cua-driver/skills/cua-driver")
        );
        for output in [
            "",
            "relative/pack",
            "/a\n/b",
            "Local skill pack: not installed",
        ] {
            assert!(parse_pack_path(output).is_err(), "{output:?}");
        }
        let fixture = Fixture::new();
        assert!(matches!(fixture.pack(), Pack::Installed(_)));
        let other = fixture.root().join("other");
        fs::create_dir_all(&other).unwrap();
        assert!(matches!(Pack::at(other.clone()), Pack::Absent(_)));
        for (skill, installed) in [
            ("---\nname: other\n---\n", false),
            ("name: cua-driver\n", false),
            ("---\ndescription: x\n---\nname: cua-driver\n", false),
            ("---\nname: \"cua-driver\"\n---\n", true),
        ] {
            fs::write(other.join("SKILL.md"), skill).unwrap();
            assert_eq!(
                matches!(Pack::at(other.clone()), Pack::Installed(_)),
                installed,
                "{skill}"
            );
        }
    }

    #[test]
    fn a_link_made_again_by_someone_else_is_theirs_even_with_the_same_target() {
        let fixture = Fixture::new();
        fixture.link(&fixture.pack());
        // Cua Driver's own `skills update`, or a dotfiles tool, makes Grok's
        // link again. The old one is kept so its inode cannot be reused.
        let grok = fixture.path(".grok");
        let target = fs::read_link(&grok).unwrap();
        fs::rename(&grok, fixture.root().join("kept")).unwrap();
        symlink(&target, &grok).unwrap();
        let moved = Pack::at(fixture.write_pack("moved/cua-driver"));
        let skills = fixture.link(&moved);
        assert_eq!(skills.targets[1].state, SkillLinkState::Linked);
        assert_eq!(
            skills.targets[2].state,
            SkillLinkState::BlockedByForeignPath
        );
        assert_eq!(fs::read_link(&grok).unwrap(), target);
        assert!(!read_manifest(&fixture.manifest).contains_key(&grok));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_exchange_that_brings_out_something_else_puts_it_back() {
        let fixture = Fixture::new();
        fixture.link(&fixture.pack());
        let link = fixture.path(".grok");
        let recorded = read_manifest(&fixture.manifest)[&link].clone();
        // After setup's check, the user put a file there.
        fs::remove_file(&link).unwrap();
        fs::write(&link, "theirs").unwrap();
        let temporary = link.with_file_name(format!("{TEMPORARY_PREFIX}test"));
        symlink(fixture.root().join("moved"), &temporary).unwrap();
        let error = exchange_recorded(&link, &temporary, &recorded).unwrap_err();
        assert!(error.contains("changed while setup ran"), "{error}");
        assert_eq!(fs::read_to_string(&link).unwrap(), "theirs");
        assert!(fs::symlink_metadata(&temporary).is_err());
        // The check before it refuses as well, and leaves nothing behind.
        symlink(fixture.root().join("moved"), &temporary).unwrap();
        assert!(swap_in(&link, &temporary, &recorded).is_err());
        assert_eq!(fs::read_to_string(&link).unwrap(), "theirs");
        assert!(fs::symlink_metadata(&temporary).is_err());
    }

    #[test]
    fn setup_never_creates_through_or_over_something_else() {
        let fixture = Fixture::new();
        // Grok's skills directory links to a directory that is gone; Muse's is a file.
        symlink(
            fixture.root().join("gone"),
            fixture.home.join(".grok/skills"),
        )
        .unwrap();
        fs::write(fixture.home.join(".config/muse/skills"), "a file").unwrap();
        let skills = fixture.link(&fixture.pack());
        let blocked = |index: usize| {
            let target = &skills.targets[index];
            assert_eq!(target.state, SkillLinkState::BlockedByForeignPath);
            target.detail.clone().unwrap()
        };
        assert!(blocked(2).ends_with("links to a directory that does not exist"));
        assert!(blocked(3).ends_with("is not a directory"));
        assert!(!skills.setup_needed());
        assert!(!fixture.root().join("gone").exists());
        assert_eq!(
            fs::read_to_string(fixture.home.join(".config/muse/skills")).unwrap(),
            "a file"
        );
        // Linking where something already is fails instead of replacing it.
        let claude = fixture.path(".claude");
        let error = create_link(&claude, &fixture.root().join("elsewhere")).unwrap_err();
        assert!(error.contains("Cannot link"), "{error}");
        assert_eq!(
            fs::read_link(&claude).unwrap(),
            fixture.root().join("pack/cua-driver")
        );
    }

    #[test]
    fn a_skills_directory_that_is_the_packs_own_directory_counts_as_linked() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.home.join(".agents")).unwrap();
        symlink(
            fixture.root().join("pack"),
            fixture.home.join(".agents/skills"),
        )
        .unwrap();
        let skills = fixture.link(&fixture.pack());
        assert_eq!(skills.targets[1].state, SkillLinkState::Linked);
        assert!(
            fs::symlink_metadata(fixture.home.join(".agents/skills"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!read_manifest(&fixture.manifest).contains_key(&fixture.path(".agents")));
    }

    #[test]
    fn a_relative_home_variable_is_reported_and_never_linked() {
        let mut fixture = Fixture::new();
        fixture.homes.claude_config_dir = Some(PathBuf::from("riwork-relative-claude"));
        fixture.homes.xdg_config_home = Some(PathBuf::from("riwork-relative-config"));
        let skills = fixture.link(&fixture.pack());
        let unsupported: Vec<_> = skills
            .targets
            .iter()
            .filter(|target| target.state == SkillLinkState::Unsupported)
            .map(|target| (target.name.as_str(), target.detail.clone().unwrap()))
            .collect();
        assert_eq!(
            unsupported,
            [
                (
                    "Claude Code ($CLAUDE_CONFIG_DIR)",
                    "CLAUDE_CONFIG_DIR is a relative path, which each session resolves from its own directory".to_owned()
                ),
                (
                    "Muse ($XDG_CONFIG_HOME)",
                    "XDG_CONFIG_HOME is a relative path, which each session resolves from its own directory".to_owned()
                ),
            ]
        );
        assert!(
            skills.message.contains("relative path"),
            "{}",
            skills.message
        );
        assert!(!Path::new("riwork-relative-claude").exists());
        assert!(!Path::new("riwork-relative-config").exists());
    }

    #[test]
    fn a_swap_an_interrupted_setup_left_is_cleared_and_nothing_else() {
        let fixture = Fixture::new();
        fixture.link(&fixture.pack());
        let skills_directory = fixture.home.join(".grok/skills");
        let left = skills_directory.join(format!("{TEMPORARY_PREFIX}old"));
        symlink(fixture.root().join("pack/cua-driver"), &left).unwrap();
        let notes = skills_directory.join(".cua-driver.notes");
        fs::write(&notes, "keep").unwrap();
        fixture.link(&Pack::at(fixture.write_pack("moved/cua-driver")));
        assert!(fs::symlink_metadata(&left).is_err());
        assert_eq!(fs::read_to_string(&notes).unwrap(), "keep");
        let leftovers = tree(&fixture.home)
            .into_keys()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(TEMPORARY_PREFIX))
            })
            .count();
        assert_eq!(leftovers, 0);
    }
}
