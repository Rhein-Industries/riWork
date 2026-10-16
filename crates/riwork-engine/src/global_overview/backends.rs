//! The processes behind the work, for the dashboard's Backends section: the RiWork app, the
//! chat host, and each provider's CLI with the agents that run it.
//!
//! **Freshness.** A process is *stale* when it started before the on-disk executable it was
//! launched from was last written or replaced: it still runs the code it started with, and
//! only a restart picks up what is on disk now. "Replaced" is the latest modification or
//! status change (mtime, ctime) of the file and of each symbolic link on the way to it, so an
//! update that swaps a link (Claude's and Grok's installers, Homebrew) or writes a new file
//! with a backdated modification time (npm) counts too. A change of metadata alone
//! (permissions, extended attributes) counts as well, which errs towards "restart to apply".
//! When the start or the replacement is not known, freshness is unknown, never guessed
//! (`freshness`).
//!
//! What each row compares:
//! - The RiWork app: each running GUI of this home (its runtime registration), its process
//!   start against the executable the system says it runs.
//! - The chat host: the process `run/chat.lock` names, its start against its executable.
//! - A provider: the CLI a launch would run now (`sessions::installed_harness_program`)
//!   against each running agent of it. A harness shell's agent is its pane's process (the
//!   shell `exec`s into it), whose start the session sample's pane pid gives; without one, the
//!   shell's creation is a lower bound. A live chat's agent is started by the chat host, which
//!   records no time of it: the later of the chat's creation and the host's start is a lower
//!   bound. A lower bound can say an agent is current, never that it is stale.
//!
//! Reading starts nothing: no provider CLI, chat host, tmux, `ps` or network request. It reads
//! one lock file, the runtime registry's records, the system's entry for a few known pids
//! (`proc_pidinfo`), the metadata of a handful of files, and an npm package's `package.json`
//! for its version. It never connects to the chat host's socket, since a connection keeps an
//! idle host alive.

use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{
    chat::model::{ChatInfo, Provider},
    runtime::{self, RegisteredGui},
    sessions::{self, HarnessKind, ProgramLookup, ShellSession},
};

/// The providers the section lists, in its order.
pub const PROVIDERS: [HarnessKind; 4] = [
    HarnessKind::Codex,
    HarnessKind::Claude,
    HarnessKind::Muse,
    HarnessKind::Grok,
];

/// The host writes its pid into its lock as it starts: a process that started later than
/// this after the lock was written took the pid of a host that ended without emptying it.
const LOCK_SLACK_SECS: u64 = 2;
/// The most symbolic links followed from a CLI's path to its file.
const MAX_LINKS: usize = 16;
/// The largest `package.json` read for a version.
const MAX_PACKAGE_BYTES: u64 = 1 << 20;
/// A version longer than this is not taken from a `package.json`.
const MAX_VERSION_CHARS: usize = 64;

// ---- What is read --------------------------------------------------------------------------

/// A file a process runs, as it is on disk now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    /// The version its installed location names (`version_named`), when it names one.
    pub version: Option<String>,
    /// When it, or a link on the way to it, was last written or replaced (Unix seconds);
    /// `None` when that could not be read.
    pub replaced_at: Option<u64>,
}

/// A running process, as the system says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// When it started (Unix seconds), where the platform says.
    pub started_at: Option<u64>,
    /// The file it runs, as that is on disk now; `None` when the system does not say.
    pub binary: Option<Binary>,
}

/// A running RiWork GUI of this home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gui {
    pub process: Process,
    /// Its windows, as its registration lists them.
    pub windows: usize,
}

/// The chat host, as `run/chat.lock` and the system say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostSample {
    /// No host holds the lock: it is missing or empty, or names a process that is gone.
    Stopped,
    Running(Process),
    /// The lock could not be read; it says why.
    Unreadable(String),
}

/// A provider's CLI, as a launch would find it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliSample {
    Found(Binary),
    /// In none of the directories a launch searches.
    Missing,
    /// Not where this process looked, and the login shell's directories were not read.
    Unknown,
}

