//! Folder trust for the agent sessions RiWork starts. Creating a session in a
//! folder is the user's decision to run that agent there, so the vendor's own
//! "trust this folder?" gate is settled for exactly that folder, through the
//! harness's own mechanism, before the agent starts. Approval and sandbox
//! choices are untouched: trust only lets the agent load the folder's own
//! configuration (rules, hooks, MCP servers) without asking.
//!
//! - Codex: a per-run `-c` (`codex_arguments`); nothing is saved.
//! - Muse: `--trust-workspace` for the run (`muse_may_trust`); nothing is saved.
//! - Claude Code: the entry its own dialog saves (`trust_for_claude`); Claude has
//!   no per-run switch.
//! - Grok: the entry its own prompt saves (`trust_for_grok`); Grok's per-run
//!   switches trust more than one folder.
//!
//! Where the harness would take the trust for more than the folder (a folder
//! inside a repository, which Muse and Grok trust as the whole repository),
//! nothing is done and the harness asks, as it did before. A failure to save
//! trust is reported and the launch goes on.

use std::{
    env,
    ffi::OsStr,
    fs,
    fs::OpenOptions,
    io::Write,
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

/// Larger than any harness configuration seen; a file past it is left alone.
const MAX_CONFIG_BYTES: u64 = 64 * 1024 * 1024;
/// How long to wait for a harness that is saving its own configuration.
const LOCK_WAIT: Duration = Duration::from_secs(1);

/// Whether RiWork may trust `cwd` on the user's behalf: an absolute folder
/// that is neither the file system root nor the home directory, which every
/// harness would read as trust in far more than one project.
fn trustable(cwd: &Path) -> bool {
    let home = env::var_os("HOME").map(PathBuf::from);
    cwd.is_absolute()
        && cwd.parent().is_some()
        && home.as_deref().is_none_or(|home| {
            cwd != home && fs::canonicalize(home).map_or(true, |home| cwd != home)
        })
}

/// The nearest folder at or above `cwd` with an entry named one of `markers`.
fn marked_root<'a>(cwd: &'a Path, markers: &[&str]) -> Option<&'a Path> {
    cwd.ancestors().find(|folder| {
        markers
            .iter()
            .any(|marker| fs::symlink_metadata(folder.join(marker)).is_ok())
    })
}

/// `text` as a TOML basic string: JSON's escapes, and DEL, which TOML wants
/// escaped too.
fn toml_basic_string(text: &str) -> String {
    serde_json::to_string(text)
        .expect("serializing a string cannot fail")
        .replace('\u{7f}', "\\u007f")
}

/// The `-c` that makes Codex trust exactly `cwd` for this run, saving nothing.
/// `cwd` must be canonical: Codex looks up its physical working directory byte
/// for byte, before the repository's root. A dotted key cannot name the folder,
/// since Codex splits `-c` keys at every `.` and keeps quotes in them; an inline
/// table can, and Codex merges it into the saved `projects` table. A second
/// `projects` override in the same run would replace this one.
///
/// Nothing for a folder that is no project to Codex (no `.git` at or above it,
/// no `.codex` in it): Codex asks nothing there, and trust would trade its
/// granular approvals for on-request ones.
pub(crate) fn codex_arguments(cwd: &Path) -> Vec<String> {
    let Some(folder) = cwd.to_str().filter(|_| trustable(cwd)) else {
        return Vec::new();
    };
    if marked_root(cwd, &[".git"]).is_none() && !cwd.join(".codex").is_dir() {
        return Vec::new();
    }
    vec![
        "-c".to_owned(),
        format!(
            "projects={{{}={{trust_level=\"trusted\"}}}}",
            toml_basic_string(folder)
        ),
    ]
}

/// Whether Muse may be told to trust its workspace for a session started in
/// `cwd` (`--trust-workspace`, for one run, saving nothing): only when that
/// workspace is `cwd` itself. Muse's workspace is the nearest folder at or
/// above `cwd` with a `.git`, `.hg` or `.sl` entry, so below such a root the
/// switch would trust the whole repository, and Muse is left to ask.
pub(crate) fn muse_may_trust(cwd: &Path) -> bool {
    trustable(cwd) && marked_root(cwd, &[".git", ".hg", ".sl"]).is_none_or(|root| root == cwd)
}

/// Trust `cwd` in the configuration of the Claude Code terminal about to start
/// there (`claude_config_file`), keyed as Claude keys it: the physical path in
/// NFC. This is the entry Claude's own dialog saves, for this folder alone.
pub(crate) fn trust_for_claude(cwd: &Path, config_dir: Option<&OsStr>) {
    let Some(folder) = cwd.to_str().filter(|_| trustable(cwd)) else {
        return;
    };
    // Claude signed in to another server keeps its configuration in a file of its own.
    if env::var_os("CLAUDE_CODE_CUSTOM_OAUTH_URL").is_some() {
        return;
    }
    let Some(file) = claude_config_file(config_dir, env::var_os("HOME").as_deref()) else {
        return;
    };
    if !may_write(&file) {
        return;
    }
    let folder: String = folder.nfc().collect();
    if let Err(error) = trust_claude_folder(&file, &folder, LOCK_WAIT) {
        eprintln!(
            "riwork: could not pre-trust {folder} for Claude Code in {}: {error}",
            file.display()
        );
    }
}

