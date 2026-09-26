//! Persistent shell sessions rendered by Ghostty and owned by RiWork.
//!
//! Ghostty renders an ordinary tmux client. The shell itself runs inside a
//! dedicated tmux server, so dropping a GPUI window only disconnects a client.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env, fs,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const HISTORY_LINES: usize = 100_000;
const ORCHESTRATOR_SKILL: &str = include_str!("../skills/riwork-orchestrator/SKILL.md");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    Project,
    Orchestrator,
}

/// An official coding CLI launched directly in a persistent terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    Codex,
    Claude,
}

impl HarnessKind {
    pub fn program(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellSession {
    pub id: String,
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
    pub kind: ShellKind,
    pub cwd: PathBuf,
    pub command: Option<String>,
    #[serde(default)]
    pub harness: Option<HarnessKind>,
    #[serde(default)]
    pub unrestricted: bool,
    #[serde(default)]
    pub orchestrator_skill_loaded: bool,
    #[serde(default)]
    pub orchestrator_skill_version: Option<String>,
    #[serde(default)]
    pub orchestrator_project_root: Option<PathBuf>,
    pub created_at_unix: u64,
    /// A live check against tmux, refreshed by `list` and `get`.
    #[serde(default, skip_deserializing)]
    pub alive: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct SessionMetrics {
    pub cpu_percent: f32,
    pub ram_bytes: u64,
    pub process_count: usize,
}

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    sessions: Vec<ShellSession>,
}

/// Handles shell metadata and a dedicated tmux server. Clone is safe because
/// all registry writes take the same on-disk lock.
#[derive(Clone, Debug)]
pub struct SessionManager {
    home: PathBuf,
    tmux: PathBuf,
    socket_name: String,
}

impl SessionManager {
    pub fn open_default() -> Result<Self, String> {
        let home = match env::var_os("RIWORK_HOME") {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(env::var_os("HOME").ok_or("HOME is not set; set RIWORK_HOME")?)
                .join(".local/share/riwork"),
        };
        Self::at(home)
    }

    /// Construct a manager with an alternate state directory. Useful for
    /// isolated installations and integration tests.
    pub fn at(home: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&home).map_err(|error| format!("create {}: {error}", home.display()))?;
        let home = home
            .canonicalize()
            .map_err(|error| format!("resolve {}: {error}", home.display()))?;
        let tmux = find_tmux()
            .ok_or("tmux is required for persistent shells; install it with `brew install tmux`")?;
        let socket_name = format!(
            "riwork-{:016x}",
            stable_hash(home.as_os_str().to_string_lossy().as_bytes())
        );
        Ok(Self {
            home,
            tmux,
            socket_name,
        })
    }

    /// Start a new shell for a project, optionally in a worktree. The command
    /// is run inside tmux; if omitted, tmux starts the user's default shell.
    pub fn create(
        &self,
        project_id: String,
        worktree_id: Option<String>,
        cwd: PathBuf,
        command: Option<String>,
    ) -> Result<ShellSession, String> {
        validate_uuid(&project_id)?;
        if let Some(id) = &worktree_id {
            validate_uuid(id)?;
        }
        self.create_inner(
            Some(project_id),
            worktree_id,
            ShellKind::Project,
            cwd,
            command,
            None,
            false,
        )
    }

    /// Launch the official CLI in its interactive terminal mode. Permission
    /// bypass flags are added only when explicitly requested by the caller.
    pub fn create_harness(
        &self,
        project_id: String,
        worktree_id: Option<String>,
        cwd: PathBuf,
        harness: HarnessKind,
        unrestricted: bool,
    ) -> Result<ShellSession, String> {
        validate_uuid(&project_id)?;
        if let Some(id) = &worktree_id {
            validate_uuid(id)?;
        }
        self.create_inner(
            Some(project_id),
            worktree_id,
            ShellKind::Project,
            cwd,
            None,
            Some(harness),
            unrestricted,
        )
    }

    /// Return the projectless global orchestrator, creating it when needed.
    /// The default command is the official `codex` CLI.
    pub fn orchestrator_create(
        &self,
        cwd: PathBuf,
        command: Option<String>,
    ) -> Result<ShellSession, String> {
        self.orchestrator_create_scoped(None, None, cwd, command)
    }

    /// Return this project's orchestrator. It has project ownership but no
    /// worktree ownership, and cannot replace another project's singleton.
    pub fn orchestrator_create_for_project(
        &self,
        project_id: String,
        project_root: PathBuf,
        command: Option<String>,
    ) -> Result<ShellSession, String> {
        validate_uuid(&project_id)?;
        self.orchestrator_create_scoped(
            Some(project_id),
            Some(project_root.clone()),
            project_root,
            command,
        )
    }

    fn orchestrator_create_scoped(
        &self,
        project_id: Option<String>,
        project_root: Option<PathBuf>,
        cwd: PathBuf,
        command: Option<String>,
    ) -> Result<ShellSession, String> {
        let lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        if let Some(existing) = registry
            .sessions
            .iter()
            .find(|session| matches_orchestrator_scope(session, project_id.as_deref()))
        {
            if self.is_alive(&existing.id) {
                let mut existing = existing.clone();
                existing.alive = true;
                return Ok(existing);
            }
        }
        registry
            .sessions
            .retain(|session| !matches_orchestrator_scope(session, project_id.as_deref()));
        if command
            .as_ref()
            .is_some_and(|command| command.trim().is_empty())
        {
            return Err("shell command cannot be empty".to_owned());
        }
        let project_root = project_root
            .map(|root| {
                root.canonicalize()
                    .map_err(|error| format!("resolve {}: {error}", root.display()))
                    .and_then(|root| {
                        if root.is_dir() {
                            Ok(root)
                        } else {
                            Err(format!("{} is not a directory", root.display()))
                        }
                    })
            })
            .transpose()?;
        let default_codex = command.is_none();
        let (cwd, command) = if default_codex {
            let skill_path = if project_id.is_none() {
                self.orchestrator_skill_path()?
            } else {
                self.orchestrator_skill_path_scoped(project_id.as_deref())?
            };
            let context = orchestrator_context(&self.home, project_id.as_deref());
            let program =
                find_program("codex").ok_or("codex is not installed or is not on PATH")?;
            let executable = env::current_exe()
                .map_err(|error| format!("resolve RiWork executable: {error}"))?;
            let command = orchestrator_command(
                &program,
                &context,
                &self.home,
                &skill_path,
                &executable,
                project_id.as_deref(),
                project_root.as_deref(),
            );
            let profiles = ["CODEX_HOME", "CLAUDE_CONFIG_DIR"]
                .map(|variable| (variable, env::var_os(variable)));
            (context, without_stale_profiles(&command, &profiles))
        } else {
            let command = command.expect("custom command is present");
            let command = if command == "codex" {
                find_program("codex")
                    .map(|path| quote_arg(&path.to_string_lossy()))
                    .unwrap_or(command)
            } else {
                command
            };
            (cwd, command)
        };
        let mut session = self.new_tmux_session(
            project_id,
            None,
            ShellKind::Orchestrator,
            cwd,
            Some(command),
            None,
            false,
        )?;
        session.orchestrator_project_root = project_root;
        if default_codex {
            session.harness = Some(HarnessKind::Codex);
            session.orchestrator_skill_loaded = true;
            session.orchestrator_skill_version = Some(orchestrator_skill_version());
        }
        registry.sessions.push(session.clone());
        if let Err(error) = self.write_registry(&registry) {
            let _ = self.kill_tmux_session(&session.id);
            return Err(error);
        }
        drop(lock);
        Ok(session)
    }

    pub fn orchestrator_get(&self) -> Result<Option<ShellSession>, String> {
        Ok(self
            .list()?
            .into_iter()
            .find(|session| matches_orchestrator_scope(session, None)))
    }

    pub fn orchestrator_get_for_project(
        &self,
        project_id: &str,
    ) -> Result<Option<ShellSession>, String> {
        validate_uuid(project_id)?;
        Ok(self
            .list()?
            .into_iter()
            .find(|session| matches_orchestrator_scope(session, Some(project_id))))
    }

    /// Install RiWork's shipped skill in the orchestrator's own context. The
    /// executable embeds the resource, so a bundled app needs no repository.
    pub fn orchestrator_skill_path(&self) -> Result<PathBuf, String> {
        self.orchestrator_skill_path_scoped(None)
    }

    fn orchestrator_skill_path_scoped(&self, project_id: Option<&str>) -> Result<PathBuf, String> {
        if let Some(project_id) = project_id {
            validate_uuid(project_id)?;
        }
        let directory =
            orchestrator_context(&self.home, project_id).join(".agents/skills/riwork-orchestrator");
        fs::create_dir_all(&directory)
            .map_err(|error| format!("create {}: {error}", directory.display()))?;
        let path = directory.join("SKILL.md");
        if fs::read(&path).is_ok_and(|content| content == ORCHESTRATOR_SKILL.as_bytes()) {
            return Ok(path);
        }
        let temporary = directory.join(format!(".SKILL-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            fs::write(&temporary, ORCHESTRATOR_SKILL)
                .map_err(|error| format!("write {}: {error}", temporary.display()))?;
            fs::rename(&temporary, &path)
                .map_err(|error| format!("install {}: {error}", path.display()))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map(|()| path)
    }

    pub fn orchestrator_skill_is_current(&self, session: &ShellSession) -> bool {
        session.orchestrator_skill_loaded
            && session.orchestrator_skill_version.as_deref()
                == Some(orchestrator_skill_version().as_str())
    }

    /// Explicitly load the skill into an existing Codex singleton. This is
    /// never invoked automatically for a legacy or custom session.
    pub fn load_orchestrator_skill(&self, id: &str) -> Result<ShellSession, String> {
        validate_uuid(id)?;
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let session = registry
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or_else(|| format!("unknown shell {id}"))?;
        if session.kind != ShellKind::Orchestrator {
            return Err(format!("shell {id} is not the orchestrator"));
        }
        if !self.is_alive(id) {
            return Err(format!("shell {id} has exited"));
        }
        session.alive = true;
        if self.orchestrator_skill_is_current(session) {
            return Ok(session.clone());
        }
        let output = self.tmux_checked(&[
            "display-message",
            "-p",
            "-t",
            &pane_target(id),
            "#{pane_current_command}\t#{pane_in_mode}\t#{pane_input_off}",
        ])?;
        let status = String::from_utf8_lossy(&output.stdout);
        let fields = status.trim().split('\t').collect::<Vec<_>>();
        if fields.first() != Some(&"codex") {
            return Err("skill loading requires a running Codex orchestrator".to_owned());
        }
        if fields.get(1) != Some(&"0") || fields.get(2) != Some(&"0") {
            return Err(
                "leave terminal copy mode and enable input before loading the skill".to_owned(),
            );
        }
        let skill_path = self.orchestrator_skill_path_scoped(session.project_id.as_deref())?;
        let executable =
            env::current_exe().map_err(|error| format!("resolve RiWork executable: {error}"))?;
        self.paste_and_submit(
            id,
            &orchestrator_prompt(
                &skill_path,
                &executable,
                session.project_id.as_deref(),
                session.orchestrator_project_root.as_deref(),
            ),
        )?;
        session.orchestrator_skill_loaded = true;
        session.orchestrator_skill_version = Some(orchestrator_skill_version());
        session.harness = Some(HarnessKind::Codex);
        let session = session.clone();
        self.write_registry(&registry)?;
        Ok(session)
    }

    pub fn list(&self) -> Result<Vec<ShellSession>, String> {
        let mut sessions = self.read_registry()?.sessions;
        let live = self.live_session_names()?;
        for session in &mut sessions {
            session.alive = live.contains(&session.id);
        }
        Ok(sessions)
    }

    pub fn get(&self, id: &str) -> Result<ShellSession, String> {
        validate_uuid(id)?;
        self.list()?
            .into_iter()
            .find(|session| session.id == id)
            .ok_or_else(|| format!("unknown shell {id}"))
    }

    /// The command to pass as `TerminalOptions.command` in gpui-libghostty.
    /// Ghostty parses the command into argv and renders the tmux client.
    pub fn attach_command(&self, id: &str) -> Result<String, String> {
        self.require_live(id)?;
        // Ghostty renders a tmux client: scrollback belongs to tmux, so mouse
        // reporting is needed for wheel/trackpad scrolling and copy mode.
        // Set this per session as well, upgrading sessions made by older builds.
        self.configure_scrolling(id)?;
        Ok(format!(
            "{} -u TMUX {} -L {} attach-session -t {}",
            quote_arg("/usr/bin/env"),
            quote_arg(&self.tmux.to_string_lossy()),
            quote_arg(&self.socket_name),
            quote_arg(id)
        ))
    }

    fn configure_scrolling(&self, id: &str) -> Result<(), String> {
        self.tmux_checked(&["set-option", "-t", id, "mouse", "on"])?;
        // RiWork owns this isolated tmux server. Alternate-screen applications
        // without mouse support (including Codex) still need tmux scrollback.
        // Applications that request mouse events keep receiving their wheel input.
        self.tmux_checked(&[
            "bind-key",
            "-T",
            "root",
            "WheelUpPane",
            "if-shell",
            "-F",
            "#{||:#{pane_in_mode},#{mouse_any_flag}}",
            "send-keys -M",
            "copy-mode -e; send-keys -M",
        ])?;
        Ok(())
    }

    /// Capture scrollback and the visible screen as plain text.
    pub fn capture(&self, id: &str, lines: usize) -> Result<String, String> {
        self.require_live(id)?;
        let start = format!("-{}", lines.clamp(1, HISTORY_LINES));
        let output =
            self.tmux_checked(&["capture-pane", "-p", "-t", &pane_target(id), "-S", &start])?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Send literal text followed by Return to an existing shell.
    pub fn send(&self, id: &str, text: &str) -> Result<(), String> {
        self.require_live(id)?;
        self.tmux_checked(&["send-keys", "-l", "-t", &pane_target(id), "--", text])?;
        self.tmux_checked(&["send-keys", "-t", &pane_target(id), "Enter"])?;
        Ok(())
    }

    pub fn current_directory(&self, id: &str) -> Result<PathBuf, String> {
        self.require_live(id)?;
        let output = self.tmux_checked(&[
            "display-message",
            "-p",
            "-t",
            &pane_target(id),
            "#{pane_current_path}",
        ])?;
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if path.is_empty() {
            return Err(format!("tmux did not report a directory for shell {id}"));
        }
        Ok(PathBuf::from(path))
    }

    pub fn metrics(&self, id: &str) -> Result<SessionMetrics, String> {
        self.require_live(id)?;
        self.metrics_snapshot()?
            .remove(id)
            .ok_or_else(|| format!("tmux did not report a process for shell {id}"))
    }

    /// One tmux query and one `ps` query for all shells, suitable for a footer
    /// that refreshes periodically. CPU is the sum of process `%cpu` values;
    /// RAM is resident bytes for the shell and its descendants.
    pub fn metrics_snapshot(&self) -> Result<BTreeMap<String, SessionMetrics>, String> {
        let sessions = self.list()?;
        let live: HashSet<&str> = sessions
            .iter()
            .filter(|session| session.alive)
            .map(|session| session.id.as_str())
            .collect();
        if live.is_empty() {
            return Ok(BTreeMap::new());
        }
        let output =
            self.tmux_checked(&["list-panes", "-a", "-F", "#{session_name}\t#{pane_pid}"])?;
        let mut roots = BTreeMap::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some((id, pid)) = line.split_once('\t') {
                if live.contains(id) {
                    if let Ok(pid) = pid.parse::<u32>() {
                        roots.insert(id.to_owned(), pid);
                    }
                }
            }
        }
        let processes = read_processes()?;
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (&pid, process) in &processes {
            children.entry(process.parent).or_default().push(pid);
        }
        let mut result = BTreeMap::new();
        for (id, root) in roots {
            let mut metrics = SessionMetrics::default();
            let mut pending = vec![root];
            let mut visited = HashSet::new();
            while let Some(pid) = pending.pop() {
                if !visited.insert(pid) {
                    continue;
                }
                if let Some(process) = processes.get(&pid) {
                    metrics.cpu_percent += process.cpu_percent;
                    metrics.ram_bytes += process.rss_kb.saturating_mul(1024);
                    metrics.process_count += 1;
                }
                if let Some(descendants) = children.get(&pid) {
                    pending.extend(descendants);
                }
            }
            result.insert(id, metrics);
        }
        Ok(result)
    }

    /// Explicitly end a shell. Closing its GPUI tab must not call this method.
    pub fn close(&self, id: &str) -> Result<(), String> {
        validate_uuid(id)?;
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        if !registry.sessions.iter().any(|session| session.id == id) {
            return Err(format!("unknown shell {id}"));
        }
        if self.is_alive(id) {
            self.kill_tmux_session(id)?;
        }
        registry.sessions.retain(|session| session.id != id);
        self.write_registry(&registry)
    }

    fn create_inner(
        &self,
        project_id: Option<String>,
        worktree_id: Option<String>,
        kind: ShellKind,
        cwd: PathBuf,
        command: Option<String>,
        harness: Option<HarnessKind>,
        unrestricted: bool,
    ) -> Result<ShellSession, String> {
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let session = self.new_tmux_session(
            project_id,
            worktree_id,
            kind,
            cwd,
            command,
            harness,
            unrestricted,
        )?;
        registry.sessions.push(session.clone());
        if let Err(error) = self.write_registry(&registry) {
            let _ = self.kill_tmux_session(&session.id);
            return Err(error);
        }
        Ok(session)
    }

    fn new_tmux_session(
        &self,
        project_id: Option<String>,
        worktree_id: Option<String>,
        kind: ShellKind,
        cwd: PathBuf,
        command: Option<String>,
        harness: Option<HarnessKind>,
        unrestricted: bool,
    ) -> Result<ShellSession, String> {
        let cwd = cwd
            .canonicalize()
            .map_err(|error| format!("resolve {}: {error}", cwd.display()))?;
        if !cwd.is_dir() {
            return Err(format!("{} is not a directory", cwd.display()));
        }
        let id = Uuid::new_v4().to_string();
        let profile_locations =
            ["CODEX_HOME", "CLAUDE_CONFIG_DIR"].map(|variable| (variable, env::var_os(variable)));
        let mut command = match harness {
            Some(harness) => {
                let program = find_program(harness.program()).ok_or_else(|| {
                    format!("{} is not installed or is not on PATH", harness.program())
                })?;
                let telemetry_exe = if harness == HarnessKind::Claude {
                    Some(env::current_exe().map_err(|error| {
                        format!("resolve RiWork executable for Claude telemetry: {error}")
                    })?)
                } else {
                    None
                };
                let command = harness_command(
                    harness,
                    unrestricted,
                    &program,
                    telemetry_exe.as_deref(),
                    &id,
                )?;
                Some(without_stale_profiles(&command, &profile_locations))
            }
            None => command,
        };
        if kind == ShellKind::Orchestrator && project_id.is_none() {
            command = command.map(|command| {
                let shell = if command.starts_with("exec ") {
                    PathBuf::from("/bin/sh")
                } else {
                    self.default_command_shell()
                };
                without_project_environment(&command, &shell)
            });
        }
        let mut args = vec![
            "new-session".to_owned(),
            "-d".to_owned(),
            "-s".to_owned(),
            id.clone(),
            "-c".to_owned(),
            cwd.to_string_lossy().into_owned(),
            "-x".to_owned(),
            "100".to_owned(),
            "-y".to_owned(),
            "30".to_owned(),
            "-e".to_owned(),
            format!("RIWORK_HOME={}", self.home.display()),
            "-e".to_owned(),
            format!("RIWORK_SHELL_ID={id}"),
        ];
        if kind == ShellKind::Orchestrator {
            for (variable, value) in orchestrator_environment(project_id.as_deref()) {
                args.push("-e".to_owned());
                args.push(format!("{variable}={value}"));
            }
        }
        // The tmux server may predate this launch's selected CLI profile.
        // Pass profile locations explicitly instead of inheriting old values.
        for (variable, value) in &profile_locations {
            if let Some(value) = value {
                args.push("-e".to_owned());
                args.push(format!("{variable}={}", value.to_string_lossy()));
            }
        }
        if let Some(command) = &command {
            if command.trim().is_empty() {
                return Err("shell command cannot be empty".to_owned());
            }
            args.push(command.clone());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        self.tmux_checked(&borrowed)?;
        let configured = self
            .tmux_checked(&[
                "set-window-option",
                "-t",
                &format!("{id}:0"),
                "history-limit",
                &HISTORY_LINES.to_string(),
            ])
            .and_then(|_| self.tmux_checked(&["set-option", "-g", "status", "off"]))
            .and_then(|_| self.configure_scrolling(&id));
        if let Err(error) = configured {
            let _ = self.kill_tmux_session(&id);
            return Err(error);
        }
        let created_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Ok(ShellSession {
            id,
            project_id,
            worktree_id,
            kind,
            cwd,
            command,
            harness,
            unrestricted,
            orchestrator_skill_loaded: false,
            orchestrator_skill_version: None,
            orchestrator_project_root: None,
            created_at_unix,
            alive: true,
        })
    }

    fn require_live(&self, id: &str) -> Result<ShellSession, String> {
        let session = self.get(id)?;
        if !session.alive {
            return Err(format!("shell {id} has exited"));
        }
        Ok(session)
    }

    fn is_alive(&self, id: &str) -> bool {
        self.tmux_command(&["has-session", "-t", &format!("={id}")])
            .is_ok_and(|output| output.status.success())
    }

    fn live_session_names(&self) -> Result<HashSet<String>, String> {
        let output = self.tmux_command(&["list-sessions", "-F", "#{session_name}"])?;
        if !output.status.success() {
            if no_tmux_server(&output) {
                return Ok(HashSet::new());
            }
            return Err(tmux_error(&output));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect())
    }

    fn kill_tmux_session(&self, id: &str) -> Result<(), String> {
        self.tmux_checked(&["kill-session", "-t", &format!("={id}")])?;
        Ok(())
    }

    fn default_command_shell(&self) -> PathBuf {
        // A running server may use a different default shell from this app's
        // current environment. Keep custom command syntax in that same shell.
        if let Ok(output) = self.tmux_command(&["show-options", "-gqv", "default-shell"]) {
            if output.status.success() {
                let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
                if path.is_file() {
                    return path;
                }
            }
        }
        env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("/bin/sh"))
    }

    fn paste_and_submit(&self, id: &str, text: &str) -> Result<(), String> {
        // Paste as one bracketed block: literal newline key events could submit
        // fragments of the embedded skill instead of one complete prompt.
        let buffer = format!("riwork-skill-{}", Uuid::new_v4());
        self.tmux_checked(&["set-buffer", "-b", &buffer, "--", text])?;
        let result = self
            .tmux_checked(&[
                "paste-buffer",
                "-p",
                "-r",
                "-d",
                "-b",
                &buffer,
                "-t",
                &pane_target(id),
            ])
            .and_then(|_| self.tmux_checked(&["send-keys", "-t", &pane_target(id), "Enter"]));
        if result.is_err() {
            let _ = self.tmux_checked(&["delete-buffer", "-b", &buffer]);
        }
        result.map(|_| ())
    }

    fn tmux_command(&self, args: &[&str]) -> Result<Output, String> {
        Command::new(&self.tmux)
            .arg("-L")
            .arg(&self.socket_name)
            .arg("-f")
            .arg("/dev/null")
            .args(args)
            .env_remove("TMUX")
            .env("PATH", effective_path())
            .output()
            .map_err(|error| format!("run {}: {error}", self.tmux.display()))
    }

    fn tmux_checked(&self, args: &[&str]) -> Result<Output, String> {
        let output = self.tmux_command(args)?;
        if !output.status.success() {
            return Err(tmux_error(&output));
        }
        Ok(output)
    }

    fn lock_registry(&self) -> Result<File, String> {
        let path = self.home.join("sessions.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        file.lock()
            .map_err(|error| format!("lock {}: {error}", path.display()))?;
        Ok(file)
    }

    fn read_registry(&self) -> Result<Registry, String> {
        let path = self.home.join("sessions.json");
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("parse {}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(error) => Err(format!("read {}: {error}", path.display())),
        }
    }

    fn write_registry(&self, registry: &Registry) -> Result<(), String> {
        let path = self.home.join("sessions.json");
        let temporary = self.home.join(format!(".sessions-{}.tmp", Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(registry)
            .map_err(|error| format!("serialize shell registry: {error}"))?;
        let result = (|| -> Result<(), String> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| format!("create {}: {error}", temporary.display()))?;
            file.write_all(&bytes)
                .map_err(|error| format!("write {}: {error}", temporary.display()))?;
            file.sync_all()
                .map_err(|error| format!("sync {}: {error}", temporary.display()))?;
            fs::rename(&temporary, &path).map_err(|error| {
                format!(
                    "replace {} with {}: {error}",
                    path.display(),
                    temporary.display()
                )
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn find_tmux() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = executable_dirs()
        .into_iter()
        .map(|dir| dir.join("tmux"))
        .collect();
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/tmux"),
        PathBuf::from("/usr/local/bin/tmux"),
        PathBuf::from("/opt/local/bin/tmux"),
        PathBuf::from("/usr/bin/tmux"),
    ]);
    candidates.into_iter().find(|path| path.is_file())
}

fn find_program(name: &str) -> Option<PathBuf> {
    executable_dirs()
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .and_then(|path| path.canonicalize().ok())
}

fn harness_command(
    harness: HarnessKind,
    unrestricted: bool,
    program: &Path,
    telemetry_exe: Option<&Path>,
    shell_id: &str,
) -> Result<String, String> {
    let mut arguments = vec![quote_arg(&program.to_string_lossy())];
    match harness {
        HarnessKind::Codex => {
            if unrestricted {
                arguments.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
            }
        }
        HarnessKind::Claude => {
            if unrestricted {
                arguments.push("--dangerously-skip-permissions".to_owned());
            }
            let telemetry_exe = telemetry_exe.ok_or("Claude telemetry executable is missing")?;
            let telemetry_command = format!(
                "{} telemetry claude {}",
                quote_arg(&telemetry_exe.to_string_lossy()),
                quote_arg(shell_id)
            );
            let settings = serde_json::json!({
                "statusLine": { "type": "command", "command": telemetry_command }
            });
            arguments.push("--settings".to_owned());
            arguments.push(quote_arg(&settings.to_string()));
        }
    }
    Ok(format!("exec {}", arguments.join(" ")))
}

fn orchestrator_skill_version() -> String {
    format!("{:016x}", stable_hash(ORCHESTRATOR_SKILL.as_bytes()))
}

fn matches_orchestrator_scope(session: &ShellSession, project_id: Option<&str>) -> bool {
    session.kind == ShellKind::Orchestrator && session.project_id.as_deref() == project_id
}

fn orchestrator_context(home: &Path, project_id: Option<&str>) -> PathBuf {
    match project_id {
        Some(project_id) => home.join("orchestrators/projects").join(project_id),
        None => home.join("orchestrator"),
    }
}

fn orchestrator_environment(project_id: Option<&str>) -> Vec<(&'static str, String)> {
    match project_id {
        Some(project_id) => vec![
            ("RIWORK_ORCHESTRATOR_SCOPE", "project".to_owned()),
            ("RIWORK_PROJECT_ID", project_id.to_owned()),
        ],
        None => vec![("RIWORK_ORCHESTRATOR_SCOPE", "global".to_owned())],
    }
}

fn without_project_environment(command: &str, shell: &Path) -> String {
    if command.starts_with("exec ") {
        without_stale_profiles(command, &[("RIWORK_PROJECT_ID", None)])
    } else {
        // Preserve a custom script as one shell argument, including pipelines,
        // sequencing, and its original quoting. No server globals are changed.
        format!(
            "exec /usr/bin/env -u RIWORK_PROJECT_ID {} -c {}",
            quote_arg(&shell.to_string_lossy()),
            quote_arg(command)
        )
    }
}

fn orchestrator_prompt(
    skill_path: &Path,
    executable: &Path,
    project_id: Option<&str>,
    project_root: Option<&Path>,
) -> String {
    let scope = match project_id {
        Some(project_id) => format!(
            "Scope: project. Your project UUID is {project_id}; its root is {}. \
             RIWORK_ORCHESTRATOR_SCOPE=project and RIWORK_PROJECT_ID={project_id}. \
             Coordinate only this project's tasks and worker sessions using explicit project IDs. \
             Your isolated orchestration context is not the project's repository root.",
            project_root
                .map(|root| quote_arg(&root.to_string_lossy()))
                .unwrap_or_else(
                    || "unknown; inspect this project's registered state first".to_owned()
                )
        ),
        None => "Scope: global. You have no project or worktree ownership. \
                 RIWORK_ORCHESTRATOR_SCOPE=global and RIWORK_PROJECT_ID is unset. \
                 Coordinate across project orchestrators according to the user's objective."
            .to_owned(),
    };
    format!(
        "$riwork-orchestrator\n\n\
         Load the complete riwork-orchestrator skill below for this RiWork orchestrator session. \
         Its installed source is {}. The installed RiWork CLI is {}: use this executable \
         for `riwork` commands if PATH does not resolve it, and retain RIWORK_HOME. \
         {} \
         This startup message only loads the skill. Do not inspect projects, create tasks, \
         modify repositories, delegate work, submit input to other harnesses, or create schedules. \
         After loading, briefly acknowledge readiness and wait for the user's objective.\n\n\
         <riwork-orchestrator-skill>\n{}\n</riwork-orchestrator-skill>",
        quote_arg(&skill_path.to_string_lossy()),
        quote_arg(&executable.to_string_lossy()),
        scope,
        ORCHESTRATOR_SKILL
    )
}

fn orchestrator_command(
    program: &Path,
    context: &Path,
    state_home: &Path,
    skill_path: &Path,
    executable: &Path,
    project_id: Option<&str>,
    project_root: Option<&Path>,
) -> String {
    let mut arguments = vec![
        program.to_string_lossy().into_owned(),
        "--cd".to_owned(),
        context.to_string_lossy().into_owned(),
        "--add-dir".to_owned(),
        state_home.to_string_lossy().into_owned(),
    ];
    if let Some(root) = project_root {
        arguments.push("--add-dir".to_owned());
        arguments.push(root.to_string_lossy().into_owned());
    }
    arguments.push(orchestrator_prompt(
        skill_path,
        executable,
        project_id,
        project_root,
    ));
    format!(
        "exec {}",
        arguments
            .iter()
            .map(|argument| quote_arg(argument))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// A tmux server retains its original environment. Remove profile variables
/// missing from this launch before the CLI starts, without altering other
/// sessions or the server's global environment.
fn without_stale_profiles(
    command: &str,
    profile_locations: &[(&str, Option<std::ffi::OsString>)],
) -> String {
    let missing = profile_locations
        .iter()
        .filter(|(_, value)| value.is_none())
        .map(|(variable, _)| format!("-u {}", quote_arg(variable)))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        command.to_owned()
    } else {
        format!(
            "exec /usr/bin/env {} {}",
            missing.join(" "),
            command.strip_prefix("exec ").unwrap_or(command)
        )
    }
}

fn executable_dirs() -> Vec<PathBuf> {
    let mut dirs = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    for path in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
        let path = PathBuf::from(path);
        if !dirs.contains(&path) {
            dirs.push(path);
        }
    }
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        for path in [home.join(".local/bin"), home.join(".cargo/bin")] {
            if !dirs.contains(&path) {
                dirs.push(path);
            }
        }
    }
    dirs
}

fn effective_path() -> std::ffi::OsString {
    env::join_paths(executable_dirs()).unwrap_or_else(|_| env::var_os("PATH").unwrap_or_default())
}

fn validate_uuid(id: &str) -> Result<(), String> {
    if Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        Ok(())
    } else {
        Err(format!("invalid UUID: {id}"))
    }
}

fn pane_target(id: &str) -> String {
    format!("{id}:0.0")
}

fn no_tmux_server(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains("no server running") || stderr.contains("error connecting to")
}

fn tmux_error(output: &Output) -> String {
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if detail.is_empty() {
        format!("tmux exited with {}", output.status)
    } else {
        format!("tmux: {detail}")
    }
}

fn quote_arg(argument: &str) -> String {
    if !argument.is_empty()
        && argument
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._:-".contains(&byte))
    {
        argument.to_owned()
    } else {
        format!("'{}'", argument.replace('\'', "'\\''"))
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

struct ProcessInfo {
    parent: u32,
    cpu_percent: f32,
    rss_kb: u64,
}

fn read_processes() -> Result<HashMap<u32, ProcessInfo>, String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,%cpu=,rss="])
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("run ps: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ps: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut processes = HashMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let Some((Ok(pid), Ok(parent), Ok(cpu_percent), Ok(rss_kb))) = fields
            .next()
            .zip(fields.next())
            .zip(fields.next())
            .zip(fields.next())
            .map(|(((pid, parent), cpu), rss)| {
                (
                    pid.parse::<u32>(),
                    parent.parse::<u32>(),
                    cpu.parse::<f32>(),
                    rss.parse::<u64>(),
                )
            })
        else {
            continue;
        };
        processes.insert(
            pid,
            ProcessInfo {
                parent,
                cpu_percent,
                rss_kb,
            },
        );
    }
    Ok(processes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_arguments(command: &str) -> Vec<String> {
        // Parse only: the command becomes positional arguments and is never
        // executed. This verifies settings JSON survives the shell boundary.
        let command = command.strip_prefix("exec ").unwrap_or(command);
        let script = format!("set -- {command}; printf '%s\\0' \"$@\"");
        let output = Command::new("/bin/sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(output.status.success());
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| String::from_utf8(argument.to_vec()).unwrap())
            .collect()
    }

    #[test]
    fn legacy_sessions_remain_regular_shells() {
        let session: ShellSession = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-4000-8000-000000000001",
            "project_id": null,
            "worktree_id": null,
            "kind": "project",
            "cwd": "/tmp",
            "command": null,
            "created_at_unix": 0,
            "alive": true
        }))
        .unwrap();
        assert_eq!(session.harness, None);
        assert!(!session.unrestricted);
        assert!(!session.orchestrator_skill_loaded);
        assert_eq!(session.orchestrator_skill_version, None);
        assert_eq!(session.orchestrator_project_root, None);
        assert!(!session.alive);
    }

    #[test]
    fn codex_permission_bypass_requires_explicit_selection() {
        let program = Path::new("/Applications/Codex CLI/codex");
        let normal = harness_command(HarnessKind::Codex, false, program, None, "uuid").unwrap();
        assert_eq!(shell_arguments(&normal), [program.to_string_lossy()]);
        let unrestricted =
            harness_command(HarnessKind::Codex, true, program, None, "uuid").unwrap();
        assert_eq!(
            shell_arguments(&unrestricted),
            [
                program.to_string_lossy().into_owned(),
                "--dangerously-bypass-approvals-and-sandbox".to_owned()
            ]
        );
    }

    #[test]
    fn claude_settings_keep_telemetry_bound_to_the_shell() {
        let program = Path::new("/Users/test/Claude CLI/claude");
        let telemetry = Path::new("/Users/test/RiWork's App/riwork");
        let id = "00000000-0000-4000-8000-000000000002";
        for unrestricted in [false, true] {
            let command = harness_command(
                HarnessKind::Claude,
                unrestricted,
                program,
                Some(telemetry),
                id,
            )
            .unwrap();
            let arguments = shell_arguments(&command);
            assert_eq!(arguments[0], program.to_string_lossy());
            assert_eq!(
                arguments
                    .iter()
                    .any(|arg| arg == "--dangerously-skip-permissions"),
                unrestricted
            );
            let settings_index = arguments
                .iter()
                .position(|arg| arg == "--settings")
                .unwrap();
            let settings: serde_json::Value =
                serde_json::from_str(&arguments[settings_index + 1]).unwrap();
            assert_eq!(settings["statusLine"]["type"], "command");
            let telemetry_command = settings["statusLine"]["command"].as_str().unwrap();
            assert_eq!(
                shell_arguments(telemetry_command),
                [
                    telemetry.to_string_lossy().into_owned(),
                    "telemetry".to_owned(),
                    "claude".to_owned(),
                    id.to_owned()
                ]
            );
        }
    }

    #[test]
    fn command_arguments_preserve_shell_metacharacters() {
        let arguments = [
            "space here",
            "a'b",
            "$(printf injected)",
            "`printf injected`",
            "a\nb",
        ];
        let command = arguments.map(quote_arg).join(" ");
        assert_eq!(shell_arguments(&command), arguments);
        assert_eq!(quote_arg(""), "''");
    }

    #[test]
    fn harness_launch_removes_only_absent_profile_variables() {
        let command = "exec /opt/bin/codex";
        let none = [("CODEX_HOME", None), ("CLAUDE_CONFIG_DIR", None)];
        assert_eq!(
            shell_arguments(&without_stale_profiles(command, &none)),
            [
                "/usr/bin/env",
                "-u",
                "CODEX_HOME",
                "-u",
                "CLAUDE_CONFIG_DIR",
                "/opt/bin/codex"
            ]
        );
        let codex = [
            (
                "CODEX_HOME",
                Some(std::ffi::OsString::from("/Users/test/codex A")),
            ),
            ("CLAUDE_CONFIG_DIR", None),
        ];
        assert_eq!(
            shell_arguments(&without_stale_profiles(command, &codex)),
            ["/usr/bin/env", "-u", "CLAUDE_CONFIG_DIR", "/opt/bin/codex"]
        );
        let claude = [
            ("CODEX_HOME", None),
            (
                "CLAUDE_CONFIG_DIR",
                Some(std::ffi::OsString::from("/Users/test/claude A")),
            ),
        ];
        assert_eq!(
            shell_arguments(&without_stale_profiles(command, &claude)),
            ["/usr/bin/env", "-u", "CODEX_HOME", "/opt/bin/codex"]
        );
        let both = [
            (
                "CODEX_HOME",
                Some(std::ffi::OsString::from("/Users/test/codex A")),
            ),
            (
                "CLAUDE_CONFIG_DIR",
                Some(std::ffi::OsString::from("/Users/test/claude A")),
            ),
        ];
        assert_eq!(without_stale_profiles(command, &both), command);
    }

    #[test]
    fn orchestrator_startup_loads_the_complete_skill_as_one_prompt() {
        let context = Path::new("/Users/test/RiWork's State/orchestrator");
        let state_home = context.parent().unwrap();
        let skill = context.join(".agents/skills/riwork-orchestrator/SKILL.md");
        let executable = Path::new("/Applications/RiWork App/Contents/MacOS/riwork");
        let command = orchestrator_command(
            Path::new("/opt/bin/codex"),
            context,
            state_home,
            &skill,
            executable,
            None,
            None,
        );
        let arguments = shell_arguments(&command);
        assert_eq!(arguments.len(), 6);
        assert_eq!(arguments[0], "/opt/bin/codex");
        assert_eq!(arguments[1], "--cd");
        assert_eq!(arguments[2], context.to_string_lossy());
        assert_eq!(arguments[3], "--add-dir");
        assert_eq!(arguments[4], state_home.to_string_lossy());
        let prompt = &arguments[5];
        assert!(prompt.starts_with("$riwork-orchestrator\n"));
        assert!(prompt.contains(ORCHESTRATOR_SKILL));
        assert!(prompt.contains("wait for the user's objective"));
        assert!(prompt.contains(&quote_arg(&skill.to_string_lossy())));
        assert!(prompt.contains(&quote_arg(&executable.to_string_lossy())));
        assert!(prompt.contains("Scope: global."));
        assert!(
            !arguments
                .iter()
                .any(|argument| argument == "--dangerously-bypass-approvals-and-sandbox")
        );
    }

    fn scope_session(kind: ShellKind, project_id: Option<&str>) -> ShellSession {
        serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-4000-8000-000000000001",
            "project_id": project_id,
            "worktree_id": null,
            "kind": kind,
            "cwd": "/tmp",
            "command": "codex",
            "created_at_unix": 0
        }))
        .unwrap()
    }

    #[test]
    fn orchestrator_scope_selection_never_matches_another_project_or_worker() {
        let alpha = "00000000-0000-4000-8000-000000000010";
        let beta = "00000000-0000-4000-8000-000000000011";
        let sessions = [
            scope_session(ShellKind::Orchestrator, None),
            scope_session(ShellKind::Orchestrator, Some(alpha)),
            scope_session(ShellKind::Orchestrator, Some(beta)),
            scope_session(ShellKind::Project, Some(alpha)),
        ];
        for (scope, index) in [(None, 0), (Some(alpha), 1), (Some(beta), 2)] {
            let matching = sessions
                .iter()
                .enumerate()
                .filter(|(_, session)| matches_orchestrator_scope(session, scope))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            assert_eq!(matching, [index]);
            let retained = sessions
                .iter()
                .enumerate()
                .filter(|(_, session)| !matches_orchestrator_scope(session, scope))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            assert_eq!(retained.len(), 3);
            assert!(retained.contains(&3));
        }
    }

    #[test]
    fn project_orchestrator_preserves_root_and_scope_across_serialization() {
        let project = "00000000-0000-4000-8000-000000000010";
        let mut session = scope_session(ShellKind::Orchestrator, Some(project));
        session.orchestrator_project_root = Some(PathBuf::from("/Users/test/wrapper project"));
        session.orchestrator_skill_loaded = true;
        session.orchestrator_skill_version = Some(orchestrator_skill_version());
        let bytes = serde_json::to_vec(&session).unwrap();
        let restored: ShellSession = serde_json::from_slice(&bytes).unwrap();
        assert!(matches_orchestrator_scope(&restored, Some(project)));
        assert!(!matches_orchestrator_scope(&restored, None));
        assert_eq!(restored.worktree_id, None);
        assert_eq!(
            restored.orchestrator_project_root,
            session.orchestrator_project_root
        );
        assert_eq!(
            restored.orchestrator_skill_version,
            session.orchestrator_skill_version
        );
    }

    #[test]
    fn project_context_prompt_and_environment_are_distinct_from_global() {
        let home = Path::new("/Users/test/RiWork's State");
        let alpha = "00000000-0000-4000-8000-000000000010";
        let beta = "00000000-0000-4000-8000-000000000011";
        assert_eq!(orchestrator_context(home, None), home.join("orchestrator"));
        let context = orchestrator_context(home, Some(alpha));
        assert_eq!(context, home.join("orchestrators/projects").join(alpha));
        assert_ne!(context, orchestrator_context(home, Some(beta)));
        assert!(validate_uuid("../../another-project").is_err());
        let project_root = Path::new("/Users/test/wrapper project");
        let skill = context.join(".agents/skills/riwork-orchestrator/SKILL.md");
        let command = orchestrator_command(
            Path::new("/opt/bin/codex"),
            &context,
            home,
            &skill,
            Path::new("/Applications/RiWork App/Contents/MacOS/riwork"),
            Some(alpha),
            Some(project_root),
        );
        let arguments = shell_arguments(&command);
        assert_eq!(arguments[1], "--cd");
        assert_eq!(arguments[2], context.to_string_lossy());
        assert_eq!(arguments[5], "--add-dir");
        assert_eq!(arguments[6], project_root.to_string_lossy());
        let prompt = arguments.last().unwrap();
        assert!(prompt.contains("Scope: project."));
        assert!(prompt.contains(alpha));
        assert!(!prompt.contains(beta));
        assert!(prompt.contains(&quote_arg(&project_root.to_string_lossy())));
        assert!(prompt.contains(ORCHESTRATOR_SKILL));
        assert_eq!(
            orchestrator_environment(Some(alpha)),
            [
                ("RIWORK_ORCHESTRATOR_SCOPE", "project".to_owned()),
                ("RIWORK_PROJECT_ID", alpha.to_owned())
            ]
        );
        assert_eq!(
            orchestrator_environment(None),
            [("RIWORK_ORCHESTRATOR_SCOPE", "global".to_owned())]
        );
    }

    #[test]
    fn global_scope_removes_stale_project_id_and_keeps_custom_script_intact() {
        assert_eq!(
            shell_arguments(&without_project_environment(
                "exec /opt/bin/codex",
                Path::new("/bin/zsh")
            )),
            ["/usr/bin/env", "-u", "RIWORK_PROJECT_ID", "/opt/bin/codex"]
        );
        let custom = "printf '%s' 'a;b'; custom-tool --flag | cat";
        assert_eq!(
            shell_arguments(&without_project_environment(custom, Path::new("/bin/zsh"))),
            [
                "/usr/bin/env",
                "-u",
                "RIWORK_PROJECT_ID",
                "/bin/zsh",
                "-c",
                custom
            ]
        );
    }
}