/// What `read_backends` read, for `Inputs::backends`; `build` arranges it with the shells
/// and the chats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendSample {
    /// The running GUIs whose state home is this one.
    pub guis: Vec<Gui>,
    pub chat_host: HostSample,
    /// Each provider's CLI, in `PROVIDERS` order.
    pub providers: Vec<(HarnessKind, CliSample)>,
    /// When the pane process of each live harness shell started (by shell id), where known.
    pub shell_starts: BTreeMap<String, u64>,
}

/// What the reader asks the system, so that tests answer for it with fixed processes.
pub(crate) trait System {
    /// The running GUIs in the runtime registry, any home.
    fn guis(&self) -> Vec<RegisteredGui>;
    /// When `pid` started and what it runs; `None` when it is not running.
    fn process(&self, pid: u32) -> Option<(Option<u64>, Option<PathBuf>)>;
    fn cli(&self, provider: HarnessKind) -> ProgramLookup;
}

/// The system itself.
struct Live {
    registry: Option<PathBuf>,
}

impl System for Live {
    fn guis(&self) -> Vec<RegisteredGui> {
        self.registry
            .as_deref()
            .map(runtime::registered_guis)
            .unwrap_or_default()
    }

    fn process(&self, pid: u32) -> Option<(Option<u64>, Option<PathBuf>)> {
        runtime::process_facts(pid)
    }

    fn cli(&self, provider: HarnessKind) -> ProgramLookup {
        sessions::installed_harness_program(provider)
    }
}

/// The app, the chat host and the providers' CLIs of `home`, read without starting anything.
/// `processes` is the session sample's pane process of each live shell
/// (`SessionSample::processes`); only those of live harness shells are looked up.
pub fn read_backends(
    home: &Path,
    shells: &[ShellSession],
    processes: &BTreeMap<String, u32>,
) -> BackendSample {
    let live = Live {
        registry: runtime::registry_directory(),
    };
    read_with(home, shells, processes, &live)
}

pub(crate) fn read_with(
    home: &Path,
    shells: &[ShellSession],
    processes: &BTreeMap<String, u32>,
    system: &impl System,
) -> BackendSample {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let guis = system
        .guis()
        .into_iter()
        .filter(|gui| gui.state_home == home)
        .map(|gui| Gui {
            process: Process {
                pid: gui.pid,
                started_at: gui.started_unix,
                binary: gui
                    .executable
                    .as_deref()
                    .map(|path| binary_at(path, "riwork")),
            },
            windows: gui.windows,
        })
        .collect();
    let providers = PROVIDERS
        .iter()
        .map(|provider| {
            let cli = match system.cli(*provider) {
                ProgramLookup::Found(path) => {
                    CliSample::Found(binary_at(&path, provider.program()))
                }
                ProgramLookup::Missing => CliSample::Missing,
                ProgramLookup::Unknown => CliSample::Unknown,
            };
            (*provider, cli)
        })
        .collect();
    let shell_starts = shells
        .iter()
        .filter(|shell| shell.alive && shell.harness.is_some())
        .filter_map(|shell| {
            let pid = processes.get(&shell.id)?;
            Some((shell.id.clone(), system.process(*pid)?.0?))
        })
        .collect();
    BackendSample {
        guis,
        chat_host: read_host(&home, system),
        providers,
        shell_starts,
    }
}

fn read_host(home: &Path, system: &impl System) -> HostSample {
    let lock = home.join("run").join("chat.lock");
    let mut text = String::new();
    match fs::File::open(&lock).and_then(|file| file.take(64).read_to_string(&mut text)) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return HostSample::Stopped,
        Err(error) => return HostSample::Unreadable(format!("run/chat.lock: {error}")),
    }
    let text = text.trim();
    // A host empties its lock when it stops.
    if text.is_empty() {
        return HostSample::Stopped;
    }
    let Ok(pid) = text.parse::<u32>() else {
        return HostSample::Unreadable(format!("run/chat.lock names no process ({text:?})"));
    };
    let Some((started_at, executable)) = system.process(pid) else {
        return HostSample::Stopped;
    };
    let written = fs::metadata(&lock).ok().and_then(|meta| modified(&meta));
    if let (Some(started), Some(written)) = (started_at, written)
        && started > written + LOCK_SLACK_SECS
    {
        return HostSample::Stopped;
    }
    HostSample::Running(Process {
        pid,
        started_at,
        binary: executable.as_deref().map(|path| binary_at(path, "riwork")),
    })
}