/// The configuration Claude Code reads: `.claude.json` in `CLAUDE_CONFIG_DIR`,
/// or else in the home directory, unless an older `.config.json` with content
/// is in Claude's directory (`CLAUDE_CONFIG_DIR`, or else `~/.claude`), which
/// Claude still prefers.
fn claude_config_file(config_dir: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let config_dir = config_dir
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from);
    let home = home.filter(|home| !home.is_empty()).map(PathBuf::from);
    let legacy = config_dir
        .clone()
        .or_else(|| home.as_ref().map(|home| home.join(".claude")))?
        .join(".config.json");
    if fs::metadata(&legacy).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0) {
        return Some(legacy);
    }
    Some(config_dir.or(home)?.join(".claude.json"))
}

/// Whether trust may be saved in `file`. Tests start agents in temporary
/// folders, and must never save trust in the user's real configuration: in a
/// test build, only a file under the temporary directory may be written.
fn may_write(file: &Path) -> bool {
    if cfg!(test) {
        let temporary = env::temp_dir();
        return file.starts_with(&temporary)
            || fs::canonicalize(&temporary).is_ok_and(|temporary| file.starts_with(temporary));
    }
    true
}

/// Mark `cwd` as trusted in the Claude configuration `file`. `Ok(false)` when
/// there was nothing to do: the folder is trusted already, or there is no
/// configuration yet (Claude's first run writes its own, and a missing file
/// must stay missing so that Claude still offers to restore its backup).
fn trust_claude_folder(file: &Path, cwd: &str, wait: Duration) -> Result<bool, String> {
    // A link to a file elsewhere (a dotfiles repository) is written through,
    // as Claude writes it, and stays a link.
    let target = match fs::canonicalize(file) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("resolve {}: {error}", file.display())),
    };
    // Most launches find the folder trusted and need no lock.
    if with_claude_trust(&read_config(&target)?, cwd)?.is_none() {
        return Ok(false);
    }
    let _lock = ClaudeLock::acquire(&sibling(file, ".lock"), wait)?;
    let document = read_config(&target)?;
    let Some(updated) = with_claude_trust(&document, cwd)? else {
        return Ok(false);
    };
    let permissions = fs::metadata(&target)
        .map_err(|error| error.to_string())?
        .permissions();
    replace_file(&target, &updated, Some(permissions))?;
    Ok(true)
}

/// Trust `cwd` in Grok's trust store, `trusted_folders.toml` in `GROK_HOME` or
/// else `~/.grok`, for the Grok terminal about to start there. Grok keys trust
/// by the main checkout of the repository a folder is in, so only such a root,
/// or a folder in no repository, can be trusted alone (`grok_may_trust`).
pub(crate) fn trust_for_grok(cwd: &Path, grok_home: Option<&OsStr>) {
    let Some(folder) = cwd.to_str().filter(|_| grok_may_trust(cwd)) else {
        return;
    };
    let home = match grok_home.filter(|home| !home.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => match env::var_os("HOME") {
            Some(home) => Path::new(&home).join(".grok"),
            None => return,
        },
    };
    // Grok that has never run saves its own store.
    if !home.is_dir() {
        return;
    }
    let file = home.join("trusted_folders.toml");
    if !may_write(&file) {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if let Err(error) = trust_grok_folder(&file, folder, now, LOCK_WAIT) {
        eprintln!(
            "riwork: could not pre-trust {folder} for Grok in {}: {error}",
            file.display()
        );
    }
}

/// Whether Grok's trust in `cwd` would be trust in `cwd` alone. Grok keys trust
/// by the main checkout of the nearest repository at or above a folder, which
/// for a folder inside a repository, a linked worktree or a checkout whose Git
/// directory is elsewhere (a `.git` file) is another folder: one that covers
/// the whole repository and every worktree of it.
fn grok_may_trust(cwd: &Path) -> bool {
    trustable(cwd)
        && marked_root(cwd, &[".git"]).is_none_or(|root| root == cwd && cwd.join(".git").is_dir())
}

/// Add `cwd` to Grok's trust store `file`, under Grok's lock on it. `Ok(false)`
/// when the store has an entry for the folder already: it is trusted, or the
/// user wrote what Grok should do there.
fn trust_grok_folder(file: &Path, cwd: &str, now: u64, wait: Duration) -> Result<bool, String> {
    let target = match fs::canonicalize(file) {
        Ok(target) => Some(target),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("resolve {}: {error}", file.display())),
    };
    let read = |target: &Option<PathBuf>| match target {
        Some(target) => read_config(target),
        None => Ok(String::new()),
    };
    if with_grok_trust(&read(&target)?, cwd, now)?.is_none() {
        return Ok(false);
    }
    let _lock = lock_file(&sibling(file, ".lock"), wait)?;
    let target = match fs::canonicalize(file) {
        Ok(target) => Some(target),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("resolve {}: {error}", file.display())),
    };
    let Some(updated) = with_grok_trust(&read(&target)?, cwd, now)? else {
        return Ok(false);
    };
    let (target, permissions) = match target {
        Some(target) => {
            let permissions = fs::metadata(&target)
                .map_err(|error| error.to_string())?
                .permissions();
            (target, Some(permissions))
        }
        None => (file.to_path_buf(), None),
    };
    replace_file(&target, &updated, permissions)?;
    Ok(true)
}

