//! Resume the exact Codex conversation in its existing RiWork pane after a
//! persisted turn completion. Any observed newer turn cancels the reload.

use crate::sessions::{HarnessKind, SessionManager};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const RELOAD_DIRECTORY: &str = "session-reloads";
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;
/// The worker records "waiting" within milliseconds; a binary that cannot do
/// that (an incompatible build) is reported instead of left "queued".
const WORKER_START_TIMEOUT: Duration = Duration::from_secs(20);
const WORKER_START_POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Serialize)]
pub struct SessionReloadReport {
    pub queued: bool,
    pub shell_id: String,
    pub thread_id: String,
    pub receipt: PathBuf,
    pub worker_pid: u32,
    pub waiting_for_turn: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReloadRequest {
    home: PathBuf,
    shell_id: String,
    thread_id: String,
    executable: PathBuf,
    log_path: PathBuf,
    cursor: u64,
    active_turn: Option<String>,
    original_command: Option<String>,
    unrestricted: bool,
    codex_home: Option<String>,
    claude_config_dir: Option<String>,
    profile: Option<String>,
}

#[derive(Serialize)]
struct ReloadReceipt<'a> {
    status: &'a str,
    shell_id: &'a str,
    thread_id: &'a str,
    message: &'a str,
}

#[derive(Deserialize)]
struct StoredReceipt {
    status: String,
    #[serde(default)]
    message: String,
}

#[derive(Default, Debug)]
struct LogSnapshot {
    cursor: u64,
    active_turn: Option<String>,
    saw_lifecycle: bool,
    session_id: Option<String>,
    subagent: bool,
    noninteractive: bool,
}

// Ignore all message bodies and tool content when examining a rollout.
#[derive(Deserialize)]
struct LogRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    payload: LogPayload,
}

#[derive(Default, Deserialize)]
struct LogPayload {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    source: Option<serde_json::Value>,
}

#[derive(Debug, PartialEq, Eq)]
enum Lifecycle {
    Started(String),
    Completed(String),
}

/// Target an explicitly selected frontend when a shared Codex backend's
/// inherited shell ID belongs to a different RiWork pane. Without an explicit
/// target, use the current thread's bound pane marker. Wait for completion
/// before replacing the selected pane's CLI.
pub fn queue_reload_for_shell(
    riwork_exe: &Path,
    shell_id: Option<&str>,
) -> Result<SessionReloadReport, String> {
    let request = current_request(riwork_exe, shell_id)?;
    let directory = request.home.join(RELOAD_DIRECTORY);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Cannot create reload directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("Cannot protect reload directory: {error}"))?;
    }
    let request_path = directory.join(format!("{}.json", Uuid::new_v4()));
    write_new_json(&request_path, &request)?;
    let receipt = receipt_path(&request_path);
    write_receipt(
        &receipt,
        &request,
        "queued",
        "Waiting for a persisted completion of the current turn",
    )?;
    let worker_pid = launch_worker(&request, &request_path, &receipt, WORKER_START_TIMEOUT)?;
    Ok(SessionReloadReport {
        queued: true,
        shell_id: request.shell_id,
        thread_id: request.thread_id,
        receipt,
        worker_pid,
        waiting_for_turn: request.active_turn,
    })
}

/// Start the detached worker and wait until it has taken over the receipt. The
/// worker is the GUI binary, which may be a different build than this CLI: one
/// that rejects the subcommand or the request exits without ever writing a
/// receipt, and that must surface as an error rather than a forever-"queued"
/// reload that never happens.
fn launch_worker(
    request: &ReloadRequest,
    request_path: &Path,
    receipt: &Path,
    timeout: Duration,
) -> Result<u32, String> {
    let log_path = request_path.with_extension("log");
    let log = private_file(&log_path)?;
    let mut command = Command::new(&request.executable);
    command
        .arg("reload-session-worker")
        .arg(request_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            log.try_clone().map_err(|error| error.to_string())?,
        ))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let message = format!("Cannot start session reload worker: {error}");
            write_receipt(receipt, request, "failed", &message)?;
            return Err(message);
        }
    };
    let worker_pid = child.id();
    let deadline = Instant::now() + timeout;
    let failure = loop {
        match worker_verdict(receipt) {
            Verdict::Running => {}
            Verdict::Accepted => break None,
            Verdict::Refused(message) => {
                reap(child);
                return Err(message);
            }
        }
        // Exiting or timing out is judged only after one more look at the
        // receipt: the worker records its verdict before it exits, possibly
        // between the read above and this check.
        let ended = match child.try_wait() {
            Ok(Some(status)) => Some(format!("exited with {status} before recording progress")),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                Some(format!(
                    "did not start within {} seconds",
                    timeout.as_secs_f32().ceil()
                ))
            }
            Ok(None) => {
                thread::sleep(WORKER_START_POLL);
                None
            }
            Err(error) => Some(format!("could not be watched: {error}")),
        };
        if let Some(ended) = ended {
            match worker_verdict(receipt) {
                Verdict::Accepted => break None,
                Verdict::Refused(message) => return Err(message),
                Verdict::Running => {
                    break Some(format!(
                        "The session reload worker ({}) {ended}: {}",
                        request.executable.display(),
                        log_tail(&log_path)
                    ));
                }
            }
        }
    };
    if let Some(message) = failure {
        write_receipt(receipt, request, "failed", &message)?;
        return Err(format!("{message}. Nothing was restarted."));
    }
    reap(child);
    Ok(worker_pid)
}