/// The file at `path` as it is on disk now. `program` is the name a versioned location
/// would carry (`version_named`).
fn binary_at(path: &Path, program: &str) -> Binary {
    Binary {
        version: version_named(path, program),
        replaced_at: replaced_at(path),
    }
}

fn modified(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// When the file's contents or its inode last changed, whichever is later.
#[cfg(unix)]
fn changed(meta: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    u64::try_from(meta.mtime().max(meta.ctime())).ok()
}
#[cfg(not(unix))]
fn changed(meta: &fs::Metadata) -> Option<u64> {
    modified(meta)
}

/// When the file `path` names, or a symbolic link on the way to it, was last written or
/// replaced (Unix seconds); `None` when the file cannot be read.
pub(crate) fn replaced_at(path: &Path) -> Option<u64> {
    use std::path::Component;

    let mut latest = changed(&fs::metadata(path).ok()?)?;
    let mut pending = std::path::absolute(path).ok()?;
    let mut resolved = PathBuf::new();
    let mut followed = 0;
    loop {
        let mut components = pending.components();
        let Some(component) = components.next() else {
            return Some(latest);
        };
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::Normal(name) => {
                resolved.push(name);
                let meta = fs::symlink_metadata(&resolved).ok()?;
                if meta.file_type().is_symlink() {
                    if followed == MAX_LINKS {
                        return None;
                    }
                    followed += 1;
                    latest = latest.max(changed(&meta)?);
                    let target = fs::read_link(&resolved).ok()?;
                    resolved.pop();
                    if target.is_absolute() {
                        resolved.clear();
                    }
                    // Resolve the target before the remaining components, including `..`.
                    pending = target.join(components.as_path());
                    continue;
                }
            }
        }
        pending = components.as_path().to_path_buf();
    }
}

/// The version the installed location of `program` at `path` names, following its links:
/// a file named for it (Claude's `versions/2.1.301`, Grok's `grok-1.0.45-macos-aarch64`),
/// the `package.json` of the npm package it is part of, or a directory named for it under
/// one named for the program (Homebrew's `Cellar/codex/0.46.0/…`). `None` when it names
/// none; nothing is run to ask.
pub(crate) fn version_named(path: &Path, program: &str) -> Option<String> {
    let canonical = path.canonicalize().ok()?;
    let name = canonical.file_name()?.to_str()?;
    let bare = name
        .strip_prefix(program)
        .and_then(|rest| rest.strip_prefix(['-', '_', '@']))
        .unwrap_or(name);
    if let Some(version) = leading_version(bare) {
        return Some(version);
    }
    if canonical
        .components()
        .any(|part| part.as_os_str() == "node_modules")
    {
        for directory in canonical.ancestors().skip(1).take(4) {
            if directory
                .file_name()
                .is_some_and(|name| name == "node_modules")
            {
                break;
            }
            if let Some(version) = package_version(&directory.join("package.json")) {
                return Some(version);
            }
        }
    }
    let parts: Vec<&str> = canonical.iter().filter_map(|part| part.to_str()).collect();
    parts
        .windows(2)
        .rev()
        .find(|pair| pair[0] == program && leading_version(pair[1]).is_some())
        .and_then(|pair| leading_version(pair[1]))
}