/// Grok's trust store `document` with a table for `cwd` appended, as Grok
/// writes one, or `None` when it has one already. The rest of the text is kept
/// as it is; the result is read back to make sure it says what it should.
fn with_grok_trust(document: &str, cwd: &str, now: u64) -> Result<Option<String>, String> {
    let before: toml_edit::DocumentMut = document
        .parse()
        .map_err(|error| format!("not TOML: {error}"))?;
    match before.get("folders") {
        None => {}
        Some(toml_edit::Item::Table(folders)) if folders.contains_key(cwd) => return Ok(None),
        Some(toml_edit::Item::Table(_)) => {}
        Some(_) => return Err("folders is not a table".to_owned()),
    }
    let separator = match document {
        "" => "",
        text if text.ends_with("\n\n") => "",
        text if text.ends_with('\n') => "\n",
        _ => "\n\n",
    };
    let updated = format!(
        "{document}{separator}[folders.{}]\ntrusted = true\ndecided_at = {now}\n",
        toml_basic_string(cwd)
    );
    let after: toml_edit::DocumentMut = updated
        .parse()
        .map_err(|error| format!("the merged text is not TOML: {error}"))?;
    let trusted = after
        .get("folders")
        .and_then(|folders| folders.get(cwd))
        .and_then(|folder| folder.get("trusted"))
        .and_then(toml_edit::Item::as_bool);
    if trusted != Some(true) {
        return Err("the merged text does not trust the folder".to_owned());
    }
    Ok(Some(updated))
}

fn read_config(file: &Path) -> Result<String, String> {
    let metadata = fs::metadata(file).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err(format!("{} is not a file", file.display()));
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(format!("{} is too large", file.display()));
    }
    fs::read_to_string(file).map_err(|error| error.to_string())
}

/// `file` with `suffix` appended to its name.
fn sibling(file: &Path, suffix: &str) -> PathBuf {
    let mut path = file.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

/// Claude's own lock on its configuration: the directory `<file>.lock` beside
/// the file as Claude names it (proper-lockfile), not beside a link's target.
/// Claude takes it for every save and re-reads the file under it, so an entry
/// written under it is not lost to a running Claude. Held for one read and one
/// rename, far below the 10 seconds after which Claude takes a lock for
/// abandoned. Claude that cannot get it within `wait` is let be: nothing is
/// written without it.
struct ClaudeLock(PathBuf);

impl ClaudeLock {
    fn acquire(path: &Path, wait: Duration) -> Result<Self, String> {
        let started = Instant::now();
        loop {
            match fs::create_dir(path) {
                Ok(()) => return Ok(Self(path.to_path_buf())),
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        && started.elapsed() < wait =>
                {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err("Claude Code kept its configuration locked".to_owned());
                }
                Err(error) => return Err(format!("lock {}: {error}", path.display())),
            }
        }
    }
}

impl Drop for ClaudeLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// Grok's own lock on its trust store: an exclusive lock on `<file>.lock`,
/// which Grok takes to save and under which it re-reads the store. Released
/// when the returned file is closed.
fn lock_file(path: &Path, wait: Duration) -> Result<fs::File, String> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|error| format!("lock {}: {error}", path.display()))?;
    let started = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(_) if started.elapsed() < wait => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return Err("Grok kept its trust store locked".to_owned()),
        }
    }
}

/// Replace `file` with `content` in one rename, with `permissions`, or else
/// private to the user.
fn replace_file(
    file: &Path,
    content: &str,
    permissions: Option<fs::Permissions>,
) -> Result<(), String> {
    let directory = file.parent().ok_or("the file has no directory")?;
    let name = file
        .file_name()
        .ok_or("the file has no name")?
        .to_string_lossy();
    let temporary: PathBuf = directory.join(format!(".{name}.riwork-{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&temporary)
        .and_then(|mut handle| {
            if let Some(permissions) = permissions {
                handle.set_permissions(permissions)?;
            }
            handle.write_all(content.as_bytes())?;
            handle.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, file));
    written.map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("write {}: {error}", file.display())
    })
}