enum Verdict {
    Running,
    Accepted,
    Refused(String),
}

/// What the worker itself has recorded: it took over the receipt ("waiting", or
/// "completed" when the turn had already finished) or refused the request.
fn worker_verdict(receipt: &Path) -> Verdict {
    match read_receipt(receipt) {
        Some(stored) => match stored.status.as_str() {
            "waiting" | "completed" => Verdict::Accepted,
            "aborted" | "failed" => Verdict::Refused(stored.message),
            _ => Verdict::Running,
        },
        None => Verdict::Running,
    }
}

/// Collect the worker's exit status without ever blocking the caller.
fn reap(mut child: std::process::Child) {
    thread::spawn(move || {
        let _ = child.wait();
    });
}

fn read_receipt(path: &Path) -> Option<StoredReceipt> {
    let file = File::open(path).ok()?;
    serde_json::from_reader(file.take(64 * 1024)).ok()
}

/// The end of the worker's output, flattened to one line for an error message.
fn log_tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return "(no output)".to_owned();
    };
    let length = file.metadata().map_or(0, |metadata| metadata.len());
    let mut bytes = Vec::new();
    if file
        .seek(SeekFrom::Start(length.saturating_sub(400)))
        .is_ok()
    {
        let _ = file.take(400).read_to_end(&mut bytes);
    }
    let text = String::from_utf8_lossy(&bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        "(no output)".to_owned()
    } else {
        text
    }
}

/// Check the exact conversation and selected live pane before any GUI reload.
/// This does not create a receipt or worker.
pub fn validate_reload_for_shell(shell_id: Option<&str>) -> Result<(), String> {
    let executable = env::current_exe()
        .map_err(|error| format!("Cannot locate the RiWork executable: {error}"))?;
    current_request(&executable, shell_id).map(|_| ())
}

fn current_request(
    riwork_exe: &Path,
    explicit_shell: Option<&str>,
) -> Result<ReloadRequest, String> {
    let home = PathBuf::from(
        env::var_os("RIWORK_HOME")
            .ok_or("Run --session from a RiWork Codex terminal (RIWORK_HOME is missing)")?,
    )
    .canonicalize()
    .map_err(|error| format!("Cannot resolve RIWORK_HOME: {error}"))?;
    // Shared Codex backends can inherit RIWORK_SHELL_ID from an older
    // frontend. Only the per-thread shell policy marker identifies this pane.
    let bound_shell = env::var("RIWORK_CODEX_SHELL_ID").ok();
    let shell_id = select_shell_id(explicit_shell, bound_shell.as_deref())?;
    let thread_id = required_uuid("CODEX_THREAD_ID")?;
    let manager = SessionManager::at(home.clone())?;
    let session = manager.get(&shell_id)?;
    if !session.alive || session.harness != Some(HarnessKind::Codex) {
        return Err("The current RiWork shell must be a live Codex session".to_owned());
    }
    crate::cua::CuaManager::at(home.clone())?.driver_path()?;
    let executable = riwork_exe
        .canonicalize()
        .map_err(|error| format!("Cannot resolve RiWork executable: {error}"))?;
    if !executable.is_file() {
        return Err("The RiWork executable is not a file".to_owned());
    }
    let codex_home = frozen_codex_home(&session)?;
    let log_home = match &codex_home {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env::var_os("HOME").ok_or("HOME is missing")?).join(".codex"),
    };
    let log_path = find_rollout(&log_home.join("sessions"), &thread_id)?;
    let snapshot = snapshot(&log_path)?;
    if snapshot.subagent {
        return Err(
            "Run --session from the main RiWork Codex conversation, not a delegated agent"
                .to_owned(),
        );
    }
    if snapshot.noninteractive {
        return Err(
            "Run --session from the interactive RiWork Codex conversation, not a codex exec child"
                .to_owned(),
        );
    }
    if snapshot.session_id.as_deref() != Some(&thread_id) || !snapshot.saw_lifecycle {
        return Err(
            "Cannot confirm this Codex conversation's persisted turn state; nothing was restarted"
                .to_owned(),
        );
    }
    let profile = metadata_profile(session.command.as_deref())?;
    Ok(ReloadRequest {
        home: home.clone(),
        shell_id: shell_id.clone(),
        thread_id: thread_id.clone(),
        executable,
        log_path,
        cursor: snapshot.cursor,
        active_turn: snapshot.active_turn.clone(),
        original_command: session.command,
        unrestricted: session.unrestricted,
        codex_home,
        claude_config_dir: profile_environment("CLAUDE_CONFIG_DIR")?,
        profile,
    })
}