/// The version `text` starts with: numbers joined by dots, at least two, after an optional
/// `v`, and then nothing or a separator ("1.0.45-macos", "0.46.0_1"); nothing for any other.
fn leading_version(text: &str) -> Option<String> {
    let text = text.strip_prefix('v').unwrap_or(text);
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let version = text[..end].trim_end_matches('.');
    let rest = &text[version.len()..];
    let numbers: Vec<&str> = version.split('.').collect();
    if numbers.len() < 2 || numbers.iter().any(|number| number.is_empty()) {
        return None;
    }
    (rest.is_empty() || rest.starts_with(['-', '_', '+', '.'])).then(|| version.to_owned())
}

fn package_version(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_PACKAGE_BYTES).read_to_end(&mut bytes).ok()?;
    let package: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let version = package.get("version")?.as_str()?.trim();
    (!version.is_empty() && version.chars().count() <= MAX_VERSION_CHARS)
        .then(|| version.to_owned())
}

// ---- What the dashboard shows --------------------------------------------------------------

/// Whether a process runs what is on disk now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// It started when what it runs was already in place.
    Current,
    /// It started before what it runs was replaced: a restart picks up the new one.
    Stale,
    /// The start, or when the file was replaced, is not known.
    Unknown,
}

impl Freshness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }
}

/// When a process started, as far as the records tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    At(u64),
    /// Not before this: a lower bound, such as the creation of the session or chat it is in.
    NotBefore(u64),
    Unknown,
}

/// The freshness rule (see the module's notes): stale when the process started before its
/// executable was last replaced, current when it started at or after that, and unknown
/// when either time is not known. A lower bound proves current only.
pub fn freshness(start: Start, replaced_at: Option<u64>) -> Freshness {
    match (start, replaced_at) {
        (_, None) | (Start::Unknown, _) => Freshness::Unknown,
        (Start::At(started), Some(replaced)) if started < replaced => Freshness::Stale,
        (Start::At(_), Some(_)) => Freshness::Current,
        (Start::NotBefore(bound), Some(replaced)) if bound >= replaced => Freshness::Current,
        (Start::NotBefore(_), Some(_)) => Freshness::Unknown,
    }
}

/// The running RiWork GUIs of this home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppOverview {
    /// Normally one; none only for a reader outside the app (`riwork dashboard`).
    pub running: usize,
    pub windows: usize,
    /// The earliest start among them, when known.
    pub started_at: Option<u64>,
    /// When the executable of the earliest was last replaced: the build on disk.
    pub build_at: Option<u64>,
    /// Stale when any of them is, unknown when any cannot be told; `None` when none runs.
    pub freshness: Option<Freshness>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatHostOverview {
    Stopped,
    Running {
        pid: u32,
        started_at: Option<u64>,
        /// When its executable was last replaced.
        build_at: Option<u64>,
        freshness: Freshness,
    },
    Unreadable(String),
}

/// A provider's CLI on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliState {
    Found {
        version: Option<String>,
        replaced_at: Option<u64>,
    },
    Missing,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderOverview {
    pub provider: HarnessKind,
    pub cli: CliState,
    /// Live harness shells that run it.
    pub shells: usize,
    /// Live chats that run it; `None` while the chats, or whether the chat host runs, are
    /// not known.
    pub chats: Option<usize>,
    /// The earliest start of an agent of it, among the starts known exactly.
    pub oldest_start: Option<u64>,
    /// Running agents that started before the CLI was replaced.
    pub stale: usize,
    /// Running agents whose freshness cannot be told.
    pub unknown: usize,
}