/// The JSON text of a Claude configuration with `projects[cwd]` marked as
/// trusted, or `None` when it already is. Everything else in `document`, bytes
/// and order included, is kept: the change is spliced into the text, then
/// checked against the parsed document so that nothing else differs.
fn with_claude_trust(document: &str, cwd: &str) -> Result<Option<String>, String> {
    let before: serde_json::Value =
        serde_json::from_str(document).map_err(|error| format!("not JSON: {error}"))?;
    if !before.is_object() {
        return Err("not a JSON object".to_owned());
    }
    let key = serde_json::to_string(cwd).expect("serializing a string cannot fail");
    let root = skip_space(document.as_bytes(), 0);
    let updated = match last_member(document, root, "projects")? {
        None => insert_member(
            document,
            root,
            &format!("\"projects\": {{{key}: {{\"hasTrustDialogAccepted\": true}}}}"),
        )?,
        Some(projects) => match last_member(document, projects.start, cwd)? {
            None => insert_member(
                document,
                projects.start,
                &format!("{key}: {{\"hasTrustDialogAccepted\": true}}"),
            )?,
            Some(project) => {
                match last_member(document, project.start, "hasTrustDialogAccepted")? {
                    Some(flag) if &document[flag.clone()] == "true" => return Ok(None),
                    Some(flag) => splice(document, flag, "true"),
                    None => {
                        insert_member(document, project.start, "\"hasTrustDialogAccepted\": true")?
                    }
                }
            }
        },
    };
    let mut expected = before;
    let projects = expected
        .as_object_mut()
        .expect("checked above")
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}));
    let project = projects
        .as_object_mut()
        .expect("spliced into an object")
        .entry(cwd)
        .or_insert_with(|| serde_json::json!({}));
    project
        .as_object_mut()
        .expect("spliced into an object")
        .insert("hasTrustDialogAccepted".to_owned(), true.into());
    let after: serde_json::Value = serde_json::from_str(&updated)
        .map_err(|error| format!("the merged text is not JSON: {error}"))?;
    if after != expected {
        return Err("the merged text changes more than the folder's trust".to_owned());
    }
    Ok(Some(updated))
}

/// `text` with `range` replaced by `with`.
fn splice(text: &str, range: Range<usize>, with: &str) -> String {
    let mut result = String::with_capacity(text.len() + with.len());
    result.push_str(&text[..range.start]);
    result.push_str(with);
    result.push_str(&text[range.end..]);
    result
}

/// `text` with `member` (`"key": value`) added to the end of the object that
/// opens at `open`, on a line of its own indented as the last member is.
fn insert_member(text: &str, open: usize, member: &str) -> Result<String, String> {
    let (members, _) = members(text, open)?;
    Ok(match members.last() {
        Some(last) => {
            let indent = &text[text[..last.key].trim_end_matches(is_space).len()..last.key];
            let at = last.value.end;
            splice(text, at..at, &format!(",{indent}{member}"))
        }
        None => splice(text, open + 1..open + 1, member),
    })
}

/// A member of a JSON object: where its key starts and the span of its value.
struct Member {
    name: String,
    key: usize,
    value: Range<usize>,
}

/// The value of the last member named `name` in the object that opens at
/// `open`, which is what a JSON reader keeps when a name repeats. A value that
/// is not an object is an error: there is nothing to merge into.
fn last_member(text: &str, open: usize, name: &str) -> Result<Option<Range<usize>>, String> {
    let (members, _) = members(text, open)?;
    match members.into_iter().rev().find(|member| member.name == name) {
        Some(member) if name == "hasTrustDialogAccepted" => Ok(Some(member.value)),
        Some(member) if text.as_bytes()[member.value.start] == b'{' => Ok(Some(member.value)),
        Some(_) => Err(format!("{name} is not an object")),
        None => Ok(None),
    }
}

/// The members of the object that opens at `open`, and where it closes.
fn members(text: &str, open: usize) -> Result<(Vec<Member>, usize), String> {
    let bytes = text.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return Err("expected an object".to_owned());
    }
    let mut at = skip_space(bytes, open + 1);
    let mut members = Vec::new();
    if bytes.get(at) == Some(&b'}') {
        return Ok((members, at));
    }
    loop {
        let key = at;
        let key_end = string_end(bytes, key)?;
        let name = serde_json::from_str(&text[key..key_end])
            .map_err(|error| format!("unreadable key: {error}"))?;
        at = skip_space(bytes, key_end);
        if bytes.get(at) != Some(&b':') {
            return Err("expected ':'".to_owned());
        }
        at = skip_space(bytes, at + 1);
        let value = at..value_end(bytes, at)?;
        at = skip_space(bytes, value.end);
        members.push(Member { name, key, value });
        match bytes.get(at) {
            Some(b',') => at = skip_space(bytes, at + 1),
            Some(b'}') => return Ok((members, at)),
            _ => return Err("expected ',' or '}'".to_owned()),
        }
    }
}

fn is_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r')
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while matches!(bytes.get(at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        at += 1;
    }
    at
}

/// The end of the string whose opening quote is at `at`.
fn string_end(bytes: &[u8], at: usize) -> Result<usize, String> {
    if bytes.get(at) != Some(&b'"') {
        return Err("expected a string".to_owned());
    }
    let mut at = at + 1;
    loop {
        match bytes.get(at) {
            Some(b'\\') => at += 2,
            Some(b'"') => return Ok(at + 1),
            Some(_) => at += 1,
            None => return Err("unterminated string".to_owned()),
        }
    }
}