/// Internal CLI entry point for the detached helper. No other pane, server,
/// or Codex configuration file is changed.
pub fn run_reload_worker(request_path: &Path) -> Result<(), String> {
    let request: ReloadRequest = serde_json::from_reader(
        File::open(request_path).map_err(|error| format!("Cannot open reload request: {error}"))?,
    )
    .map_err(|error| format!("Cannot read reload request: {error}"))?;
    validate_request(request_path, &request)?;
    let receipt = receipt_path(request_path);
    let lock_path = request
        .home
        .join(RELOAD_DIRECTORY)
        .join(format!("{}.lock", request.shell_id));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|error| format!("Cannot open session reload lock: {error}"))?;
    if let Err(error) = fs2::FileExt::try_lock_exclusive(&lock) {
        let message = format!("Another reload is already pending for this pane: {error}");
        write_receipt(&receipt, &request, "aborted", &message)?;
        return Err(message);
    }
    write_receipt(
        &receipt,
        &request,
        "waiting",
        "Waiting for the queued turn to finish",
    )?;
    let result = finish_reload(&request);
    match &result {
        Ok(()) => write_receipt(
            &receipt,
            &request,
            "completed",
            "Resumed the exact Codex conversation in the original RiWork pane",
        )?,
        Err(message) => write_receipt(&receipt, &request, "failed", message)?,
    }
    result
}

fn finish_reload(request: &ReloadRequest) -> Result<(), String> {
    let current = snapshot(&request.log_path)?;
    if current.subagent
        || current.noninteractive
        || current.session_id.as_deref() != Some(&request.thread_id)
        || current.cursor < request.cursor
    {
        return Err(
            "The reload log no longer identifies the exact Codex thread; nothing was restarted"
                .to_owned(),
        );
    }
    let mut cursor = request.cursor;
    wait_for_completion(
        &request.log_path,
        &mut cursor,
        request.active_turn.as_deref(),
        COMPLETION_TIMEOUT,
    )?;
    let manager = SessionManager::at(request.home.clone())?;
    let session = manager.get(&request.shell_id)?;
    if !session.alive
        || session.harness != Some(HarnessKind::Codex)
        || session.command != request.original_command
        || session.unrestricted != request.unrestricted
    {
        return Err(
            "The original Codex pane changed while reload was pending; nothing was restarted"
                .to_owned(),
        );
    }
    // Cancel if a newer turn appeared while validating metadata. The final
    // log read and tmux respawn are separate operations, not an atomic gate.
    if read_lifecycle(&request.log_path, &mut cursor)?
        .iter()
        .any(|event| matches!(event, Lifecycle::Started(_)))
    {
        return Err("A newer Codex turn started; nothing was restarted".to_owned());
    }
    manager.respawn_command(&request.shell_id, &resume_command(request))?;
    Ok(())
}