impl ProviderOverview {
    /// Its running agents, when the chats are known.
    pub fn running(&self) -> Option<usize> {
        self.chats.map(|chats| self.shells + chats)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendsOverview {
    pub app: AppOverview,
    pub chat_host: ChatHostOverview,
    /// The providers whose CLI was found or that run, in `PROVIDERS` order.
    pub providers: Vec<ProviderOverview>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backends {
    NotRead,
    Read(BackendsOverview),
}

pub fn harness_name(provider: HarnessKind) -> &'static str {
    match provider {
        HarnessKind::Codex => "Codex",
        HarnessKind::Claude => "Claude",
        HarnessKind::Muse => "Muse",
        HarnessKind::Grok => "Grok",
    }
}

fn harness_of(provider: Provider) -> HarnessKind {
    match provider {
        Provider::Codex => HarnessKind::Codex,
        Provider::Claude => HarnessKind::Claude,
        Provider::Muse => HarnessKind::Muse,
    }
}

/// The section from what was read, the shells and the chats. It reads nothing.
pub(super) fn arrange(
    sample: &BackendSample,
    shells: &[ShellSession],
    chats: Option<&[ChatInfo]>,
) -> BackendsOverview {
    BackendsOverview {
        app: app(&sample.guis),
        chat_host: chat_host(&sample.chat_host),
        providers: providers(sample, shells, chats),
    }
}

fn worst(each: impl IntoIterator<Item = Freshness>) -> Option<Freshness> {
    each.into_iter().fold(None, |worst, freshness| {
        Some(match (worst, freshness) {
            (Some(Freshness::Stale), _) | (_, Freshness::Stale) => Freshness::Stale,
            (Some(Freshness::Unknown), _) | (_, Freshness::Unknown) => Freshness::Unknown,
            _ => Freshness::Current,
        })
    })
}

fn process_freshness(process: &Process) -> Freshness {
    freshness(
        process.started_at.map_or(Start::Unknown, Start::At),
        process
            .binary
            .as_ref()
            .and_then(|binary| binary.replaced_at),
    )
}

fn app(guis: &[Gui]) -> AppOverview {
    let oldest = guis
        .iter()
        .min_by_key(|gui| (gui.process.started_at.is_none(), gui.process.started_at));
    AppOverview {
        running: guis.len(),
        windows: guis.iter().map(|gui| gui.windows).sum(),
        started_at: oldest.and_then(|gui| gui.process.started_at),
        build_at: oldest
            .and_then(|gui| gui.process.binary.as_ref())
            .and_then(|binary| binary.replaced_at),
        freshness: worst(guis.iter().map(|gui| process_freshness(&gui.process))),
    }
}

fn chat_host(host: &HostSample) -> ChatHostOverview {
    match host {
        HostSample::Stopped => ChatHostOverview::Stopped,
        HostSample::Unreadable(error) => ChatHostOverview::Unreadable(error.clone()),
        HostSample::Running(process) => ChatHostOverview::Running {
            pid: process.pid,
            started_at: process.started_at,
            build_at: process
                .binary
                .as_ref()
                .and_then(|binary| binary.replaced_at),
            freshness: process_freshness(process),
        },
    }
}

fn providers(
    sample: &BackendSample,
    shells: &[ShellSession],
    chats: Option<&[ChatInfo]>,
) -> Vec<ProviderOverview> {
    // Chat agents run under the host: none without one, unknown when that is.
    let host_start = match &sample.chat_host {
        HostSample::Running(process) => Some(process.started_at),
        HostSample::Stopped => None,
        HostSample::Unreadable(_) => return unknown_chats(sample, shells),
    };
    sample
        .providers
        .iter()
        .filter_map(|(provider, cli)| {
            let replaced_at = match cli {
                CliSample::Found(binary) => binary.replaced_at,
                CliSample::Missing | CliSample::Unknown => None,
            };
            let mut starts: Vec<Start> = shells
                .iter()
                .filter(|shell| shell.alive && shell.harness == Some(*provider))
                .map(|shell| match sample.shell_starts.get(&shell.id) {
                    Some(started) => Start::At(*started),
                    None if shell.created_at_unix > 0 => Start::NotBefore(shell.created_at_unix),
                    None => Start::Unknown,
                })
                .collect();
            let shell_count = starts.len();
            let chat_count = match (host_start, chats) {
                (None, _) => Some(0),
                (Some(_), None) => None,
                (Some(host), Some(chats)) => {
                    let before = starts.len();
                    starts.extend(
                        chats
                            .iter()
                            .filter(|chat| harness_of(chat.provider) == *provider)
                            .filter(|chat| super::chat_is_live(&chat.state))
                            .map(|chat| match chat.created_at_unix.max(host.unwrap_or(0)) {
                                0 => Start::Unknown,
                                bound => Start::NotBefore(bound),
                            }),
                    );
                    Some(starts.len() - before)
                }
            };
            provider_row(
                *provider,
                cli,
                replaced_at,
                &starts,
                shell_count,
                chat_count,
            )
        })
        .collect()
}

/// The rows when whether the chat host runs is not known: the shells only.
fn unknown_chats(sample: &BackendSample, shells: &[ShellSession]) -> Vec<ProviderOverview> {
    let known = BackendSample {
        chat_host: HostSample::Stopped,
        ..sample.clone()
    };
    providers(&known, shells, None)
        .into_iter()
        .map(|row| ProviderOverview { chats: None, ..row })
        .collect()
}

fn provider_row(
    provider: HarnessKind,
    cli: &CliSample,
    replaced_at: Option<u64>,
    starts: &[Start],
    shells: usize,
    chats: Option<usize>,
) -> Option<ProviderOverview> {
    let found = matches!(cli, CliSample::Found(_));
    if !found && starts.is_empty() {
        return None;
    }
    let each: Vec<Freshness> = starts
        .iter()
        .map(|start| freshness(*start, replaced_at))
        .collect();
    Some(ProviderOverview {
        provider,
        cli: match cli {
            CliSample::Found(binary) => CliState::Found {
                version: binary.version.clone(),
                replaced_at: binary.replaced_at,
            },
            CliSample::Missing => CliState::Missing,
            CliSample::Unknown => CliState::Unknown,
        },
        shells,
        chats,
        oldest_start: starts
            .iter()
            .filter_map(|start| match start {
                Start::At(at) => Some(*at),
                _ => None,
            })
            .min(),
        stale: each
            .iter()
            .filter(|each| **each == Freshness::Stale)
            .count(),
        unknown: each
            .iter()
            .filter(|each| **each == Freshness::Unknown)
            .count(),
    })
}

// ---- Words ---------------------------------------------------------------------------------

/// How a row of the section reads at a glance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Runs what is on disk.
    Current,
    /// Something runs that started before what it runs was replaced.
    Stale,
    /// Something runs whose freshness cannot be told, or the row could not be read.
    Unknown,
    /// Nothing of it runs.
    Idle,
}

/// One row of the section, as the desktop and `riwork dashboard` say it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// "app", "chat-host", or the provider's program name.
    pub key: &'static str,
    pub title: String,
    pub detail: String,
    pub tone: Tone,
}