/// The end of the value that starts at `at`.
fn value_end(bytes: &[u8], at: usize) -> Result<usize, String> {
    match bytes.get(at) {
        Some(b'"') => string_end(bytes, at),
        Some(b'{' | b'[') => {
            let mut depth = 0usize;
            let mut at = at;
            loop {
                match bytes.get(at) {
                    Some(b'"') => {
                        at = string_end(bytes, at)?;
                        continue;
                    }
                    Some(b'{' | b'[') => depth += 1,
                    Some(b'}' | b']') => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(at + 1);
                        }
                    }
                    Some(_) => {}
                    None => return Err("unterminated value".to_owned()),
                }
                at += 1;
            }
        }
        Some(_) => {
            let mut at = at;
            while !matches!(
                bytes.get(at),
                None | Some(b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            ) {
                at += 1;
            }
            Ok(at)
        }
        None => Err("expected a value".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own under the system's temporary directory, removed on drop.
    struct Temporary(PathBuf);

    impl Temporary {
        fn new() -> Self {
            let path = env::temp_dir().join(format!("riwork-folder-trust-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        /// A new folder `name` in this one, with an entry named `marker` in it.
        fn folder(&self, name: &str, marker: Option<&str>) -> PathBuf {
            let folder = self.0.join(name);
            fs::create_dir_all(&folder).unwrap();
            match marker {
                Some(".git file") => {
                    fs::write(folder.join(".git"), "gitdir: /elsewhere\n").unwrap()
                }
                Some(marker) => fs::create_dir(folder.join(marker)).unwrap(),
                None => {}
            }
            folder
        }
    }

    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const ODD: &str = "It's a \"folder\" with spaces, a.dot, a=b, back\\slash, Zürich ✓";

    fn trusted(document: &str, cwd: &str) -> bool {
        let value: serde_json::Value = serde_json::from_str(document).unwrap();
        value["projects"][cwd]["hasTrustDialogAccepted"] == serde_json::json!(true)
    }

    fn codex_key(arguments: &[String]) -> String {
        assert_eq!(arguments.len(), 2, "{arguments:?}");
        assert_eq!(arguments[0], "-c");
        arguments[1]
            .strip_prefix("projects={")
            .and_then(|rest| rest.strip_suffix("={trust_level=\"trusted\"}}"))
            .unwrap_or_else(|| panic!("{}", arguments[1]))
            .to_owned()
    }

    #[test]
    fn codex_trusts_exactly_the_folder_through_an_inline_table_with_escaped_keys() {
        let temporary = Temporary::new();
        for name in [ODD, "line\nbreak\ttab", "delete\u{7f}me", "my.project"] {
            let folder = temporary.folder(name, Some(".git"));
            let key = codex_key(&codex_arguments(&folder));
            // A dotted key would split at a dot and keep the quotes; this is one
            // TOML basic string, which TOML reads back as the folder.
            assert!(key.starts_with('"') && key.ends_with('"'), "{key}");
            assert!(!key.contains('\u{7f}') && !key.contains('\n'), "{key}");
            let parsed: toml_edit::DocumentMut = format!("key = {key}").parse().unwrap();
            assert_eq!(parsed["key"].as_str(), folder.to_str(), "{name:?}");
        }
        assert_eq!(
            codex_arguments(&temporary.folder("plain", Some(".git"))),
            [
                "-c".to_owned(),
                format!(
                    "projects={{\"{}\"={{trust_level=\"trusted\"}}}}",
                    temporary.0.join("plain").display()
                )
            ]
        );
    }

    #[test]
    fn codex_trusts_a_project_folder_but_leaves_a_projectless_one_as_it_is() {
        let temporary = Temporary::new();
        let repository = temporary.folder("repository", Some(".git"));
        let inner = repository.join("inner");
        fs::create_dir(&inner).unwrap();
        let worktree = temporary.folder("worktree", Some(".git file"));
        let configured = temporary.folder("configured", Some(".codex"));
        let plain = temporary.folder("plain", None);
        // Codex looks the exact folder up before the repository's root, so a
        // folder inside a repository is trusted alone.
        for folder in [&repository, &inner, &worktree, &configured] {
            assert_eq!(
                codex_key(&codex_arguments(folder)),
                toml_basic_string(folder.to_str().unwrap())
            );
        }
        // No project to Codex: no question asked, and its granular approvals stay.
        assert!(codex_arguments(&plain).is_empty());
    }

    #[test]
    fn no_harness_is_told_to_trust_the_root_the_home_directory_or_a_relative_folder() {
        let home = PathBuf::from(env::var_os("HOME").unwrap());
        for folder in [Path::new("/"), home.as_path(), Path::new("relative/folder")] {
            assert!(!trustable(folder), "{folder:?}");
            assert!(codex_arguments(folder).is_empty(), "{folder:?}");
            assert!(!muse_may_trust(folder), "{folder:?}");
            assert!(!grok_may_trust(folder), "{folder:?}");
        }
        assert!(trustable(&home.join("project")));
    }

    #[test]
    fn muse_trusts_its_workspace_only_when_that_is_the_folder_itself() {
        let temporary = Temporary::new();
        let plain = temporary.folder("plain folder", None);
        for marker in [".git", ".git file", ".hg", ".sl"] {
            let root = temporary.folder(&format!("root {marker}"), Some(marker));
            let inner = root.join("inner");
            fs::create_dir(&inner).unwrap();
            assert!(muse_may_trust(&root), "{marker}");
            // Muse's workspace there would be the whole repository.
            assert!(!muse_may_trust(&inner), "{marker}");
        }
        assert!(muse_may_trust(&plain));
        // Markers Muse does not know make no workspace.
        assert!(muse_may_trust(&temporary.folder("jj", Some(".jj"))));
    }

    #[test]
    fn grok_trusts_a_main_checkout_or_a_plain_folder_but_not_what_it_keys_elsewhere() {
        let temporary = Temporary::new();
        let main = temporary.folder("main", Some(".git"));
        let inner = main.join("inner");
        fs::create_dir(&inner).unwrap();
        let worktree = temporary.folder("worktree", Some(".git file"));
        assert!(grok_may_trust(&main));
        assert!(grok_may_trust(&temporary.folder("plain", None)));
        // Grok keys these by the main checkout, which covers the whole repository.
        assert!(!grok_may_trust(&inner));
        assert!(!grok_may_trust(&worktree));
        assert!(!grok_may_trust(&worktree.join("inner")));
    }

    #[test]
    fn claude_trust_is_spliced_into_the_projects_and_every_other_byte_is_kept() {
        let document = "{\n  \"numStartups\": 12,\n  \"tipsHistory\": {\"a\": 1.50, \"b\": \"}{\\\"\"},\n  \"projects\": {\n    \"/Users/test/other\": {\n      \"allowedTools\": [],\n      \"hasTrustDialogAccepted\": true\n    }\n  },\n  \"oauthAccount\": {\"x\": \"\\u00e9\"}\n}\n";
        let updated = with_claude_trust(document, ODD).unwrap().unwrap();
        let entry = format!(
            ",\n    {}: {{\"hasTrustDialogAccepted\": true}}",
            serde_json::to_string(ODD).unwrap()
        );
        let at = document.find("\n  },\n  \"oauthAccount\"").unwrap();
        assert_eq!(
            updated,
            format!("{}{entry}{}", &document[..at], &document[at..])
        );
        assert!(trusted(&updated, ODD));
        assert!(trusted(&updated, "/Users/test/other"));
        // Already trusted: nothing to write.
        assert_eq!(with_claude_trust(&updated, ODD).unwrap(), None);
        assert_eq!(
            with_claude_trust(&updated, "/Users/test/other").unwrap(),
            None
        );
    }

    #[test]
    fn claude_trust_is_added_to_an_existing_entry_or_a_new_projects_table() {
        // An entry Claude wrote for the folder keeps its fields; one not accepted is.
        for (document, expected) in [
            (
                r#"{"projects":{"/p":{"allowedTools":["x"]}}}"#,
                r#"{"projects":{"/p":{"allowedTools":["x"],"hasTrustDialogAccepted": true}}}"#,
            ),
            (
                r#"{"projects":{"/p":{"hasTrustDialogAccepted":false,"n":1}}}"#,
                r#"{"projects":{"/p":{"hasTrustDialogAccepted":true,"n":1}}}"#,
            ),
            (
                r#"{"projects":{"/p":{}}}"#,
                r#"{"projects":{"/p":{"hasTrustDialogAccepted": true}}}"#,
            ),
            (
                r#"{"projects":{}}"#,
                r#"{"projects":{"/p": {"hasTrustDialogAccepted": true}}}"#,
            ),
            (
                "{}",
                r#"{"projects": {"/p": {"hasTrustDialogAccepted": true}}}"#,
            ),
            (
                "{\n  \"numStartups\": 1\n}",
                "{\n  \"numStartups\": 1,\n  \"projects\": {\"/p\": {\"hasTrustDialogAccepted\": true}}\n}",
            ),
            // A repeated name: a JSON reader keeps the last, so that is the one changed.
            (
                r#"{"projects":{"/p":{"hasTrustDialogAccepted":false}},"projects":{}}"#,
                r#"{"projects":{"/p":{"hasTrustDialogAccepted":false}},"projects":{"/p": {"hasTrustDialogAccepted": true}}}"#,
            ),
        ] {
            assert_eq!(
                with_claude_trust(document, "/p").unwrap().as_deref(),
                Some(expected),
                "{document}"
            );
        }
    }

    #[test]
    fn claude_trust_covers_only_the_folder_itself() {
        let document = r#"{"projects":{"/a":{"hasTrustDialogAccepted":true}}}"#;
        let updated = with_claude_trust(document, "/a/b").unwrap().unwrap();
        assert!(trusted(&updated, "/a/b"));
        let value: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(value["projects"].as_object().unwrap().len(), 2);
        // A parent's or a prefix's trust is not this folder's.
        let updated = with_claude_trust(document, "/ab").unwrap().unwrap();
        assert!(trusted(&updated, "/ab"));
    }

    #[test]
    fn a_claude_configuration_that_cannot_be_merged_is_refused() {
        for document in [
            "",
            "{",
            "not json",
            "[]",
            "\"text\"",
            r#"{"projects":null}"#,
            r#"{"projects":[]}"#,
            r#"{"projects":{"/p":true}}"#,
            r#"{"projects":{"/p":[]}}"#,
        ] {
            assert!(with_claude_trust(document, "/p").is_err(), "{document}");
        }
    }

    #[test]
    fn claude_reads_an_older_configuration_with_content_before_its_usual_one() {
        let temporary = Temporary::new();
        let home = temporary.folder("home", None);
        let config = temporary.folder("config", None);
        let home_os = Some(home.as_os_str());
        assert_eq!(
            claude_config_file(None, home_os),
            Some(home.join(".claude.json"))
        );
        assert_eq!(
            claude_config_file(Some(config.as_os_str()), home_os),
            Some(config.join(".claude.json"))
        );
        assert_eq!(
            claude_config_file(Some(OsStr::new("")), home_os),
            Some(home.join(".claude.json"))
        );
        // An empty older file is not read.
        fs::create_dir(home.join(".claude")).unwrap();
        fs::write(home.join(".claude/.config.json"), "").unwrap();
        fs::write(config.join(".config.json"), "").unwrap();
        assert_eq!(
            claude_config_file(None, home_os),
            Some(home.join(".claude.json"))
        );
        fs::write(home.join(".claude/.config.json"), "{}").unwrap();
        fs::write(config.join(".config.json"), "{}").unwrap();
        assert_eq!(
            claude_config_file(None, home_os),
            Some(home.join(".claude/.config.json"))
        );
        assert_eq!(
            claude_config_file(Some(config.as_os_str()), home_os),
            Some(config.join(".config.json"))
        );
        assert_eq!(claude_config_file(None, None), None);
    }

    #[test]
    fn claude_keys_the_folder_by_its_path_in_nfc() {
        let temporary = Temporary::new();
        let config = temporary.folder("config", None);
        fs::write(config.join(".claude.json"), "{}").unwrap();
        // "Zürich" with the umlaut as a combining mark, as some file systems keep it.
        let decomposed = temporary.0.join("Zu\u{308}rich");
        trust_for_claude(&decomposed, Some(config.as_os_str()));
        let written = fs::read_to_string(config.join(".claude.json")).unwrap();
        let composed = temporary.0.join("Z\u{fc}rich");
        assert!(trusted(&written, composed.to_str().unwrap()), "{written}");
        assert!(!trusted(&written, decomposed.to_str().unwrap()));
    }

    #[test]
    #[cfg(unix)]
    fn the_claude_configuration_is_written_through_its_link_under_the_lock_claude_takes() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temporary = Temporary::new();
        let target = temporary.folder("dotfiles", None).join("claude.json");
        let document = "{\n  \"projects\": {}\n}\n";
        fs::write(&target, document).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = temporary.0.join(".claude.json");
        symlink(&target, &link).unwrap();
        // Claude locks the name it reads, not the link's target.
        let lock = temporary.0.join(".claude.json.lock");
        fs::create_dir(&lock).unwrap();
        assert!(trust_claude_folder(&link, ODD, Duration::ZERO).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), document);
        fs::remove_dir(&lock).unwrap();
        fs::create_dir(sibling(&target, ".lock")).unwrap();
        assert_eq!(trust_claude_folder(&link, ODD, Duration::ZERO), Ok(true));
        fs::remove_dir(sibling(&target, ".lock")).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let written = fs::read_to_string(&target).unwrap();
        assert!(trusted(&written, ODD));
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Idempotent, and nothing is left behind: no lock, no temporary file.
        assert_eq!(trust_claude_folder(&link, ODD, Duration::ZERO), Ok(false));
        assert_eq!(fs::read_to_string(&target).unwrap(), written);
        assert!(!lock.exists());
        let names: Vec<_> = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["claude.json"]);
    }

    #[test]
    fn a_missing_locked_or_malformed_claude_configuration_is_left_as_it_is() {
        let temporary = Temporary::new();
        let file = temporary.0.join(".claude.json");
        // No configuration yet: Claude's first run writes its own.
        assert_eq!(trust_claude_folder(&file, "/p", Duration::ZERO), Ok(false));
        assert!(!file.exists());
        // Malformed: refused, unchanged.
        fs::write(&file, "{\"projects\": {").unwrap();
        assert!(trust_claude_folder(&file, "/p", Duration::ZERO).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "{\"projects\": {");
        // Claude is saving: after the wait, refused and unchanged, its lock kept.
        fs::write(&file, "{}").unwrap();
        let lock = temporary.0.join(".claude.json.lock");
        fs::create_dir(&lock).unwrap();
        let error = trust_claude_folder(&file, "/p", Duration::from_millis(120)).unwrap_err();
        assert!(error.contains("locked"), "{error}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "{}");
        assert!(lock.is_dir());
        // Once Claude is done, the entry goes in.
        fs::remove_dir(&lock).unwrap();
        assert_eq!(trust_claude_folder(&file, "/p", Duration::ZERO), Ok(true));
        assert!(trusted(&fs::read_to_string(&file).unwrap(), "/p"));
        assert!(!lock.exists());
    }

    #[test]
    fn a_test_never_saves_trust_in_the_users_real_configuration() {
        let home = PathBuf::from(env::var_os("HOME").unwrap());
        assert!(!may_write(&home.join(".claude.json")));
        assert!(!may_write(&home.join(".grok/trusted_folders.toml")));
        assert!(may_write(&Temporary::new().0.join(".claude.json")));
    }

    fn grok_trusted(document: &str, cwd: &str) -> Option<bool> {
        let parsed: toml_edit::DocumentMut = document.parse().unwrap();
        parsed.get("folders")?.get(cwd)?.get("trusted")?.as_bool()
    }

    #[test]
    fn grok_trust_is_appended_as_grok_writes_it_and_the_store_is_otherwise_kept() {
        let store = "# kept\n[folders.\"/Users/test/a\"]\ntrusted = true\ndecided_at = 1791971463\n\n[folders.'/Users/test/b']\ntrusted = true\n";
        let updated = with_grok_trust(store, "/Users/test/new", 1800000000)
            .unwrap()
            .unwrap();
        assert_eq!(
            updated,
            format!(
                "{store}\n[folders.\"/Users/test/new\"]\ntrusted = true\ndecided_at = 1800000000\n"
            )
        );
        // Nothing to do for a folder the store names, whatever it says there.
        assert_eq!(
            with_grok_trust(&updated, "/Users/test/new", 1).unwrap(),
            None
        );
        assert_eq!(with_grok_trust(store, "/Users/test/b", 1).unwrap(), None);
        let declined = "[folders.\"/p\"]\ntrusted = false\n";
        assert_eq!(with_grok_trust(declined, "/p", 1).unwrap(), None);
        // Every spelling of a folder makes one key.
        let odd = format!("/Users/test/{ODD}\n\u{7f}");
        for store in ["", "[other]\nx = 1", "[folders]\n", "x = 1\n\n"] {
            let updated = with_grok_trust(store, &odd, 1).unwrap().unwrap();
            assert!(updated.starts_with(store), "{updated}");
            assert_eq!(grok_trusted(&updated, &odd), Some(true), "{updated}");
        }
    }

    #[test]
    fn a_grok_store_that_cannot_be_merged_is_refused() {
        for store in [
            "[folders",
            "folders = 1\n",
            "folders = []\n",
            // TOML cannot add a table to an inline one.
            "folders = {}\n",
        ] {
            assert!(with_grok_trust(store, "/p", 1).is_err(), "{store}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn the_grok_store_is_created_private_and_merged_under_groks_lock() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = Temporary::new();
        let file = temporary.0.join("trusted_folders.toml");
        assert_eq!(trust_grok_folder(&file, "/p", 7, Duration::ZERO), Ok(true));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "[folders.\"/p\"]\ntrusted = true\ndecided_at = 7\n"
        );
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(trust_grok_folder(&file, "/p", 8, Duration::ZERO), Ok(false));
        // Grok is saving: refused after the wait, the store unchanged.
        let held = lock_file(&sibling(&file, ".lock"), Duration::ZERO).unwrap();
        let error = trust_grok_folder(&file, "/q", 9, Duration::from_millis(120)).unwrap_err();
        assert!(error.contains("locked"), "{error}");
        drop(held);
        assert_eq!(trust_grok_folder(&file, "/q", 9, Duration::ZERO), Ok(true));
        let store = fs::read_to_string(&file).unwrap();
        assert_eq!(grok_trusted(&store, "/p"), Some(true));
        assert_eq!(grok_trusted(&store, "/q"), Some(true));
        // A malformed store is left alone.
        fs::write(&file, "[folders").unwrap();
        assert!(trust_grok_folder(&file, "/r", 9, Duration::ZERO).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "[folders");
    }

    #[test]
    fn grok_trust_needs_a_grok_home_and_a_folder_grok_keys_by_itself() {
        let temporary = Temporary::new();
        let home = temporary.folder("grok home", None);
        let repository = temporary.folder("repository", Some(".git"));
        let worktree = temporary.folder("worktree", Some(".git file"));
        trust_for_grok(&worktree, Some(home.as_os_str()));
        assert!(!home.join("trusted_folders.toml").exists());
        trust_for_grok(&repository, Some(temporary.0.join("missing").as_os_str()));
        assert!(!temporary.0.join("missing").exists());
        trust_for_grok(&repository, Some(home.as_os_str()));
        let store = fs::read_to_string(home.join("trusted_folders.toml")).unwrap();
        assert_eq!(
            grok_trusted(&store, repository.to_str().unwrap()),
            Some(true)
        );
        assert_eq!(grok_trusted(&store, worktree.to_str().unwrap()), None);
    }
}