fn wait_for_completion(
    log_path: &Path,
    cursor: &mut u64,
    turn: Option<&str>,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut completed = turn.is_none();
    loop {
        for event in read_lifecycle(log_path, cursor)? {
            match event {
                Lifecycle::Started(_) => {
                    return Err(
                        "A newer Codex turn started before reload; nothing was restarted"
                            .to_owned(),
                    );
                }
                Lifecycle::Completed(id) if turn == Some(id.as_str()) => completed = true,
                Lifecycle::Completed(_) => {}
            }
        }
        if completed {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                "Timed out waiting for the queued Codex turn to finish; nothing was restarted"
                    .to_owned(),
            );
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn snapshot(path: &Path) -> Result<LogSnapshot, String> {
    let mut result = LogSnapshot::default();
    let mut cursor = 0;
    scan_records(path, &mut cursor, |record| {
        if record.kind == "session_meta" {
            result.session_id = record.payload.id;
            result.subagent = record
                .payload
                .source
                .as_ref()
                .and_then(|source| source.get("subagent"))
                .is_some();
            result.noninteractive = record
                .payload
                .source
                .as_ref()
                .and_then(|source| source.as_str())
                == Some("exec");
        } else if let Some(event) = lifecycle(record) {
            result.saw_lifecycle = true;
            match event {
                Lifecycle::Started(id) => result.active_turn = Some(id),
                Lifecycle::Completed(id) if result.active_turn.as_deref() == Some(id.as_str()) => {
                    result.active_turn = None
                }
                Lifecycle::Completed(_) => {}
            }
        }
    })?;
    result.cursor = cursor;
    Ok(result)
}

fn read_lifecycle(path: &Path, cursor: &mut u64) -> Result<Vec<Lifecycle>, String> {
    let mut events = Vec::new();
    scan_records(path, cursor, |record| events.extend(lifecycle(record)))?;
    Ok(events)
}

fn lifecycle(record: LogRecord) -> Option<Lifecycle> {
    if record.kind != "event_msg" {
        return None;
    }
    let id = record.payload.turn_id?;
    match record.payload.kind.as_str() {
        "task_started" | "turn_started" => Some(Lifecycle::Started(id)),
        "task_complete" | "turn_complete" => Some(Lifecycle::Completed(id)),
        _ => None,
    }
}

/// Feed each complete record after `cursor` to `visit`, advancing the cursor
/// past it. Neither the number of records nor the size of one is buffered: a
/// rollout can be gigabytes and a line without a newline is cut off at the
/// record limit instead of being read to its end.
fn scan_records(
    path: &Path,
    cursor: &mut u64,
    mut visit: impl FnMut(LogRecord),
) -> Result<(), String> {
    let mut file =
        File::open(path).map_err(|error| format!("Cannot read Codex turn log: {error}"))?;
    if file.metadata().map_err(|error| error.to_string())?.len() < *cursor {
        return Err("The Codex turn log was truncated; nothing was restarted".to_owned());
    }
    file.seek(SeekFrom::Start(*cursor))
        .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let length = (&mut reader)
            .take(MAX_RECORD_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|error| error.to_string())?;
        if length > MAX_RECORD_BYTES {
            return Err("Codex log record exceeds the reload reader limit".to_owned());
        }
        if length == 0 || line.last() != Some(&b'\n') {
            break;
        }
        let record = serde_json::from_slice(&line)
            .map_err(|error| format!("Cannot confirm Codex turn state: {error}"))?;
        visit(record);
        *cursor += length as u64;
    }
    Ok(())
}

fn find_rollout(directory: &Path, thread_id: &str) -> Result<PathBuf, String> {
    let suffix = format!("-{thread_id}.jsonl");
    let mut pending = vec![directory.to_path_buf()];
    let mut matched = None;
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| {
            format!(
                "Cannot inspect Codex session directory {}: {error}",
                directory.display()
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() && entry.file_name().to_string_lossy().ends_with(&suffix) {
                if matched.is_some() {
                    return Err(
                        "Multiple logs match the exact Codex thread; nothing was restarted"
                            .to_owned(),
                    );
                }
                matched = Some(entry.path());
            }
        }
    }
    matched.ok_or_else(|| {
        format!("Cannot find a persisted log for Codex thread {thread_id}; nothing was restarted")
    })
}

fn required_uuid(variable: &str) -> Result<String, String> {
    let value = env::var(variable).map_err(|_| {
        format!("{variable} is missing; run --session from the current RiWork Codex conversation")
    })?;
    validate_uuid(&value)?;
    Ok(value)
}

fn select_shell_id(explicit: Option<&str>, bound_shell: Option<&str>) -> Result<String, String> {
    let value = explicit
        .or(bound_shell)
        .ok_or("Cannot identify this conversation terminal; use --session --shell UUID")?;
    validate_uuid(value)?;
    Ok(value.to_owned())
}

fn validate_uuid(value: &str) -> Result<(), String> {
    if Uuid::parse_str(value)
        .map_err(|_| "Session and thread IDs must be exact UUIDs".to_owned())?
        .to_string()
        != value
    {
        return Err("Session and thread IDs must be canonical UUIDs".to_owned());
    }
    Ok(())
}

fn frozen_codex_home(session: &crate::sessions::ShellSession) -> Result<Option<String>, String> {
    match &session.codex_home {
        Some(home) => home
            .clone()
            .into_os_string()
            .into_string()
            .map(Some)
            .map_err(|_| "This session's CODEX_HOME must contain a Unicode path".to_owned()),
        None => profile_environment("CODEX_HOME"),
    }
}

fn profile_environment(variable: &str) -> Result<Option<String>, String> {
    let Some(value) = env::var_os(variable) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    let path = if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    path.into_os_string()
        .into_string()
        .map(Some)
        .map_err(|_| format!("{variable} must contain a Unicode path"))
}

fn validate_request(path: &Path, request: &ReloadRequest) -> Result<(), String> {
    validate_uuid(&request.shell_id)?;
    validate_uuid(&request.thread_id)?;
    let directory = request
        .home
        .canonicalize()
        .map_err(|error| error.to_string())?
        .join(RELOAD_DIRECTORY);
    if path
        .canonicalize()
        .map_err(|error| error.to_string())?
        .parent()
        != Some(directory.as_path())
        || !request.executable.is_absolute()
        || !request.executable.is_file()
    {
        return Err("Invalid RiWork session reload request".to_owned());
    }
    Ok(())
}

fn resume_command(request: &ReloadRequest) -> String {
    let mut args = vec!["/usr/bin/env".to_owned()];
    for (name, value) in [
        ("CODEX_HOME", &request.codex_home),
        ("CLAUDE_CONFIG_DIR", &request.claude_config_dir),
    ] {
        if value.is_none() {
            args.extend(["-u".to_owned(), name.to_owned()]);
        }
    }
    for (name, value) in [
        ("CODEX_HOME", &request.codex_home),
        ("CLAUDE_CONFIG_DIR", &request.claude_config_dir),
    ] {
        if let Some(value) = value {
            args.push(format!("{name}={value}"));
        }
    }
    if let Some(home) = &request.codex_home {
        args.push(format!("RIWORK_CODEX_ACCOUNT_HOME={home}"));
    } else {
        args.extend(["-u".to_owned(), "RIWORK_CODEX_ACCOUNT_HOME".to_owned()]);
    }
    args.extend([
        request.executable.to_string_lossy().into_owned(),
        "cua".to_owned(),
        "harness".to_owned(),
        "codex".to_owned(),
        "--".to_owned(),
    ]);
    if let Some(profile) = &request.profile {
        args.extend(["--profile".to_owned(), profile.clone()]);
    }
    if request.unrestricted {
        args.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
    }
    args.extend(["resume".to_owned(), request.thread_id.clone()]);
    format!(
        "exec {}",
        args.iter()
            .map(|argument| shell_quote(argument))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn metadata_profile(command: Option<&str>) -> Result<Option<String>, String> {
    let Some(command) = command else {
        return Ok(None);
    };
    let arguments = shell_words(command)?;
    let mut profile = None;
    for (index, argument) in arguments.iter().enumerate() {
        if argument == "--profile" || argument == "-p" {
            let value = arguments
                .get(index + 1)
                .ok_or("The original Codex profile flag has no value")?;
            profile = Some(value.clone());
        } else if let Some(value) = argument.strip_prefix("--profile=") {
            profile = Some(value.to_owned());
        }
    }
    Ok(profile)
}

// Parse only saved argv for a profile flag. This never evaluates shell text.
fn shell_words(command: &str) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escape = false;
    let mut started = false;
    for character in command.chars() {
        if escape {
            word.push(character);
            escape = false;
            started = true;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escape = true;
            started = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                word.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => {
                quote = Some(character);
                started = true;
            }
            value if value.is_whitespace() => {
                if started {
                    result.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            value => {
                word.push(value);
                started = true;
            }
        }
    }
    if quote.is_some() || escape {
        return Err(
            "Cannot preserve the saved Codex command's profile because its quoting is incomplete"
                .to_owned(),
        );
    }
    if started {
        result.push(word);
    }
    Ok(result)
}

fn receipt_path(request: &Path) -> PathBuf {
    request.with_extension("status.json")
}

fn private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("Cannot create {}: {error}", path.display()))
}

fn write_new_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut file = private_file(path)?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn write_receipt(
    path: &Path,
    request: &ReloadRequest,
    status: &str,
    message: &str,
) -> Result<(), String> {
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    write_new_json(
        &temporary,
        &ReloadReceipt {
            status,
            shell_id: &request.shell_id,
            thread_id: &request.thread_id,
            message,
        },
    )?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("Cannot save session reload receipt: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temporary(PathBuf);
    impl Temporary {
        fn new() -> Self {
            let directory =
                env::temp_dir().join(format!("riwork-session-reload-test-{}", Uuid::new_v4()));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
    }
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn log_record(kind: &str, payload: serde_json::Value) -> String {
        format!("{}\n", serde_json::json!({"type":kind,"payload":payload}))
    }
    fn event(kind: &str, turn: &str) -> String {
        log_record("event_msg", serde_json::json!({"type":kind,"turn_id":turn}))
    }
    fn append(path: &Path, value: &str) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(value.as_bytes())
            .unwrap();
    }

    #[test]
    fn waits_for_the_new_completion_of_the_exact_turn() {
        let temporary = Temporary::new();
        let path = temporary.0.join("log.jsonl");
        fs::write(
            &path,
            format!(
                "{}{}{}",
                log_record("session_meta", serde_json::json!({"id":"thread"})),
                event("task_complete", "old"),
                event("task_started", "current")
            ),
        )
        .unwrap();
        let mut state = snapshot(&path).unwrap();
        assert_eq!(state.active_turn.as_deref(), Some("current"));
        append(&path, &event("task_complete", "old"));
        assert!(
            wait_for_completion(&path, &mut state.cursor, Some("current"), Duration::ZERO)
                .unwrap_err()
                .contains("Timed out")
        );
        append(&path, &event("task_complete", "current"));
        wait_for_completion(&path, &mut state.cursor, Some("current"), Duration::ZERO).unwrap();
    }

    #[test]
    fn newer_turn_aborts_even_if_the_queued_turn_completed() {
        let temporary = Temporary::new();
        let path = temporary.0.join("log.jsonl");
        fs::write(&path, event("task_started", "current")).unwrap();
        let mut cursor = snapshot(&path).unwrap().cursor;
        append(
            &path,
            &format!(
                "{}{}",
                event("task_complete", "current"),
                event("task_started", "newer")
            ),
        );
        assert!(
            wait_for_completion(&path, &mut cursor, Some("current"), Duration::ZERO)
                .unwrap_err()
                .contains("newer")
        );
    }

    #[test]
    fn partial_records_are_not_accepted_as_completion() {
        let temporary = Temporary::new();
        let path = temporary.0.join("log.jsonl");
        fs::write(&path, event("task_started", "current")).unwrap();
        let mut cursor = snapshot(&path).unwrap().cursor;
        let completion = event("task_complete", "current");
        append(&path, completion.trim_end());
        let before = cursor;
        assert!(read_lifecycle(&path, &mut cursor).unwrap().is_empty());
        assert_eq!(cursor, before);
        append(&path, "\n");
        wait_for_completion(&path, &mut cursor, Some("current"), Duration::ZERO).unwrap();
    }

    #[test]
    fn idle_and_truncated_logs_are_handled_conservatively() {
        let temporary = Temporary::new();
        let path = temporary.0.join("log.jsonl");
        fs::write(
            &path,
            format!(
                "{}{}",
                event("task_started", "turn"),
                event("task_complete", "turn")
            ),
        )
        .unwrap();
        let state = snapshot(&path).unwrap();
        assert!(state.active_turn.is_none());
        let mut cursor = state.cursor;
        wait_for_completion(&path, &mut cursor, None, Duration::ZERO).unwrap();
        fs::write(&path, "").unwrap();
        assert!(
            read_lifecycle(&path, &mut cursor)
                .unwrap_err()
                .contains("truncated")
        );
    }

    fn request(unrestricted: bool) -> ReloadRequest {
        ReloadRequest {
            home: PathBuf::from("/tmp/riwork"),
            shell_id: Uuid::new_v4().to_string(),
            thread_id: Uuid::new_v4().to_string(),
            executable: PathBuf::from("/tmp/Ri Work's $(touch nope)/riwork"),
            log_path: PathBuf::new(),
            cursor: 0,
            active_turn: None,
            original_command: None,
            unrestricted,
            codex_home: Some("/tmp/Codex's `profile`".to_owned()),
            claude_config_dir: None,
            profile: Some("a profile's name".to_owned()),
        }
    }

    #[test]
    fn reload_keeps_the_sessions_saved_account_home() {
        let session: crate::sessions::ShellSession = serde_json::from_value(serde_json::json!({
            "id":Uuid::new_v4().to_string(),"project_id":null,"worktree_id":null,
            "kind":"project","cwd":"/tmp","command":"codex","harness":"codex",
            "codex_home":"/tmp/account A's home","codex_account_id":"account-a",
            "created_at_unix":0
        }))
        .unwrap();
        assert_eq!(
            frozen_codex_home(&session).unwrap().as_deref(),
            Some("/tmp/account A's home")
        );
        let saved = request(false);
        let argv = shell_words(&resume_command(&saved)).unwrap();
        assert!(argv.contains(&format!(
            "RIWORK_CODEX_ACCOUNT_HOME={}",
            saved.codex_home.as_ref().unwrap()
        )));
    }

    #[test]
    fn resume_argv_preserves_profiles_exact_id_and_existing_permission_mode() {
        let unrestricted = request(true);
        let argv = shell_words(&resume_command(&unrestricted)).unwrap();
        assert_eq!(argv[0], "exec");
        assert_eq!(
            &argv[1..5],
            [
                "/usr/bin/env",
                "-u",
                "CLAUDE_CONFIG_DIR",
                "CODEX_HOME=/tmp/Codex's `profile`"
            ]
        );
        assert!(argv.contains(&unrestricted.executable.to_string_lossy().into_owned()));
        assert_eq!(
            &argv[argv.len() - 5..],
            [
                "--profile",
                "a profile's name",
                "--dangerously-bypass-approvals-and-sandbox",
                "resume",
                &unrestricted.thread_id
            ]
        );
        let restricted = request(false);
        assert!(
            !shell_words(&resume_command(&restricted))
                .unwrap()
                .iter()
                .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox")
        );
        assert_eq!(
            metadata_profile(Some("exec codex -p 'my profile' 'startup prompt' ")).unwrap(),
            Some("my profile".to_owned())
        );
    }

    #[test]
    #[cfg(unix)]
    fn real_shell_receives_literal_argv_and_profile_paths() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = Temporary::new();
        let mut saved = request(true);
        saved.executable = temporary.0.join("Ri Work's $(touch injected)");
        fs::write(
            &saved.executable,
            "#!/bin/sh\nprintf '%s\\n' \"$CODEX_HOME\" \"${CLAUDE_CONFIG_DIR-absent}\" \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&saved.executable, fs::Permissions::from_mode(0o700)).unwrap();
        let result = Command::new("/bin/sh")
            .arg("-c")
            .arg(resume_command(&saved))
            .env("CLAUDE_CONFIG_DIR", "stale tmux profile")
            .current_dir(&temporary.0)
            .output()
            .unwrap();
        assert!(result.status.success());
        let stdout = String::from_utf8(result.stdout).unwrap();
        let argv: Vec<_> = stdout.lines().collect();
        assert_eq!(
            argv,
            [
                "/tmp/Codex's `profile`",
                "absent",
                "cua",
                "harness",
                "codex",
                "--",
                "--profile",
                "a profile's name",
                "--dangerously-bypass-approvals-and-sandbox",
                "resume",
                &saved.thread_id,
            ]
        );
        assert!(!temporary.0.join("injected").exists());
    }

    #[test]
    fn delegated_rollouts_are_identified_without_reading_message_bodies() {
        let temporary = Temporary::new();
        let path = temporary.0.join("log.jsonl");
        fs::write(
            &path,
            log_record(
                "session_meta",
                serde_json::json!({
                    "id":"child", "source":{"subagent":{"thread_spawn":{"parent_thread_id":"root"}}}
                }),
            ),
        )
        .unwrap();
        assert!(snapshot(&path).unwrap().subagent);
        fs::write(
            &path,
            log_record(
                "session_meta",
                serde_json::json!({"id":"exec-child", "source":"exec"}),
            ),
        )
        .unwrap();
        assert!(snapshot(&path).unwrap().noninteractive);
    }

    #[test]
    fn record_reader_stops_at_the_record_limit_without_consuming_the_log() {
        let temporary = Temporary::new();
        let path = temporary.0.join("log.jsonl");
        let first = event("task_started", "turn");
        let mut file = File::create(&path).unwrap();
        file.write_all(first.as_bytes()).unwrap();
        // An unterminated record past the limit is refused, not read to its end.
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..(MAX_RECORD_BYTES / chunk.len() + 1) {
            file.write_all(&chunk).unwrap();
        }
        drop(file);
        let mut cursor = 0;
        let mut seen = 0;
        let error = scan_records(&path, &mut cursor, |_| seen += 1).unwrap_err();
        assert!(error.contains("exceeds the reload reader limit"), "{error}");
        // The record before it was delivered and the cursor stops at its end.
        assert_eq!((seen, cursor), (1, first.len() as u64));
        // A terminated oversized record is refused the same way.
        append(&path, "\n");
        assert!(scan_records(&path, &mut cursor, |_| ()).is_err());
        // A short unterminated tail is simply not consumed yet.
        fs::write(&path, first.clone() + "{\"type\":\"event_m").unwrap();
        let mut cursor = 0;
        scan_records(&path, &mut cursor, |_| ()).unwrap();
        assert_eq!(cursor, first.len() as u64);
    }

    #[cfg(unix)]
    struct Worker {
        request: ReloadRequest,
        request_path: PathBuf,
        receipt: PathBuf,
    }
    #[cfg(unix)]
    impl Worker {
        /// A GUI binary stand-in whose behavior is the given shell script body.
        fn new(temporary: &Temporary, body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let executable = temporary.0.join("fake-riwork");
            fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            let mut request = request(false);
            request.home = temporary.0.clone();
            request.executable = executable;
            let request_path = temporary.0.join(format!("{}.json", Uuid::new_v4()));
            let receipt = receipt_path(&request_path);
            write_receipt(&receipt, &request, "queued", "queued").unwrap();
            Self {
                request,
                request_path,
                receipt,
            }
        }
        fn launch(&self, timeout: Duration) -> Result<u32, String> {
            launch_worker(&self.request, &self.request_path, &self.receipt, timeout)
        }
        fn status(&self) -> StoredReceipt {
            read_receipt(&self.receipt).unwrap()
        }
    }

    #[cfg(unix)]
    #[test]
    fn worker_from_an_incompatible_build_is_reported_instead_of_staying_queued() {
        let temporary = Temporary::new();
        // Rejects the unknown subcommand and exits non-zero, writing nothing.
        let worker = Worker::new(
            &temporary,
            "echo 'error: unrecognized command reload-session-worker' >&2\nexit 2",
        );
        let error = worker.launch(Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("exit status: 2"), "{error}");
        assert!(error.contains("unrecognized command"), "{error}");
        assert!(error.contains("Nothing was restarted"), "{error}");
        let stored = worker.status();
        assert_eq!(stored.status, "failed");
        assert!(stored.message.contains("unrecognized command"));

        // An unrelated program that exits 0 with unexpected output.
        let worker = Worker::new(&temporary, "echo 'RiWork 0.0.1 usage: riwork open'\nexit 0");
        let error = worker.launch(Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("before recording progress"), "{error}");
        assert!(error.contains("usage: riwork open"), "{error}");
        assert_eq!(worker.status().status, "failed");

        // One that never comes up is stopped and reported.
        let worker = Worker::new(&temporary, "sleep 30");
        let started = Instant::now();
        let error = worker.launch(Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("did not start"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(worker.status().status, "failed");
    }

    #[cfg(unix)]
    #[test]
    fn worker_that_takes_over_the_receipt_is_accepted_or_its_own_refusal_reported() {
        let temporary = Temporary::new();
        let receipt = |status: &str, message: &str| {
            format!(
                "printf '{{\"status\":\"{status}\",\"shell_id\":\"s\",\"thread_id\":\"t\",\"message\":\"{message}\"}}' > \"${{2%.json}}.status.json\""
            )
        };
        let waiting = Worker::new(
            &temporary,
            &format!("{}\nsleep 1", receipt("waiting", "ok")),
        );
        assert!(waiting.launch(Duration::from_secs(5)).unwrap() > 0);
        assert_eq!(waiting.status().status, "waiting");

        // A worker that records its verdict and exits at once must never be
        // reported as having died silently, however the CLI's polls interleave.
        for _ in 0..15 {
            // A turn that had already finished completes before the CLI looks.
            let instant = Worker::new(&temporary, &receipt("completed", "done"));
            assert!(instant.launch(Duration::from_secs(5)).is_ok());

            let refused = Worker::new(
                &temporary,
                &format!(
                    "{}\nexit 1",
                    receipt("aborted", "Another reload is pending")
                ),
            );
            assert_eq!(
                refused.launch(Duration::from_secs(5)).unwrap_err(),
                "Another reload is pending"
            );
            assert_eq!(refused.status().status, "aborted");
        }

        let missing = Worker::new(&temporary, "");
        let mut request = missing.request;
        request.executable = temporary.0.join("does-not-exist");
        let error = launch_worker(
            &request,
            &missing.request_path,
            &missing.receipt,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(
            error.contains("Cannot start session reload worker"),
            "{error}"
        );
        assert_eq!(read_receipt(&missing.receipt).unwrap().status, "failed");
    }

    #[test]
    fn explicit_frontend_selection_overrides_a_stale_backend_shell_id() {
        let stale = "07fbd313-a058-4621-872e-76ff9377b5ee";
        let visible = "3accc20f-4182-41a0-8fce-f9a6583c39e1";
        assert_eq!(
            select_shell_id(Some(visible), Some(stale)).unwrap(),
            visible
        );
        assert_eq!(select_shell_id(Some(visible), None).unwrap(), visible);
        assert_eq!(select_shell_id(None, Some(visible)).unwrap(), visible);
        assert!(select_shell_id(Some("3accc20f"), Some(stale)).is_err());
        assert_eq!(
            select_shell_id(None, None).unwrap_err(),
            "Cannot identify this conversation terminal; use --session --shell UUID"
        );
    }
}