/// A local time as the section says it: "today 09:46", "yesterday 09:46", "Oct 14 09:46".
pub fn local_time(at: u64, now: u64) -> String {
    use chrono::{Local, TimeZone};
    let (Some(time), Some(today)) = (
        i64::try_from(at)
            .ok()
            .and_then(|at| Local.timestamp_opt(at, 0).single()),
        i64::try_from(now)
            .ok()
            .and_then(|now| Local.timestamp_opt(now, 0).single()),
    ) else {
        return "an unknown time".to_owned();
    };
    let clock = time.format("%H:%M");
    match (today.date_naive() - time.date_naive()).num_days() {
        0 => format!("today {clock}"),
        1 => format!("yesterday {clock}"),
        _ => format!("{} {clock}", time.format("%b %-d")),
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The section's rows: the app, the chat host, then each provider.
pub fn lines(backends: &BackendsOverview, now: u64) -> Vec<Line> {
    let mut lines = vec![
        app_line(&backends.app, now),
        host_line(&backends.chat_host, now),
    ];
    lines.extend(
        backends
            .providers
            .iter()
            .map(|provider| provider_line(provider, now)),
    );
    lines
}

fn build_words(freshness: Freshness, build_at: Option<u64>, now: u64) -> Option<String> {
    let build = build_at.map(|at| local_time(at, now));
    match (freshness, build) {
        (Freshness::Stale, Some(build)) => Some(format!("restart to apply the {build} build")),
        (Freshness::Stale, None) => Some("restart to apply".to_owned()),
        (Freshness::Current, Some(build)) => Some(format!("runs the {build} build")),
        (Freshness::Current, None) => None,
        (Freshness::Unknown, Some(build)) => Some(format!("build on disk {build}, start unknown")),
        (Freshness::Unknown, None) => Some("build on disk unknown".to_owned()),
    }
}

fn app_line(app: &AppOverview, now: u64) -> Line {
    let mut parts = Vec::new();
    let since = app.started_at.map_or_else(
        || "since an unknown time".to_owned(),
        |at| format!("since {}", local_time(at, now)),
    );
    match app.running {
        0 => parts.push("not running".to_owned()),
        1 => parts.push(format!("running {since}")),
        count => parts.push(format!("{count} running, the oldest {since}")),
    }
    if app.running > 0 {
        parts.push(plural(app.windows, "window", "windows"));
    }
    parts.extend(
        app.freshness
            .and_then(|freshness| build_words(freshness, app.build_at, now)),
    );
    Line {
        key: "app",
        title: "RiWork".to_owned(),
        detail: parts.join(" · "),
        tone: match app.freshness {
            None => Tone::Idle,
            Some(Freshness::Current) => Tone::Current,
            Some(Freshness::Stale) => Tone::Stale,
            Some(Freshness::Unknown) => Tone::Unknown,
        },
    }
}

fn host_line(host: &ChatHostOverview, now: u64) -> Line {
    let (detail, tone) = match host {
        ChatHostOverview::Stopped => ("not running".to_owned(), Tone::Idle),
        ChatHostOverview::Unreadable(error) => (format!("unknown: {error}"), Tone::Unknown),
        ChatHostOverview::Running {
            pid,
            started_at,
            build_at,
            freshness,
        } => {
            let mut parts = vec![match started_at {
                Some(at) => format!("running since {}", local_time(*at, now)),
                None => "running since an unknown time".to_owned(),
            }];
            parts.push(format!("pid {pid}"));
            parts.extend(build_words(*freshness, *build_at, now));
            (
                parts.join(" · "),
                match freshness {
                    Freshness::Current => Tone::Current,
                    Freshness::Stale => Tone::Stale,
                    Freshness::Unknown => Tone::Unknown,
                },
            )
        }
    };
    Line {
        key: "chat-host",
        title: "Chat host".to_owned(),
        detail,
        tone,
    }
}

fn provider_line(provider: &ProviderOverview, now: u64) -> Line {
    let mut parts = vec![match &provider.cli {
        CliState::Found {
            version: Some(version),
            ..
        } => format!("{version} on disk"),
        CliState::Found {
            version: None,
            replaced_at: Some(at),
        } => format!("version unknown, installed {}", local_time(*at, now)),
        CliState::Found { .. } => "version unknown".to_owned(),
        CliState::Missing => "not found on disk".to_owned(),
        CliState::Unknown => "not found where looked".to_owned(),
    }];
    match provider.running() {
        Some(0) => parts.push("none running".to_owned()),
        Some(running) => parts.push(match provider.oldest_start {
            Some(at) => format!("{running} running, oldest {}", local_time(at, now)),
            None => format!("{running} running"),
        }),
        None => parts.push(format!("{} in terminals, chats unknown", provider.shells)),
    }
    if provider.stale > 0 {
        parts.push(format!(
            "{} the installed binary",
            match provider.stale {
                1 => "1 predates".to_owned(),
                count => format!("{count} predate"),
            }
        ));
    }
    if provider.unknown > 0 {
        parts.push(format!("{} unknown", provider.unknown));
    }
    let running = provider.running().unwrap_or(provider.shells);
    Line {
        key: provider.provider.program(),
        title: harness_name(provider.provider).to_owned(),
        detail: parts.join(" · "),
        tone: if provider.stale > 0 {
            Tone::Stale
        } else if provider.unknown > 0 || provider.chats.is_none() {
            Tone::Unknown
        } else if running > 0 {
            Tone::Current
        } else {
            Tone::Idle
        },
    }
}

#[cfg(test)]
mod tests;
