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
/// Additive session guidance shared by both supported coding harnesses.
const CUA_GUIDANCE: &str = "RiWork provides Cua.ai Driver through the cua-driver MCP server. \
    Use this server for desktop screenshots, application inspection, and desktop interaction. \
    This is the Cua.ai product installed by RiWork setup. Keep this desktop automation \
    preference for the entire session and any delegated work. If the driver is unavailable \
    or reports missing macOS permissions, explain the reported problem and direct the user \
    to RiWork's Cua setup; do not silently switch to another computer-use provider. \
    Read the driver's tool descriptions and current state before interacting. \
    These instructions choose a desktop automation provider; they do not authorize new \
    tasks or change the user's permission policy.";

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
    /// Canonical worktree file opened by a dedicated Vim session.
    #[serde(default)]
    pub editor_path: Option<PathBuf>,
    #[serde(default)]
    pub harness: Option<HarnessKind>,
    #[serde(default)]
    pub unrestricted: bool,
    /// The account chosen when this Codex process started. Changing the
    /// preference only affects subsequent launches, never an existing agent.
    #[serde(default)]
    pub codex_account_id: Option<String>,
    #[serde(default)]
    pub codex_account_label: Option<String>,
    #[serde(default)]
    pub codex_account_email: Option<String>,
    #[serde(default)]
    pub codex_home: Option<PathBuf>,
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
            None,
        )
    }

    /// Open one selected regular worktree file in its own persistent tmux
    /// session. The file name is a single quoted shell argument after `--`;
    /// no input is sent to an existing agent terminal.
    pub fn create_editor(
        &self,
        project_id: String,
        worktree_id: Option<String>,
        root: PathBuf,
        path: PathBuf,
        expected: crate::file_preview::FileIdentity,
    ) -> Result<ShellSession, String> {
        validate_uuid(&project_id)?;
        if let Some(id) = &worktree_id {
            validate_uuid(id)?;
        }
        let path = crate::file_preview::validated_editor_path(&root, &path, expected)?;
        let vim = find_vim().ok_or("Vim is required to edit files in RiWork.")?;
        let command = editor_command(&vim, &path)?;
        self.create_inner(
            Some(project_id),
            worktree_id,
            ShellKind::Project,
            root,
            Some(command),
            None,
            false,
            Some(path),
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
            None,
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
    /// The default Codex launcher runs unrestricted.
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
        let id = Uuid::new_v4().to_string();
        let default_codex = command.is_none();
        let unrestricted = default_codex && project_id.is_some();
        let binding = if default_codex {
            Some(selected_codex_binding(&self.home, project_id.as_deref())?)
        } else {
            None
        };
        let (cwd, command) = if default_codex {
            let cua = crate::cua::CuaManager::at(self.home.clone())?;
            cua.driver_path()?;
            let executable = env::current_exe()
                .map_err(|error| format!("resolve RiWork executable: {error}"))?;
            let shim_directory = cua.ensure_harness_shims(&executable)?;
            let skill_path = if project_id.is_none() {
                self.orchestrator_skill_path()?
            } else {
                self.orchestrator_skill_path_scoped(project_id.as_deref())?
            };
            let context = orchestrator_context(&self.home, project_id.as_deref());
            let program = find_harness_program(HarnessKind::Codex, &shim_directory)
                .ok_or("codex is not installed or is not on PATH")?;
            let command = orchestrator_command(
                &program,
                &context,
                &self.home,
                &skill_path,
                &executable,
                project_id.as_deref(),
                project_root.as_deref(),
                &id,
                binding.as_ref().map(|binding| binding.home.as_path()),
            );
            (context, command)
        } else {
            let command = command.expect("custom command is present");
            (cwd, command)
        };
        let mut session = self.new_tmux_session(
            id,
            project_id,
            None,
            ShellKind::Orchestrator,
            cwd,
            Some(command),
            None,
            unrestricted,
            binding,
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

    /// Saved attribution does not depend on tmux still being alive after a
    /// harness finishes. Completion hooks use this metadata-only lookup.
    pub fn registered_session(&self, id: &str) -> Result<ShellSession, String> {
        validate_uuid(id)?;
        self.read_registry()?
            .sessions
            .into_iter()
            .find(|session| session.id == id)
            .ok_or_else(|| format!("unknown shell {id}"))
    }

    pub fn state_home(&self) -> &Path {
        &self.home
    }

    fn record_codex_launch(
        &self,
        id: &str,
        binding: &crate::codex_accounts::CodexAccountBinding,
        arguments: &[String],
        resumed: bool,
    ) -> Result<(), String> {
        validate_uuid(id)?;
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let session = registry
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or_else(|| format!("unknown shell {id}"))?;
        session.harness = Some(HarnessKind::Codex);
        session.codex_account_id = binding.id.clone();
        session.codex_account_label = binding.label.clone();
        session.codex_account_email = binding.email.clone();
        session.codex_home = Some(binding.home.clone());
        if !resumed {
            session.unrestricted = arguments
                .iter()
                .take_while(|argument| argument.as_str() != "--")
                .any(|argument| {
                    matches!(
                        argument.as_str(),
                        "--dangerously-bypass-approvals-and-sandbox" | "--yolo"
                    )
                });
        }
        self.write_registry(&registry)?;
        if !resumed {
            let path = self.home.join("agent-activity").join(format!("{id}.json"));
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "Cannot reset activity for the new Codex launch: {error}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// A notification can identify a legacy pane's actual log home without
    /// guessing from the current selection or moving its authenticated process.
    pub fn freeze_codex_home_if_unknown(&self, id: &str, log_home: &Path) -> Result<(), String> {
        validate_uuid(id)?;
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let Some(session) = registry
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
        else {
            return Ok(());
        };
        if session.codex_home.is_some() {
            return Ok(());
        }
        session.codex_home = Some(
            log_home
                .canonicalize()
                .map_err(|error| format!("Cannot resolve this Codex session's home: {error}"))?,
        );
        session.harness = Some(HarnessKind::Codex);
        self.write_registry(&registry)
    }

    /// Resume an explicitly selected Codex conversation in its existing pane.
    /// The caller must first wait for that conversation's active turn to finish.
    pub fn respawn_command(&self, id: &str, command: &str) -> Result<ShellSession, String> {
        validate_uuid(id)?;
        if command.trim().is_empty() {
            return Err("reload command cannot be empty".to_owned());
        }
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let session = registry
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or_else(|| format!("unknown shell {id}"))?;
        if session.harness != Some(HarnessKind::Codex) || !self.is_alive(id) {
            return Err("session reload requires a live RiWork Codex pane".to_owned());
        }
        let cwd = self.current_directory(id)?;
        let executable =
            env::current_exe().map_err(|error| format!("resolve RiWork executable: {error}"))?;
        let cua = crate::cua::CuaManager::at(self.home.clone())?;
        cua.driver_path()?;
        let shims = cua.ensure_harness_shims(&executable)?;
        let managed_path = path_with_harness_shims(&shims)?;
        let mut args = vec![
            "respawn-pane".to_owned(),
            "-k".to_owned(),
            "-t".to_owned(),
            pane_target(id),
            "-c".to_owned(),
            cwd.to_string_lossy().into_owned(),
            "-e".to_owned(),
            format!("RIWORK_HOME={}", self.home.display()),
            "-e".to_owned(),
            format!("RIWORK_SHELL_ID={id}"),
            "-e".to_owned(),
            format!("PATH={}", managed_path.to_string_lossy()),
        ];
        if let Some(home) = &session.codex_home {
            args.extend([
                "-e".to_owned(),
                format!("CODEX_HOME={}", home.display()),
                "-e".to_owned(),
                format!("RIWORK_CODEX_ACCOUNT_HOME={}", home.display()),
            ]);
        }
        args.push(command.to_owned());
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        self.tmux_checked(&borrowed)?;
        session.command = Some(command.to_owned());
        session.alive = true;
        let result = session.clone();
        self.write_registry(&registry).map_err(|error| {
            format!("Codex pane restarted, but metadata update failed: {error}")
        })?;
        Ok(result)
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

    /// A pane identity changes on respawn, even when its RiWork UUID is retained.
    pub(crate) fn schedule_pane_identity(&self, id: &str) -> Result<String, String> {
        validate_uuid(id)?;
        self.tmux_text(&[
            "display-message",
            "-p",
            "-t",
            &pane_target(id),
            "#{pane_id}|#{pane_pid}|#{pane_start_command}",
        ])
    }

    pub(crate) fn schedule_provider_identity(
        &self,
        shell: &ShellSession,
    ) -> Result<String, String> {
        match shell.harness {
            Some(HarnessKind::Codex) => {
                let tracker = crate::activity::ActivityTracker::at(self.home.clone());
                let bound = tracker.schedule_identity(shell);
                // Normal RiWork launches exec Codex in the pane. A wrapper or
                // shared server without exact descriptor proof stays unknown.
                let descriptor = self.schedule_codex_rollout(&shell.id)?;
                match (bound, descriptor) {
                    (Some(bound), Some(path)) => {
                        let home = shell.codex_home.as_ref().ok_or("Codex account home is unknown")?;
                        if !path.starts_with(home.join("sessions")) { return Err("Codex rollout moved outside the pinned account home".into()); }
                        let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("");
                        if !name.ends_with(&bound) { return Err("Codex thread changed in the selected pane".into()); }
                        Ok(bound)
                    },
                    (Some(bound), None) => Ok(bound),
                    (None, Some(path)) => tracker.bind_schedule_rollout(shell, &path),
                    (None, None) => Err("Codex thread identity is not yet known; complete a turn or use a directly launched RiWork Codex pane".into()),
                }
            }
            Some(HarnessKind::Claude) => crate::agent_hooks::schedule_state(&self.home, &shell.id)
                .map(|(id, _)| id)
                .ok_or(
                    "Claude session identity is not yet known; wait for a completed turn".into(),
                ),
            None => Err("Scheduling requires Codex or Claude".into()),
        }
    }

    fn schedule_codex_rollout(&self, id: &str) -> Result<Option<PathBuf>, String> {
        let pane = self.tmux_text(&[
            "display-message",
            "-p",
            "-t",
            &pane_target(id),
            "#{pane_pid}|#{pane_current_command}",
        ])?;
        let (pid, command) = pane
            .trim()
            .split_once('|')
            .ok_or("Cannot identify the harness process")?;
        if command != "codex" || pid.parse::<u32>().is_err() {
            return Ok(None);
        }
        // A bounded child query, never a scan of another pane's files/processes.
        let mut child = Command::new("/usr/sbin/lsof")
            .args(["-nP", "-a", "-p", pid, "-Fn"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot inspect Codex rollout identity: {e}"))?;
        let started = std::time::Instant::now();
        loop {
            if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                break;
            }
            if started.elapsed() > std::time::Duration::from_secs(2) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Codex descriptor identity check timed out".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err("Cannot verify Codex descriptor identity".into());
        }
        let paths: HashSet<_> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix('n'))
            .filter(|path| path.contains("/rollout-") && path.ends_with(".jsonl"))
            .map(PathBuf::from)
            .collect();
        if paths.len() != 1 {
            return Err("Codex has no unique open primary rollout; scheduling is deferred".into());
        }
        Ok(paths.into_iter().next())
    }

    /// The scheduling gate and normal `send` share the input lock. The registry
    /// lock also excludes RiWork close/respawn while checking and submitting.
    pub(crate) fn send_scheduled(
        &self,
        target: &crate::schedules::Target,
        state: &crate::store::State,
        text: &str,
        tracker: &mut crate::activity::ActivityTracker,
        claim: &mut dyn FnMut(&str) -> Result<bool, String>,
    ) -> Result<crate::schedules::Delivery, String> {
        use crate::schedules::{Delivery, Scope};
        let _registry = self.lock_registry()?;
        let shell = match self.get(&target.shell_id) { Ok(s) if s.alive => s, _ => return Ok(Delivery::Failed("Target session no longer exists or has exited; select an existing target explicitly.".into())) };
        if !target.matches(state, &shell)
            || self.schedule_pane_identity(&shell.id)? != target.pane_identity
            || self.schedule_provider_identity(&shell).ok().as_ref()
                != Some(&target.provider_session)
        {
            return Ok(Delivery::Failed(
                "Target identity changed; edit and explicitly select the intended session.".into(),
            ));
        }
        if let Scope::Workspace { worktree_id, .. } = &target.scope {
            let workspace = state
                .worktrees
                .iter()
                .find(|w| &w.id == worktree_id)
                .ok_or("Workspace is missing")?;
            if !self
                .current_directory(&shell.id)?
                .starts_with(&workspace.path)
            {
                return Ok(Delivery::Failed(
                    "Worker left the selected workspace.".into(),
                ));
            }
        }
        let mut gate_outcome = None;
        let mut claim_error = None;
        let mut claimed = false;
        let result = crate::session_input::submit_checked(
            &self.home,
            &shell.id,
            text,
            &|args| self.tmux_text(args),
            || {
                let token = match shell.harness {
                    Some(HarnessKind::Codex) => {
                        tracker.schedule_idle_token(&shell, &target.provider_session)
                    }
                    Some(HarnessKind::Claude) => {
                        crate::agent_hooks::schedule_state(&self.home, &shell.id)
                            .filter(|(id, _)| id == &target.provider_session)
                            .and_then(|(_, token)| token)
                    }
                    None => None,
                };
                let ready = (|| {
                    let token =
                        token.ok_or("Harness is busy, blocked or its idle lifecycle is unknown")?;
                    let first = self.schedule_prompt_screen(&shell)?;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if first != self.schedule_prompt_screen(&shell)? {
                        return Err("Harness prompt is changing".to_owned());
                    }
                    let second_token = match shell.harness {
                        Some(HarnessKind::Codex) => {
                            tracker.schedule_idle_token(&shell, &target.provider_session)
                        }
                        Some(HarnessKind::Claude) => {
                            crate::agent_hooks::schedule_state(&self.home, &shell.id)
                                .filter(|(id, _)| id == &target.provider_session)
                                .and_then(|(_, t)| t)
                        }
                        None => None,
                    };
                    if second_token.as_ref() != Some(&token) {
                        return Err("Harness lifecycle changed while checking readiness".into());
                    }
                    if self.schedule_provider_identity(&shell)? != target.provider_session {
                        return Err("Provider identity changed while checking readiness".into());
                    }
                    Ok(format!("{}:{token}", target.provider_session))
                })();
                let token = match ready {
                    Ok(token) => token,
                    Err(error) => {
                        gate_outcome = Some(Delivery::Deferred(error));
                        return Err("schedule gate deferred".into());
                    }
                };
                match claim(&token) {
                    Ok(true) => {
                        claimed = true;
                        Ok(())
                    }
                    Ok(false) => {
                        gate_outcome = Some(Delivery::Deferred("Waiting for a fresh completed turn or the four-attempts-per-minute delivery limit.".into()));
                        Err("schedule gate deferred".into())
                    }
                    Err(error) => {
                        claim_error = Some(error.clone());
                        Err(error)
                    }
                }
            },
        );
        if let Some(error) = claim_error {
            return Err(error);
        }
        if let Some(outcome) = gate_outcome {
            return Ok(outcome);
        }
        Ok(match result {
            Ok(()) => Delivery::Submitted,
            Err(error) if claimed => Delivery::Uncertain(format!(
                "Terminal delivery may be partial: {error}. Review the target; no automatic retry."
            )),
            Err(error) => Delivery::Deferred(format!("Terminal input unavailable: {error}")),
        })
    }

    fn schedule_prompt_screen(&self, shell: &ShellSession) -> Result<String, String> {
        let pane = pane_target(&shell.id);
        let cursor = self.tmux_text(&[
            "display-message",
            "-p",
            "-t",
            &pane,
            "#{cursor_y}|#{cursor_x}|#{pane_dead}",
        ])?;
        let fields: Vec<_> = cursor.trim().split('|').collect();
        let row = fields
            .first()
            .and_then(|v| v.parse::<usize>().ok())
            .ok_or("Cannot read harness cursor")?;
        if fields.get(2) != Some(&"0") {
            return Err("Harness has exited".into());
        }
        let screen = self.tmux_text(&["capture-pane", "-p", "-e", "-t", &pane])?;
        let column = fields
            .get(1)
            .and_then(|v| v.parse::<usize>().ok())
            .ok_or("Cannot read harness cursor")?;
        if !schedule_empty_prompt_at(shell.harness, &screen, row, column) {
            return Err(
                "Harness is not at an empty prompt (busy, approval, trust, login or draft input)"
                    .into(),
            );
        }
        Ok(format!("{cursor}{screen}"))
    }

    /// Send literal text followed by Return to an existing shell.
    pub fn send(&self, id: &str, text: &str) -> Result<(), String> {
        self.require_live(id)?;
        self.paste_and_submit(id, text)
    }

    pub fn resize_viewport(
        &self,
        id: &str,
        owner: &str,
        lease: &str,
        columns: u32,
        rows: u32,
    ) -> Result<crate::session_viewport::Size, String> {
        self.require_live(id)?;
        let exe = env::current_exe().map_err(|e| e.to_string())?;
        crate::session_viewport::resize(
            &self.home,
            crate::session_viewport::Size {
                shell_id: id.into(),
                columns,
                rows,
            },
            owner,
            lease,
            &exe,
            &|args| self.tmux_text(args),
        )
    }
    pub fn clear_viewport(&self, id: &str, owner: &str, lease: &str) -> Result<(), String> {
        crate::session_viewport::clear(&self.home, id, owner, lease, &|args| self.tmux_text(args))
    }
    pub fn watch_viewport(&self, id: &str, owner: &str, lease: &str) -> Result<(), String> {
        crate::session_viewport::watch(&self.home, id, owner, lease, &|args| self.tmux_text(args))
    }
    fn tmux_text(&self, args: &[&str]) -> Result<String, String> {
        Ok(String::from_utf8_lossy(&self.tmux_checked(args)?.stdout).into_owned())
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

    /// Collect the owned 0.0 pane's directory for every live shell in one query.
    /// Periodic UI sampling already has a live shell snapshot; rechecking every
    /// shell separately would reread the registry and spawn two tmux clients.
    pub fn current_directories(
        &self,
        shells: &[ShellSession],
    ) -> Result<BTreeMap<String, PathBuf>, String> {
        let live: HashSet<&str> = shells
            .iter()
            .filter(|shell| shell.alive)
            .map(|shell| shell.id.as_str())
            .collect();
        if live.is_empty() {
            return Ok(BTreeMap::new());
        }
        let output = self.tmux_checked(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{window_index}\t#{pane_index}\t#{pane_current_path}",
        ])?;
        Ok(pane_directories(
            &String::from_utf8_lossy(&output.stdout),
            &live,
        ))
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
        editor_path: Option<PathBuf>,
    ) -> Result<ShellSession, String> {
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let mut session = self.new_tmux_session(
            Uuid::new_v4().to_string(),
            project_id,
            worktree_id,
            kind,
            cwd,
            command,
            harness,
            unrestricted,
            None,
        )?;
        session.editor_path = editor_path;
        registry.sessions.push(session.clone());
        if let Err(error) = self.write_registry(&registry) {
            let _ = self.kill_tmux_session(&session.id);
            return Err(error);
        }
        Ok(session)
    }

    fn new_tmux_session(
        &self,
        id: String,
        project_id: Option<String>,
        worktree_id: Option<String>,
        kind: ShellKind,
        cwd: PathBuf,
        command: Option<String>,
        harness: Option<HarnessKind>,
        unrestricted: bool,
        binding: Option<crate::codex_accounts::CodexAccountBinding>,
    ) -> Result<ShellSession, String> {
        let binding = match binding {
            Some(binding) => Some(binding),
            None if harness == Some(HarnessKind::Codex) => {
                Some(selected_codex_binding(&self.home, project_id.as_deref())?)
            }
            None => None,
        };
        let cwd = cwd
            .canonicalize()
            .map_err(|error| format!("resolve {}: {error}", cwd.display()))?;
        if !cwd.is_dir() {
            return Err(format!("{} is not a directory", cwd.display()));
        }
        let plain_codex_home = if binding.is_none()
            && harness.is_none()
            && kind == ShellKind::Project
            && command.is_none()
        {
            Some(
                selected_codex_binding(&self.home, project_id.as_deref())?
                    .home
                    .into_os_string(),
            )
        } else {
            None
        };
        let profile_locations = [
            (
                "CODEX_HOME",
                binding
                    .as_ref()
                    .map(|binding| binding.home.clone().into_os_string())
                    .or(plain_codex_home)
                    .or_else(|| env::var_os("CODEX_HOME")),
            ),
            ("CLAUDE_CONFIG_DIR", env::var_os("CLAUDE_CONFIG_DIR")),
        ];
        let executable =
            env::current_exe().map_err(|error| format!("resolve RiWork executable: {error}"))?;
        let cua = crate::cua::CuaManager::at(self.home.clone())?;
        let shim_directory = cua.ensure_harness_shims(&executable)?;
        let managed_path = path_with_harness_shims(&shim_directory)?;
        let shell = self.default_command_shell();
        let zsh_environment = if shell.file_name().is_some_and(|name| name == "zsh") {
            let directory = install_zsh_startup_forwarding(&self.home, &shim_directory)?;
            zsh_startup_environment(&directory, env::var_os("ZDOTDIR").as_deref())
        } else {
            Vec::new()
        };
        let mut command = match harness {
            Some(harness) => {
                cua.driver_path()?;
                let program = find_harness_program(harness, &shim_directory).ok_or_else(|| {
                    format!("{} is not installed or is not on PATH", harness.program())
                })?;
                let command = harness_command(
                    harness,
                    unrestricted,
                    &program,
                    &executable,
                    &self.home,
                    &id,
                    binding.as_ref().map(|binding| binding.home.as_path()),
                )?;
                Some(without_stale_profiles(&command, &profile_locations))
            }
            None => command.map(|command| {
                if let Some(binding) = &binding {
                    without_stale_profiles(
                        &with_codex_home(&command, &binding.home),
                        &profile_locations,
                    )
                } else {
                    command
                }
            }),
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
            "-e".to_owned(),
            format!("PATH={}", managed_path.to_string_lossy()),
        ];
        args.extend([
            "-e".to_owned(),
            "RIWORK_CODEX_SHELL_ID=".to_owned(),
            "-e".to_owned(),
            format!(
                "RIWORK_CODEX_ACCOUNT_HOME={}",
                binding
                    .as_ref()
                    .map(|binding| binding.home.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
        ]);
        for (variable, value) in zsh_environment {
            args.push("-e".to_owned());
            args.push(format!("{variable}={value}"));
        }
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
            editor_path: None,
            harness,
            unrestricted,
            codex_account_id: binding.as_ref().and_then(|binding| binding.id.clone()),
            codex_account_label: binding.as_ref().and_then(|binding| binding.label.clone()),
            codex_account_email: binding.as_ref().and_then(|binding| binding.email.clone()),
            codex_home: binding.map(|binding| binding.home),
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
        crate::session_input::submit(&self.home, id, text, &|args| self.tmux_text(args))
    }

    fn tmux_command(&self, args: &[&str]) -> Result<Output, String> {
        Command::new(&self.tmux)
            .arg("-L")
            .arg(&self.socket_name)
            .arg("-f")
            .arg("/dev/null")
            .args(args)
            .env_remove("TMUX")
            .env_remove("RIWORK_RESTORE_TICKET")
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

fn find_vim() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("/usr/bin/vim")];
    candidates.extend(
        executable_dirs()
            .into_iter()
            .map(|dir| dir.join("vim"))
            .collect::<Vec<_>>(),
    );
    candidates.into_iter().find(|path| path.is_file())
}

fn editor_command(vim: &Path, path: &Path) -> Result<String, String> {
    let vim = vim
        .to_str()
        .ok_or("The Vim executable path cannot be passed to the shell.")?;
    let path = path
        .to_str()
        .ok_or("This file name cannot be passed to Vim.")?;
    Ok(format!("exec {} -- {}", quote_arg(vim), quote_arg(path)))
}

fn selected_codex_binding(
    home: &Path,
    project_id: Option<&str>,
) -> Result<crate::codex_accounts::CodexAccountBinding, String> {
    if let Some(project_id) = project_id {
        return crate::codex_accounts::resolve_project_launch_binding(home, project_id);
    }
    let settings = crate::settings::SettingsStore::open(home)?.load()?;
    crate::codex_accounts::resolve_launch_binding(home, settings.selected_codex_account.as_deref())
}

/// Keep the profile on the command as well as tmux's session environment:
/// login-shell startup files must not redirect a managed agent to another home.
fn with_codex_home(command: &str, home: &Path) -> String {
    format!(
        "exec /usr/bin/env {} {}",
        quote_arg(&format!("CODEX_HOME={}", home.display())),
        command.strip_prefix("exec ").unwrap_or(command)
    )
}

fn codex_account_environment_arguments(home: &Path) -> Vec<String> {
    ["CODEX_HOME", "RIWORK_CODEX_ACCOUNT_HOME"]
        .into_iter()
        .flat_map(|key| {
            [
                "-c".to_owned(),
                format!(
                    "shell_environment_policy.set.{key}={}",
                    toml_string(&home.to_string_lossy())
                ),
            ]
        })
        .collect()
}

fn harness_command(
    harness: HarnessKind,
    unrestricted: bool,
    program: &Path,
    executable: &Path,
    state_home: &Path,
    shell_id: &str,
    codex_home: Option<&Path>,
) -> Result<String, String> {
    let mut arguments = vec![program.to_string_lossy().into_owned()];
    arguments.extend(cua_harness_arguments(harness, executable, state_home));
    match harness {
        HarnessKind::Codex => {
            arguments.extend(codex_shell_environment_arguments(
                state_home,
                Some(shell_id),
            ));
            arguments.extend(codex_activity_arguments(
                executable,
                state_home,
                Some(shell_id),
                &[],
                codex_home,
            ));
            if let Some(home) = codex_home {
                arguments.extend(codex_account_environment_arguments(home));
            }
            if unrestricted {
                arguments.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
            }
            arguments.push(cua_startup_prompt());
        }
        HarnessKind::Claude => {
            if unrestricted {
                arguments.push("--dangerously-skip-permissions".to_owned());
            }
            let telemetry_command = format!(
                "{} telemetry claude {}",
                quote_arg(&executable.to_string_lossy()),
                quote_arg(shell_id)
            );
            let hook_command = format!(
                "{} agent-hook claude {} {}",
                quote_arg(&executable.to_string_lossy()),
                quote_arg(&state_home.to_string_lossy()),
                quote_arg(shell_id)
            );
            let settings = serde_json::json!({
                "statusLine": { "type": "command", "command": telemetry_command },
                "hooks": {
                    "UserPromptSubmit": [{"hooks":[{"type":"command","command":hook_command,"timeout":10}]}],
                    "Stop": [{"hooks":[{"type":"command","command":hook_command,"timeout":10}]}]
                }
            });
            arguments.push("--settings".to_owned());
            arguments.push(settings.to_string());
        }
    }
    let command = format!(
        "exec {}",
        arguments
            .iter()
            .map(|argument| quote_arg(argument))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Ok(match codex_home.filter(|_| harness == HarnessKind::Codex) {
        Some(home) => with_codex_home(&command, home),
        None => command,
    })
}

fn cua_startup_prompt() -> String {
    format!(
        "Load the following RiWork desktop automation guidance for this session. \
         This startup message only loads guidance. Briefly acknowledge readiness \
         and wait for the user's objective.\n\n{CUA_GUIDANCE}"
    )
}

/// Return actual CLI arguments so wrappers and tmux launches share one MCP
/// target without replacing the user's profile, authentication, or model.
fn cua_harness_arguments(
    harness: HarnessKind,
    executable: &Path,
    state_home: &Path,
) -> Vec<String> {
    match harness {
        HarnessKind::Codex => {
            let overrides = [
                format!(
                    "mcp_servers.cua-driver.command={}",
                    toml_string(&executable.to_string_lossy())
                ),
                "mcp_servers.cua-driver.args=[\"cua\",\"mcp\"]".to_owned(),
                format!(
                    "mcp_servers.cua-driver.env.RIWORK_HOME={}",
                    toml_string(&state_home.to_string_lossy())
                ),
                "mcp_servers.cua-driver.enabled=true".to_owned(),
                "mcp_servers.cua-driver.required=true".to_owned(),
                "mcp_servers.cua-driver.startup_timeout_sec=120".to_owned(),
            ];
            let mut arguments = vec!["--disable".to_owned(), "computer_use".to_owned()];
            for value in overrides {
                arguments.extend(["-c".to_owned(), value]);
            }
            arguments
        }
        HarnessKind::Claude => {
            let config = serde_json::json!({
                "mcpServers": {
                    "cua-driver": {
                        "type": "stdio",
                        "command": executable.to_string_lossy(),
                        "args": ["cua", "mcp"],
                        "env": { "RIWORK_HOME": state_home.to_string_lossy() }
                    }
                }
            });
            vec![
                "--mcp-config".to_owned(),
                config.to_string(),
                "--append-system-prompt".to_owned(),
                CUA_GUIDANCE.to_owned(),
            ]
        }
    }
}

fn toml_string(value: &str) -> String {
    // JSON basic-string escaping is also valid for these TOML string values,
    // including literal quotes, backslashes, and newlines in installed paths.
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

/// Bind tools to the launching client even when Codex reuses an app server
/// whose process environment belongs to a different RiWork pane. Dotted
/// overrides retain the user's remaining environment settings. The marker is
/// deliberately a thread config value, never a daemon environment variable.
fn codex_shell_environment_arguments(state_home: &Path, shell_id: Option<&str>) -> Vec<String> {
    let shell_id = shell_id.unwrap_or("");
    let values = [
        ("RIWORK_HOME", state_home.to_string_lossy().into_owned()),
        ("RIWORK_SHELL_ID", shell_id.to_owned()),
        ("RIWORK_CODEX_SHELL_ID", shell_id.to_owned()),
    ];
    values
        .into_iter()
        .flat_map(|(key, value)| {
            [
                "-c".to_owned(),
                format!("shell_environment_policy.set.{key}={}", toml_string(&value)),
            ]
        })
        .collect()
}

fn codex_activity_arguments(
    executable: &Path,
    state_home: &Path,
    shell_id: Option<&str>,
    arguments: &[String],
    explicit_home: Option<&Path>,
) -> Vec<String> {
    let log_home = explicit_home.map(Path::to_path_buf).or_else(|| {
        env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
    });
    let system_configs = [
        PathBuf::from("/etc/codex/config.toml"),
        PathBuf::from("/Library/Managed Preferences/com.openai.codex.plist"),
        PathBuf::from("/Library/Preferences/com.openai.codex.plist"),
    ];
    log_home
        .map(|home| {
            codex_activity_arguments_at(
                executable,
                state_home,
                shell_id,
                arguments,
                &home,
                &system_configs,
            )
        })
        .unwrap_or_default()
}

fn codex_activity_arguments_at(
    executable: &Path,
    state_home: &Path,
    shell_id: Option<&str>,
    arguments: &[String],
    log_home: &Path,
    system_configs: &[PathBuf],
) -> Vec<String> {
    let Some(shell_id) =
        shell_id.filter(|id| Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == *id))
    else {
        return Vec::new();
    };
    // Conservatively defer to any existing notification configuration. The
    // pane's exact resume binding still supplies activity when a user hook exists.
    if arguments.iter().any(|argument| argument.contains("notify")) {
        return Vec::new();
    }
    let mut configs = vec![log_home.join("config.toml")];
    configs.extend_from_slice(system_configs);
    match fs::read_dir(log_home) {
        Ok(entries) => {
            for (index, entry) in entries.enumerate() {
                if index >= 512 {
                    return Vec::new();
                }
                let Ok(entry) = entry else {
                    return Vec::new();
                };
                if entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".config.toml")
                {
                    configs.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Vec::new(),
    }
    for config in configs {
        match fs::metadata(&config) {
            Ok(metadata) if metadata.len() > 1024 * 1024 => return Vec::new(),
            // Managed plist encodings vary; do not override an unknown managed
            // notification policy merely because its binary keys are unreadable.
            Ok(_)
                if config
                    .extension()
                    .is_some_and(|extension| extension == "plist") =>
            {
                return Vec::new();
            }
            Ok(_) => match fs::read(&config) {
                Ok(contents)
                    if !contents
                        .windows(b"notify".len())
                        .any(|part| part == b"notify") => {}
                _ => return Vec::new(),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Vec::new(),
        }
    }
    let command = [
        executable.to_string_lossy().into_owned(),
        "agent-notify".to_owned(),
        state_home.to_string_lossy().into_owned(),
        shell_id.to_owned(),
    ];
    vec![
        "-c".to_owned(),
        format!(
            "notify={}",
            serde_json::to_string(&command).expect("serializing argv cannot fail")
        ),
    ]
}

fn pane_directories(output: &str, live: &HashSet<&str>) -> BTreeMap<String, PathBuf> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\t');
            let (id, window, pane, path) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            (live.contains(id) && window == "0" && pane == "0" && !path.is_empty())
                .then(|| (id.to_owned(), PathBuf::from(path)))
        })
        .collect()
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
         <riwork-orchestrator-skill>\n{}\n</riwork-orchestrator-skill>\n\n{}",
        quote_arg(&skill_path.to_string_lossy()),
        quote_arg(&executable.to_string_lossy()),
        scope,
        ORCHESTRATOR_SKILL,
        CUA_GUIDANCE
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
    shell_id: &str,
    codex_home: Option<&Path>,
) -> String {
    let mut arguments = vec![
        program.to_string_lossy().into_owned(),
        "--cd".to_owned(),
        context.to_string_lossy().into_owned(),
    ];
    if project_id.is_some() {
        arguments.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
    }
    arguments.extend(cua_harness_arguments(
        HarnessKind::Codex,
        executable,
        state_home,
    ));
    arguments.extend(codex_shell_environment_arguments(
        state_home,
        Some(shell_id),
    ));
    arguments.extend(codex_activity_arguments(
        executable,
        state_home,
        Some(shell_id),
        &[],
        codex_home,
    ));
    if let Some(home) = codex_home {
        arguments.extend(codex_account_environment_arguments(home));
    }
    arguments.push(orchestrator_prompt(
        skill_path,
        executable,
        project_id,
        project_root,
    ));
    let command = format!(
        "exec {}",
        arguments
            .iter()
            .map(|argument| quote_arg(argument))
            .collect::<Vec<_>>()
            .join(" ")
    );
    match codex_home {
        Some(home) => with_codex_home(&command, home),
        None => command,
    }
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

fn path_with_harness_shims(shim_directory: &Path) -> Result<std::ffi::OsString, String> {
    let mut directories = vec![shim_directory.to_path_buf()];
    directories.extend(
        executable_dirs()
            .into_iter()
            .filter(|directory| directory != shim_directory),
    );
    env::join_paths(directories).map_err(|error| format!("construct RiWork harness PATH: {error}"))
}

fn zsh_startup_environment(
    directory: &Path,
    original_zdotdir: Option<&std::ffi::OsStr>,
) -> Vec<(&'static str, String)> {
    vec![
        ("ZDOTDIR", directory.to_string_lossy().into_owned()),
        (
            "RIWORK_USER_ZDOTDIR",
            original_zdotdir
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
        (
            "RIWORK_USER_ZDOTDIR_SET",
            if original_zdotdir.is_some() { "1" } else { "0" }.to_owned(),
        ),
        (
            "RIWORK_USER_ZDOTDIR_EXPORT",
            if original_zdotdir.is_some() { "1" } else { "0" }.to_owned(),
        ),
    ]
}

/// macOS login zsh runs path_helper and user startup files after tmux passes
/// PATH. Forward those files, then restore the managed CLI prefix. The user's
/// files stay in place, and their final ZDOTDIR and Ghostty hooks are retained.
fn install_zsh_startup_forwarding(home: &Path, shim_directory: &Path) -> Result<PathBuf, String> {
    let directory = home.join("cua/shell-integration/zsh");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let restore = r#"if [[ "${RIWORK_USER_ZDOTDIR_SET:-0}" == 1 ]]; then
    'builtin' 'typeset' -g ZDOTDIR="$RIWORK_USER_ZDOTDIR"
    if [[ "${RIWORK_USER_ZDOTDIR_EXPORT:-0}" == 1 ]]; then
        'builtin' 'export' ZDOTDIR
    else
        'builtin' 'typeset' +x ZDOTDIR
    fi
else
    'builtin' 'unset' ZDOTDIR
fi
"#;
    let capture = r#"if [[ -n "${ZDOTDIR+x}" ]]; then
    'builtin' 'export' RIWORK_USER_ZDOTDIR="$ZDOTDIR" RIWORK_USER_ZDOTDIR_SET=1
    if [[ "${parameters[ZDOTDIR]}" == *export* ]]; then
        'builtin' 'export' RIWORK_USER_ZDOTDIR_EXPORT=1
    else
        'builtin' 'export' RIWORK_USER_ZDOTDIR_EXPORT=0
    fi
else
    'builtin' 'export' RIWORK_USER_ZDOTDIR='' RIWORK_USER_ZDOTDIR_SET=0 RIWORK_USER_ZDOTDIR_EXPORT=0
fi
"#;
    for stage in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
        let mut content = format!("# RiWork session-scoped zsh startup forwarding\n{restore}");
        if stage == ".zshrc" {
            // Apple's /etc/zshrc derives its default history location from
            // ZDOTDIR before this file runs. Preserve that original default.
            content.push_str(&format!(
                "if [[ \"${{HISTFILE-}}\" == {} ]]; then\n    HISTFILE=\"${{ZDOTDIR-$HOME}}/.zsh_history\"\nfi\n",
                quote_arg(&directory.join(".zsh_history").to_string_lossy())
            ));
        }
        content.push_str(&format!(
            "'builtin' 'typeset' _riwork_rc_path=\"${{ZDOTDIR-$HOME}}/{stage}\"\n\
             if [[ -r \"$_riwork_rc_path\" && ! -d \"$_riwork_rc_path\" ]]; then\n\
                 'builtin' 'source' '--' \"$_riwork_rc_path\"\n\
             fi\n'builtin' 'unset' _riwork_rc_path\n{capture}"
        ));
        let shim = quote_arg(&shim_directory.to_string_lossy());
        content.push_str(&format!(
            "if [[ \"$PATH\" != {shim}:* && \"$PATH\" != {shim} ]]; then\n    'builtin' 'export' PATH={shim}:\"$PATH\"\nfi\n"
        ));
        let later_files = match stage {
            ".zshenv" => Some("-o rcs && ( -o login || -o interactive )"),
            ".zprofile" | ".zshrc" => Some("-o rcs && -o login"),
            _ => None,
        };
        if let Some(condition) = later_files {
            content.push_str(&format!(
                "if [[ {condition} ]]; then\n    'builtin' 'export' ZDOTDIR={}\nfi\n",
                quote_arg(&directory.to_string_lossy())
            ));
        }
        let path = directory.join(stage);
        if fs::read(&path).is_ok_and(|existing| existing == content.as_bytes()) {
            continue;
        }
        let temporary = directory.join(format!(".{stage}-{}.tmp", Uuid::new_v4()));
        fs::write(&temporary, content)
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(format!("install {}: {error}", path.display()));
        }
    }
    Ok(directory)
}

fn find_harness_program(harness: HarnessKind, shim_directory: &Path) -> Option<PathBuf> {
    let shim_directory = shim_directory
        .canonicalize()
        .unwrap_or_else(|_| shim_directory.to_owned());
    executable_dirs().into_iter().find_map(|directory| {
        let candidate = directory.join(harness.program());
        let path = candidate.canonicalize().ok()?;
        if !path.is_file() || path.parent() == Some(shim_directory.as_path()) {
            return None;
        }
        // Ignore wrappers from another RiWork state directory as well. Read
        // only a small prefix, never an entire official CLI binary.
        use std::io::Read;
        let mut prefix = [0; 512];
        if let Ok(mut file) = File::open(&path) {
            if let Ok(length) = file.read(&mut prefix) {
                if String::from_utf8_lossy(&prefix[..length]).contains("# RiWork Cua harness shim")
                {
                    return None;
                }
            }
        }
        Some(path)
    })
}

fn harness_utility_invocation(harness: HarnessKind, arguments: &[String]) -> bool {
    if arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| matches!(argument.as_str(), "--help" | "-h" | "--version" | "-V"))
    {
        return true;
    }
    if harness == HarnessKind::Claude && arguments.first().is_some_and(|argument| argument == "-v")
    {
        return true;
    }
    let first = if harness == HarnessKind::Codex {
        codex_first_command(arguments).map(|(command, _)| command)
    } else {
        arguments.first().map(String::as_str)
    };
    let Some(first) = first else {
        return false;
    };
    match harness {
        HarnessKind::Codex => matches!(
            first,
            "help"
                | "login"
                | "logout"
                | "mcp"
                | "plugin"
                | "features"
                | "doctor"
                | "completion"
                | "update"
                | "app-server"
        ),
        HarnessKind::Claude => matches!(
            first,
            "help"
                | "auth"
                | "login"
                | "logout"
                | "mcp"
                | "plugin"
                | "plugins"
                | "doctor"
                | "install"
                | "setup-token"
                | "update"
                | "upgrade"
        ),
    }
}

/// Entry point for the managed PATH wrappers. Preserve arguments as argv,
/// including supplied prompts, and replace this process with the real CLI.
fn cua_proxy_arguments(
    harness: HarnessKind,
    arguments: &[String],
    executable: &Path,
    home: &Path,
    shell_id: Option<&str>,
    codex_home: Option<&Path>,
) -> Vec<String> {
    if harness_utility_invocation(harness, arguments) {
        return arguments.to_vec();
    }
    let mut additions = cua_harness_arguments(harness, executable, home);
    if harness == HarnessKind::Codex {
        additions.extend(codex_shell_environment_arguments(home, shell_id));
        additions.extend(codex_activity_arguments(
            executable, home, shell_id, arguments, codex_home,
        ));
        if let Some(home) = codex_home {
            additions.extend(codex_account_environment_arguments(home));
        }
    }
    let mut result = match harness {
        HarnessKind::Codex => {
            // Codex combines global and subcommand config tables by replacing
            // nested tables. Put Cua options in the innermost supplied command
            // so local -c flags cannot discard the driver's transport fields.
            let separator = arguments
                .iter()
                .position(|argument| argument == "--")
                .unwrap_or(arguments.len());
            let mut result = arguments[..separator].to_vec();
            result.extend(additions);
            result.extend_from_slice(&arguments[separator..]);
            result
        }
        HarnessKind::Claude => {
            let mut result = additions;
            result.extend_from_slice(arguments);
            result
        }
    };
    if arguments.is_empty() && harness == HarnessKind::Codex {
        result.push(cua_startup_prompt());
    }
    result
}

/// Identify a subcommand without interpreting a prompt or flag value as one.
fn codex_first_command(arguments: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" {
            return None;
        }
        if matches!(
            argument.as_str(),
            "-c" | "--config"
                | "-p"
                | "--profile"
                | "-m"
                | "--model"
                | "-C"
                | "--cd"
                | "-i"
                | "--image"
                | "--add-dir"
                | "--remote"
                | "--remote-id"
                | "-a"
                | "--ask-for-approval"
                | "-s"
                | "--sandbox"
                | "--enable"
                | "--disable"
                | "--local-provider"
                | "--output-schema"
                | "--output-last-message"
                | "-o"
                | "--color"
                | "--plugin-dir"
        ) {
            index += 2;
            continue;
        }
        index += 1;
        if argument.starts_with('-') {
            continue;
        }
        return Some((argument, &arguments[index..]));
    }
    None
}

/// A fork also starts from an existing account's persisted thread.
fn codex_resumes_existing(arguments: &[String]) -> bool {
    match codex_first_command(arguments) {
        Some(("resume" | "fork", _)) => true,
        Some(("exec" | "e", remaining)) => codex_resumes_existing(remaining),
        _ => false,
    }
}

/// A managed agent and its tool/delegated children always retain their account.
/// A plain shell's next new Codex invocation follows the current preference.
fn proxy_uses_frozen_account(arguments: &[String], pinned_home: Option<&Path>) -> bool {
    pinned_home.is_some() || codex_resumes_existing(arguments)
}

fn legacy_codex_child(shell_id: Option<&str>, thread_shell_id: Option<&str>) -> bool {
    shell_id
        .zip(thread_shell_id)
        .is_some_and(|(shell, thread)| {
            !shell.is_empty() && shell == thread && validate_uuid(shell).is_ok()
        })
}

fn codex_proxy_binding(
    state_home: &Path,
    arguments: &[String],
    pinned_home: Option<&Path>,
    saved: Option<&ShellSession>,
) -> Result<crate::codex_accounts::CodexAccountBinding, String> {
    if proxy_uses_frozen_account(arguments, pinned_home) {
        let home = pinned_home
            .map(Path::to_path_buf)
            .or_else(|| saved.and_then(|session| session.codex_home.clone()));
        if let Some(home) = home {
            let saved = saved.filter(|session| session.codex_home.as_ref() == Some(&home));
            return Ok(crate::codex_accounts::CodexAccountBinding {
                home,
                id: saved.and_then(|session| session.codex_account_id.clone()),
                label: saved.and_then(|session| session.codex_account_label.clone()),
                email: saved.and_then(|session| session.codex_account_email.clone()),
            });
        }
        // Legacy resume requests retain their explicit or inherited environment.
        return crate::codex_accounts::resolve_launch_binding(state_home, None);
    }
    selected_codex_binding(
        state_home,
        saved.and_then(|session| session.project_id.as_deref()),
    )
}

pub fn run_cua_harness(harness: HarnessKind, arguments: &[String]) -> Result<(), String> {
    let home = match env::var_os("RIWORK_HOME") {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from(env::var_os("HOME").ok_or("HOME is not set; set RIWORK_HOME")?)
            .join(".local/share/riwork"),
    };
    let cua = crate::cua::CuaManager::at(home.clone())?;
    let home = home
        .canonicalize()
        .map_err(|error| format!("resolve RiWork state: {error}"))?;
    let executable =
        env::current_exe().map_err(|error| format!("resolve RiWork executable: {error}"))?;
    let shim_directory = cua.ensure_harness_shims(&executable)?;
    let program = find_harness_program(harness, &shim_directory).ok_or_else(|| {
        format!(
            "{} is not installed or is not on PATH outside RiWork's Cua wrappers",
            harness.program()
        )
    })?;
    let mut command = Command::new(program);
    if !harness_utility_invocation(harness, arguments) {
        cua.driver_path()?;
    }
    let shell_id =
        if harness == HarnessKind::Codex && !harness_utility_invocation(harness, arguments) {
            match env::var("RIWORK_SHELL_ID") {
                Ok(value) => Some(value),
                Err(env::VarError::NotPresent) => None,
                Err(env::VarError::NotUnicode(_)) => {
                    return Err("RIWORK_SHELL_ID is not valid UTF-8".to_owned());
                }
            }
        } else {
            None
        };
    let pinned_home = env::var_os("RIWORK_CODEX_ACCOUNT_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let account =
        if harness == HarnessKind::Codex && !harness_utility_invocation(harness, arguments) {
            let manager = shell_id
                .as_ref()
                .map(|_| SessionManager::at(home.clone()))
                .transpose()?;
            let saved = manager
                .as_ref()
                .zip(shell_id.as_ref())
                .map(|(manager, id)| manager.get(id))
                .transpose()?;
            let legacy_child = legacy_codex_child(
                shell_id.as_deref(),
                env::var("RIWORK_CODEX_SHELL_ID").ok().as_deref(),
            );
            let pinned_home = if pinned_home.is_none() && legacy_child {
                Some(
                    saved
                        .as_ref()
                        .and_then(|session| session.codex_home.clone())
                        .map(Ok)
                        .unwrap_or_else(|| {
                            crate::codex_accounts::resolve_launch_binding(&home, None)
                                .map(|binding| binding.home)
                        })?,
                )
            } else {
                pinned_home.clone()
            };
            let binding =
                codex_proxy_binding(&home, arguments, pinned_home.as_deref(), saved.as_ref())?;
            if legacy_child {
                if let Some((manager, id)) = manager.as_ref().zip(shell_id.as_ref()) {
                    if binding.home.is_dir() {
                        manager.freeze_codex_home_if_unknown(id, &binding.home)?;
                    }
                }
            }
            if pinned_home.is_none() {
                if let Some((manager, id)) = manager.as_ref().zip(shell_id.as_ref()) {
                    manager.record_codex_launch(
                        id,
                        &binding,
                        arguments,
                        codex_resumes_existing(arguments),
                    )?;
                }
            }
            command
                .env("CODEX_HOME", &binding.home)
                .env("RIWORK_CODEX_ACCOUNT_HOME", &binding.home);
            Some(binding)
        } else {
            None
        };
    command
        .args(cua_proxy_arguments(
            harness,
            arguments,
            &executable,
            &home,
            shell_id.as_deref(),
            account.as_ref().map(|binding| binding.home.as_path()),
        ))
        .env("RIWORK_HOME", &home);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(format!("launch {}: {}", harness.program(), command.exec()))
    }
    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .map_err(|error| format!("launch {}: {error}", harness.program()))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{} exited with {status}", harness.program()))
        }
    }
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

fn schedule_empty_prompt_at(
    harness: Option<HarnessKind>,
    screen: &str,
    cursor_row: usize,
    cursor_column: usize,
) -> bool {
    let raw_line = screen.lines().nth(cursor_row).unwrap_or("");
    let plain = strip_schedule_sgr(screen);
    let line = plain.lines().nth(cursor_row).unwrap_or("").trim();
    let expected = match harness {
        Some(HarnessKind::Codex) => "›",
        Some(HarnessKind::Claude) => "❯",
        None => return false,
    };
    // Codex paints its empty composer placeholder dim, at the initial cursor.
    // A typed lookalike without the dim span is a draft and must never be sent.
    let codex_placeholder = harness == Some(HarnessKind::Codex)
        && cursor_column == 2
        && line == "› Ask Codex to do anything"
        && raw_line.contains("\x1b[2mAsk Codex to do anything\x1b[");
    if line != expected && !codex_placeholder {
        return false;
    }
    if cursor_column != 2 {
        return false;
    }
    // Everything above the cursor's composer may be completed conversation,
    // including quoted login/approval screens. It is not readiness evidence.
    // Ongoing tasks are gated by the exact structured lifecycle in send_scheduled.
    // Only current controls/status below this composer can additionally veto it.
    !plain.lines().skip(cursor_row + 1).any(|line| {
        let status = line.trim().to_lowercase();
        status.contains("esc to interrupt")
            || status.contains("esc to cancel")
            || [
                "approval required",
                "allow once",
                "sign in to continue",
                "log in to continue",
                "do you trust this",
                "trust this directory",
                "yes, proceed",
                "select an option",
            ]
            .iter()
            .any(|control| status.starts_with(control))
    })
}
fn strip_schedule_sgr(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn editor_command_passes_unicode_and_metacharacters_as_one_file_name() {
        use std::os::unix::fs::PermissionsExt;
        let directory = env::temp_dir().join(format!("riwork-editor-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let vim = directory.join("fake vim");
        let capture = directory.join("argv");
        let file = directory.join("café ' $(touch PWNED) ;.txt");
        fs::write(&file, "sample\n").unwrap();
        fs::write(&vim, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CAPTURE\"\n").unwrap();
        fs::set_permissions(&vim, fs::Permissions::from_mode(0o700)).unwrap();
        let result = Command::new("/bin/sh")
            .arg("-c")
            .arg(editor_command(&vim, &file).unwrap())
            .current_dir(&directory)
            .env("CAPTURE", &capture)
            .status()
            .unwrap();
        assert!(result.success());
        assert_eq!(
            fs::read_to_string(&capture).unwrap(),
            format!("--\n{}\n", file.display())
        );
        assert!(!directory.join("PWNED").exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn directory_sampling_batches_live_shells_and_uses_the_owned_pane() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = AccountFixture::new();
        let alpha = "00000000-0000-4000-8000-000000000010";
        let beta = "00000000-0000-4000-8000-000000000011";
        let dead = "00000000-0000-4000-8000-000000000012";
        let panes = fixture.0.join("panes");
        let calls = fixture.0.join("calls");
        let tmux = fixture.0.join("fake-tmux");
        fs::write(
            &panes,
            format!(
                "{alpha}\t0\t0\t/project with spaces\n\
                 {alpha}\t0\t1\t/other-pane\n\
                 {beta}\t0\t0\t/project\twith-tab\n\
                 {beta}\t1\t0\t/other-window\n\
                 {dead}\t0\t0\t/dead-shell\n\
                 unregistered\t0\t0\t/unregistered-shell\n\
                 incomplete\n"
            ),
        )
        .unwrap();
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\nprintf 'called\\n' >> {}\ncat {}\n",
                quote_arg(&calls.to_string_lossy()),
                quote_arg(&panes.to_string_lossy())
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let manager = SessionManager {
            home: fixture.0.clone(),
            tmux,
            socket_name: "isolated-fake".into(),
        };
        let shells = [(alpha, true), (beta, true), (dead, false)].map(|(id, alive)| {
            let mut shell = scope_session(ShellKind::Project, None);
            shell.id = id.into();
            shell.alive = alive;
            shell
        });
        assert_eq!(
            manager.current_directories(&shells).unwrap(),
            BTreeMap::from([
                (alpha.into(), PathBuf::from("/project with spaces")),
                (beta.into(), PathBuf::from("/project\twith-tab")),
            ])
        );
        assert_eq!(fs::read_to_string(&calls).unwrap(), "called\n");
        assert!(manager.current_directories(&[]).unwrap().is_empty());
        assert_eq!(fs::read_to_string(&calls).unwrap(), "called\n");
    }

    struct AccountFixture(PathBuf);

    impl AccountFixture {
        fn new() -> Self {
            if let Some(root) = env::var_os("RIWORK_TEST_ACCOUNT_FIXTURE") {
                return Self(PathBuf::from(root));
            }
            let root = env::temp_dir().join(format!("riwork-account-launch-{}", Uuid::new_v4()));
            fs::create_dir_all(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
        /// Only the isolated child sees the fake Orca profile override. This
        /// preserves the real cache's profile validation and avoids mutating
        /// environment variables shared with other tests or user processes.
        fn run_in_child(&self, name: &str) -> bool {
            if env::var_os("RIWORK_TEST_ACCOUNT_FIXTURE").is_some() {
                return true;
            }
            let output = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    &format!("sessions::tests::{name}"),
                    "--nocapture",
                ])
                .env("RIWORK_TEST_ACCOUNT_FIXTURE", &self.0)
                .env(
                    "ORCA_USER_DATA_PATH",
                    self.0.join("Orca's `profiles` $(touch injected)"),
                )
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            false
        }

        fn selected(&self, id: &str) -> PathBuf {
            let state = self.0.join("state");
            let user_data = self.0.join("Orca's `profiles` $(touch injected)");
            for account in ["account-a", "account-b"] {
                fs::create_dir_all(user_data.join("codex-accounts").join(account).join("home"))
                    .unwrap();
            }
            fs::create_dir_all(&state).unwrap();
            fs::write(
                state.join("codex-accounts.json"),
                serde_json::to_vec(&serde_json::json!({
                    "version": 1,
                    "user_data": user_data,
                    "accounts": [
                        {"id":"account-a","email":"a@example.test","managedHomeRuntime":"host"},
                        {"id":"account-b","email":"b@example.test","managedHomeRuntime":"host"}
                    ],
                    "source_active_id": "account-a"
                }))
                .unwrap(),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    state.join("codex-accounts.json"),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            crate::settings::SettingsStore::open(&state)
                .unwrap()
                .update(|settings| {
                    settings.selected_codex_account = Some(id.to_owned());
                })
                .unwrap();
            state
        }
    }
    impl Drop for AccountFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn project_accounts_override_app_choice_without_rebinding_existing_sessions() {
        use crate::store::{ProjectCodexAccount, Store};
        let fixture = AccountFixture::new();
        if !fixture.run_in_child(
            "project_accounts_override_app_choice_without_rebinding_existing_sessions",
        ) {
            return;
        }
        let state = fixture.selected("account-b");
        let store = Store::open(&state).unwrap();
        let alpha_root = fixture.0.join("alpha");
        let beta_root = fixture.0.join("beta");
        fs::create_dir_all(&alpha_root).unwrap();
        fs::create_dir_all(&beta_root).unwrap();
        let alpha = store.add_project(&alpha_root, Some("Alpha")).unwrap();
        let beta = store.add_project(&beta_root, Some("Beta")).unwrap();
        let account_a =
            crate::codex_accounts::resolve_launch_binding(&state, Some("account-a")).unwrap();
        let account_b =
            crate::codex_accounts::resolve_launch_binding(&state, Some("account-b")).unwrap();
        assert_eq!(
            selected_codex_binding(&state, Some(&alpha.id)).unwrap(),
            account_b
        );
        assert_eq!(selected_codex_binding(&state, None).unwrap(), account_b);
        store
            .set_project_codex_account(&alpha.id, ProjectCodexAccount::Saved("account-a".into()))
            .unwrap();
        store
            .set_project_codex_account(&beta.id, ProjectCodexAccount::SystemDefault)
            .unwrap();
        assert_eq!(
            selected_codex_binding(&state, Some(&alpha.id)).unwrap(),
            account_a
        );
        assert_eq!(
            selected_codex_binding(&state, Some(&beta.id)).unwrap().id,
            None
        );
        assert_eq!(selected_codex_binding(&state, None).unwrap(), account_b);

        let mut plain = scope_session(ShellKind::Project, Some(&alpha.id));
        plain.codex_account_id = account_b.id.clone();
        plain.codex_account_label = account_b.label.clone();
        plain.codex_account_email = account_b.email.clone();
        plain.codex_home = Some(account_b.home.clone());
        assert_eq!(
            codex_proxy_binding(&state, &[], None, Some(&plain)).unwrap(),
            account_a
        );
        assert_eq!(
            codex_proxy_binding(&state, &[], Some(&account_b.home), Some(&plain)).unwrap(),
            account_b
        );
        assert_eq!(
            codex_proxy_binding(&state, &["resume".into()], None, Some(&plain)).unwrap(),
            account_b
        );

        crate::settings::SettingsStore::open(&state)
            .unwrap()
            .update(|settings| {
                settings.selected_codex_account = Some("missing".into());
            })
            .unwrap();
        assert!(selected_codex_binding(&state, None).is_err());
        assert_eq!(
            selected_codex_binding(&state, Some(&alpha.id)).unwrap(),
            account_a
        );
        assert_eq!(
            selected_codex_binding(&state, Some(&beta.id)).unwrap().id,
            None
        );
        store
            .set_project_codex_account(&beta.id, ProjectCodexAccount::Inherit)
            .unwrap();
        assert!(selected_codex_binding(&state, Some(&beta.id)).is_err());
        assert_eq!(plain.codex_home, Some(account_b.home));

        fs::remove_dir_all(&account_a.home).unwrap();
        assert_eq!(
            selected_codex_binding(&state, Some(&alpha.id)).unwrap_err(),
            "This saved account's home is missing. Restore or sign in through Orca."
        );
        assert_eq!(plain.codex_account_id.as_deref(), Some("account-b"));
    }

    #[test]
    #[cfg(unix)]
    fn plain_project_shell_receives_initial_home_without_pin() {
        use crate::store::{ProjectCodexAccount, Store};
        use std::os::unix::fs::PermissionsExt;
        let fixture = AccountFixture::new();
        if !fixture.run_in_child("plain_project_shell_receives_initial_home_without_pin") {
            return;
        }
        let state = fixture.selected("account-b");
        let project_root = fixture.0.join("plain");
        fs::create_dir_all(&project_root).unwrap();
        let store = Store::open(&state).unwrap();
        let project = store.add_project(&project_root, Some("Plain")).unwrap();
        store
            .set_project_codex_account(&project.id, ProjectCodexAccount::Saved("account-a".into()))
            .unwrap();
        let account_a = selected_codex_binding(&state, Some(&project.id)).unwrap();
        let tmux = fixture.0.join("fake-tmux-plain");
        let capture = fixture.0.join("plain-tmux-argv");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {}\n",
                quote_arg(&capture.to_string_lossy())
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let manager = SessionManager {
            home: state,
            tmux,
            socket_name: "isolated-plain".into(),
        };
        let shell = manager
            .new_tmux_session(
                Uuid::new_v4().to_string(),
                Some(project.id),
                None,
                ShellKind::Project,
                project_root,
                None,
                None,
                false,
                None,
            )
            .unwrap();
        assert_eq!(shell.harness, None);
        assert_eq!(shell.codex_home, None);
        let arguments = fs::read_to_string(capture).unwrap();
        assert!(
            arguments
                .lines()
                .any(|argument| argument == format!("CODEX_HOME={}", account_a.home.display()))
        );
        assert!(
            arguments
                .lines()
                .any(|argument| argument == "RIWORK_CODEX_ACCOUNT_HOME=")
        );
    }

    #[test]
    fn plain_shell_new_launch_selects_b_while_managed_children_and_resume_keep_a() {
        let fixture = AccountFixture::new();
        if !fixture.run_in_child(
            "plain_shell_new_launch_selects_b_while_managed_children_and_resume_keep_a",
        ) {
            return;
        }
        let state = fixture.selected("account-b");
        let binding_a =
            crate::codex_accounts::resolve_launch_binding(&state, Some("account-a")).unwrap();
        let mut saved = scope_session(ShellKind::Project, None);
        saved.codex_account_id = binding_a.id.clone();
        saved.codex_account_label = binding_a.label.clone();
        saved.codex_account_email = binding_a.email.clone();
        saved.codex_home = Some(binding_a.home.clone());
        let new = codex_proxy_binding(&state, &[], None, Some(&saved)).unwrap();
        assert_eq!(new.id.as_deref(), Some("account-b"));
        assert_ne!(new.home, binding_a.home);
        let child = codex_proxy_binding(
            &state,
            &["exec".into(), "new work".into()],
            Some(&binding_a.home),
            Some(&saved),
        )
        .unwrap();
        assert_eq!(child, binding_a);
        for command in ["resume", "fork"] {
            let resumed = codex_proxy_binding(
                &state,
                &[
                    "--profile".into(),
                    "work".into(),
                    command.into(),
                    Uuid::new_v4().to_string(),
                ],
                None,
                Some(&saved),
            )
            .unwrap();
            assert_eq!(resumed, binding_a);
        }
        crate::settings::SettingsStore::open(&state)
            .unwrap()
            .update(|settings| settings.selected_codex_account = Some("missing".into()))
            .unwrap();
        assert!(codex_proxy_binding(&state, &[], None, Some(&saved)).is_err());
        assert_eq!(
            codex_proxy_binding(&state, &[], Some(&binding_a.home), Some(&saved)).unwrap(),
            binding_a
        );
        assert!(!codex_resumes_existing(&[
            "--model".into(),
            "resume".into(),
            "new prompt".into()
        ]));
        assert!(!codex_resumes_existing(&["--".into(), "resume".into()]));
        assert!(legacy_codex_child(Some(&saved.id), Some(&saved.id)));
        assert!(!legacy_codex_child(Some(&saved.id), Some("different-pane")));
        assert!(!legacy_codex_child(Some(&saved.id), None));
        assert!(!legacy_codex_child(None, Some(&saved.id)));
        assert!(codex_resumes_existing(&[
            "exec".into(),
            "-c".into(),
            "name=resume".into(),
            "resume".into(),
            "--last".into()
        ]));
    }

    #[test]
    #[cfg(unix)]
    fn tmux_launch_receives_account_environment_and_registry_keeps_frozen_binding() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = AccountFixture::new();
        if !fixture.run_in_child(
            "tmux_launch_receives_account_environment_and_registry_keeps_frozen_binding",
        ) {
            return;
        }
        let state = fixture.selected("account-b");
        let binding = selected_codex_binding(&state, None).unwrap();
        let tmux = fixture.0.join("fake-tmux");
        let capture = fixture.0.join("tmux-argv");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {}\n",
                quote_arg(&capture.to_string_lossy())
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let manager = SessionManager {
            home: state,
            tmux,
            socket_name: "isolated-fake".into(),
        };
        let session = manager
            .new_tmux_session(
                Uuid::new_v4().to_string(),
                None,
                None,
                ShellKind::Project,
                fixture.0.clone(),
                Some("exec /fake/codex".into()),
                None,
                false,
                Some(binding.clone()),
            )
            .unwrap();
        assert_eq!(session.codex_home.as_ref(), Some(&binding.home));
        assert_eq!(session.codex_account_id, binding.id);
        let args = fs::read_to_string(capture).unwrap();
        assert!(
            args.lines()
                .any(|argument| argument == format!("CODEX_HOME={}", binding.home.display()))
        );
        assert!(
            args.lines().any(|argument| argument
                == format!("RIWORK_CODEX_ACCOUNT_HOME={}", binding.home.display()))
        );
        let argv = shell_arguments(session.command.as_deref().unwrap());
        assert!(argv.contains(&format!("CODEX_HOME={}", binding.home.display())));
        assert!(!fixture.0.join("injected").exists());
        let restored: ShellSession =
            serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
        assert_eq!(restored.codex_home, session.codex_home);
        assert_eq!(restored.codex_account_id, session.codex_account_id);
    }

    #[test]
    fn notification_freezes_legacy_home_without_rebinding_an_existing_account() {
        let fixture = AccountFixture::new();
        if !fixture
            .run_in_child("notification_freezes_legacy_home_without_rebinding_an_existing_account")
        {
            return;
        }
        let state = fixture.selected("account-b");
        let binding_a =
            crate::codex_accounts::resolve_launch_binding(&state, Some("account-a")).unwrap();
        let binding_b = selected_codex_binding(&state, None).unwrap();
        let manager = SessionManager {
            home: state,
            tmux: PathBuf::from("/unused/tmux"),
            socket_name: "unused".into(),
        };
        let legacy = scope_session(ShellKind::Project, None);
        manager
            .write_registry(&Registry {
                sessions: vec![legacy.clone()],
            })
            .unwrap();
        manager
            .freeze_codex_home_if_unknown(&legacy.id, &binding_a.home)
            .unwrap();
        let frozen = manager.read_registry().unwrap().sessions.remove(0);
        assert_eq!(frozen.codex_home, Some(binding_a.home.clone()));
        assert_eq!(frozen.codex_account_id, None);
        manager
            .freeze_codex_home_if_unknown(&legacy.id, &binding_b.home)
            .unwrap();
        assert_eq!(
            manager.read_registry().unwrap().sessions[0].codex_home,
            Some(binding_a.home)
        );
    }

    #[test]
    fn plain_shell_permission_mode_ignores_prompt_tokens_and_survives_resume() {
        let fixture = AccountFixture::new();
        let manager = SessionManager {
            home: fixture.0.clone(),
            tmux: PathBuf::from("/unused/tmux"),
            socket_name: "unused".into(),
        };
        let session = scope_session(ShellKind::Project, None);
        manager
            .write_registry(&Registry {
                sessions: vec![session.clone()],
            })
            .unwrap();
        let binding = crate::codex_accounts::CodexAccountBinding {
            id: Some("account-a".into()),
            label: None,
            email: None,
            home: fixture.0.clone(),
        };
        manager
            .record_codex_launch(
                &session.id,
                &binding,
                &["--".into(), "--yolo".into()],
                false,
            )
            .unwrap();
        assert!(!manager.read_registry().unwrap().sessions[0].unrestricted);
        manager
            .record_codex_launch(&session.id, &binding, &["--yolo".into()], false)
            .unwrap();
        assert!(manager.read_registry().unwrap().sessions[0].unrestricted);
        manager
            .record_codex_launch(
                &session.id,
                &binding,
                &["resume".into(), "--last".into()],
                true,
            )
            .unwrap();
        assert!(manager.read_registry().unwrap().sessions[0].unrestricted);
    }

    #[test]
    #[cfg(unix)]
    fn official_harness_receives_literal_selected_home_even_with_a_stale_environment() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = AccountFixture::new();
        if !fixture.run_in_child(
            "official_harness_receives_literal_selected_home_even_with_a_stale_environment",
        ) {
            return;
        }
        let state = fixture.selected("account-b");
        let binding = selected_codex_binding(&state, None).unwrap();
        let program = fixture.0.join("fake codex's CLI");
        fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' \"$CODEX_HOME\" \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let command = harness_command(
            HarnessKind::Codex,
            false,
            &program,
            Path::new("/fake/riwork"),
            &state,
            "uuid",
            Some(&binding.home),
        )
        .unwrap();
        let result = Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .env("CODEX_HOME", "stale profile")
            .current_dir(&fixture.0)
            .output()
            .unwrap();
        assert!(result.status.success());
        let output = String::from_utf8(result.stdout).unwrap();
        assert_eq!(
            output.lines().next().unwrap(),
            binding.home.to_string_lossy()
        );
        let arguments = shell_arguments(&command);
        assert!(arguments.contains(&format!(
            "shell_environment_policy.set.CODEX_HOME={}",
            toml_string(&binding.home.to_string_lossy())
        )));
        assert!(arguments.contains(&format!(
            "shell_environment_policy.set.RIWORK_CODEX_ACCOUNT_HOME={}",
            toml_string(&binding.home.to_string_lossy())
        )));
        assert!(!fixture.0.join("injected").exists());
    }

    #[test]
    fn account_probe_and_auth_utilities_do_not_choose_a_managed_account() {
        for arguments in [
            vec!["app-server".into(), "--stdio".into()],
            vec!["login".into(), "status".into()],
            vec!["logout".into()],
            vec![
                "--profile".into(),
                "work".into(),
                "login".into(),
                "status".into(),
            ],
        ] {
            assert!(harness_utility_invocation(HarnessKind::Codex, &arguments));
            assert_eq!(
                cua_proxy_arguments(
                    HarnessKind::Codex,
                    &arguments,
                    Path::new("/fake/riwork"),
                    Path::new("/fake/state"),
                    None,
                    Some(Path::new("/another/account"))
                ),
                arguments
            );
        }
    }

    #[test]
    fn activity_notification_preserves_user_profile_and_command_line_hooks() {
        let temporary = env::temp_dir().join(format!("riwork-notify-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&temporary).unwrap();
        let shell_id = Uuid::new_v4().to_string();
        let executable = Path::new("/Applications/RiWork's App/riwork");
        let home = temporary.join("state");
        let arguments =
            codex_activity_arguments_at(executable, &home, Some(&shell_id), &[], &temporary, &[]);
        assert_eq!(arguments[0], "-c");
        let notification: Vec<String> =
            serde_json::from_str(arguments[1].strip_prefix("notify=").unwrap()).unwrap();
        assert_eq!(
            notification,
            [
                executable.to_string_lossy().into_owned(),
                "agent-notify".into(),
                home.to_string_lossy().into_owned(),
                shell_id.clone()
            ]
        );
        let configured = vec!["-c".to_owned(), "notify=[\"user-hook\"]".to_owned()];
        assert!(
            codex_activity_arguments_at(
                executable,
                &home,
                Some(&shell_id),
                &configured,
                &temporary,
                &[]
            )
            .is_empty()
        );
        let global = temporary.join("config.toml");
        fs::write(&global, "notify = [\"user-global-hook\"]\n").unwrap();
        let before = fs::read(&global).unwrap();
        assert!(
            codex_activity_arguments_at(executable, &home, Some(&shell_id), &[], &temporary, &[])
                .is_empty()
        );
        assert_eq!(fs::read(&global).unwrap(), before);
        fs::remove_file(global).unwrap();
        let profile = temporary.join("work.config.toml");
        fs::write(&profile, "notify = [\"user-profile-hook\"]\n").unwrap();
        assert!(
            codex_activity_arguments_at(executable, &home, Some(&shell_id), &[], &temporary, &[])
                .is_empty()
        );
        fs::remove_file(profile).unwrap();
        let managed = temporary.join("managed.plist");
        fs::write(&managed, b"binary-managed-settings").unwrap();
        assert!(
            codex_activity_arguments_at(
                executable,
                &home,
                Some(&shell_id),
                &[],
                &temporary,
                &[managed]
            )
            .is_empty()
        );
        assert!(
            codex_activity_arguments_at(executable, &home, None, &[], &temporary, &[]).is_empty()
        );
        fs::remove_dir_all(temporary).unwrap();
    }

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
        assert_eq!(session.codex_account_id, None);
        assert_eq!(session.codex_account_label, None);
        assert_eq!(session.codex_home, None);
        assert!(!session.alive);
    }

    #[test]
    fn codex_permission_bypass_requires_explicit_selection() {
        let program = Path::new("/Applications/Codex CLI/codex");
        let executable = Path::new("/Applications/RiWork's App/riwork");
        let home = Path::new("/Users/test/RiWork's State");
        for unrestricted in [false, true] {
            let command = harness_command(
                HarnessKind::Codex,
                unrestricted,
                program,
                executable,
                home,
                "uuid",
                None,
            )
            .unwrap();
            let arguments = shell_arguments(&command);
            assert_eq!(arguments[0], program.to_string_lossy());
            assert_eq!(
                arguments
                    .iter()
                    .any(|argument| argument == "--dangerously-bypass-approvals-and-sandbox"),
                unrestricted
            );
            assert_codex_cua_arguments(&arguments, executable, home);
            assert_codex_shell_environment(&arguments, home, "uuid");
            assert!(arguments.last().unwrap().contains(CUA_GUIDANCE));
            assert!(
                arguments
                    .last()
                    .unwrap()
                    .contains("wait for the user's objective")
            );
            assert!(!arguments.iter().any(|argument| argument == "--profile"));
        }
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
                telemetry,
                Path::new("/Users/test/RiWork's State"),
                id,
                None,
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
            let mcp_index = arguments
                .iter()
                .position(|arg| arg == "--mcp-config")
                .unwrap();
            let mcp: serde_json::Value = serde_json::from_str(&arguments[mcp_index + 1]).unwrap();
            assert_eq!(
                mcp["mcpServers"]["cua-driver"]["command"],
                telemetry.to_string_lossy().as_ref()
            );
            assert_eq!(
                mcp["mcpServers"]["cua-driver"]["args"],
                serde_json::json!(["cua", "mcp"])
            );
            assert_eq!(
                mcp["mcpServers"]["cua-driver"]["env"]["RIWORK_HOME"],
                "/Users/test/RiWork's State"
            );
            let prompt_index = arguments
                .iter()
                .position(|arg| arg == "--append-system-prompt")
                .unwrap();
            assert_eq!(arguments[prompt_index + 1], CUA_GUIDANCE);
            assert!(
                !arguments
                    .iter()
                    .any(|argument| argument == "--strict-mcp-config")
            );
            let settings: serde_json::Value =
                serde_json::from_str(&arguments[settings_index + 1]).unwrap();
            assert_eq!(settings["statusLine"]["type"], "command");
            for event in ["UserPromptSubmit", "Stop"] {
                let hook = settings["hooks"][event][0]["hooks"][0].as_object().unwrap();
                assert_eq!(hook["type"], "command");
                assert_eq!(
                    shell_arguments(hook["command"].as_str().unwrap()),
                    [
                        telemetry.to_string_lossy().into_owned(),
                        "agent-hook".into(),
                        "claude".into(),
                        "/Users/test/RiWork's State".into(),
                        id.into()
                    ]
                );
            }
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

    fn assert_codex_cua_arguments(arguments: &[String], executable: &Path, home: &Path) {
        let overrides = arguments
            .windows(2)
            .filter(|pair| pair[0] == "-c")
            .map(|pair| pair[1].as_str())
            .collect::<Vec<_>>();
        let value = |key: &str| {
            overrides
                .iter()
                .find_map(|value| value.strip_prefix(&format!("{key}=")))
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            serde_json::from_str::<String>(&value("mcp_servers.cua-driver.command")).unwrap(),
            executable.to_string_lossy()
        );
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&value("mcp_servers.cua-driver.args")).unwrap(),
            ["cua", "mcp"]
        );
        assert_eq!(
            serde_json::from_str::<String>(&value("mcp_servers.cua-driver.env.RIWORK_HOME"))
                .unwrap(),
            home.to_string_lossy()
        );
        assert_eq!(value("mcp_servers.cua-driver.enabled"), "true");
        assert_eq!(value("mcp_servers.cua-driver.required"), "true");
        assert_eq!(value("mcp_servers.cua-driver.startup_timeout_sec"), "120");
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--disable", "computer_use"])
        );
        assert!(
            !cua_harness_arguments(HarnessKind::Codex, executable, home)
                .iter()
                .any(|value| value.starts_with("model=")
                    || value.starts_with("developer_instructions="))
        );
    }

    fn assert_codex_shell_environment(arguments: &[String], home: &Path, shell_id: &str) {
        for (key, expected) in [
            ("RIWORK_HOME", home.to_string_lossy().as_ref()),
            ("RIWORK_SHELL_ID", shell_id),
            ("RIWORK_CODEX_SHELL_ID", shell_id),
        ] {
            let prefix = format!("shell_environment_policy.set.{key}=");
            let values = arguments
                .windows(2)
                .filter(|pair| pair[0] == "-c")
                .filter_map(|pair| pair[1].strip_prefix(&prefix))
                .collect::<Vec<_>>();
            let actual = serde_json::from_str::<String>(values.last().unwrap()).unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn cua_configuration_survives_shell_and_toml_quoting() {
        let executable = Path::new("/Applications/RiWork's $App/\"quoted\"/riwork");
        let home = Path::new("/Users/test/line\nbreak\\state $(false)");
        for harness in [HarnessKind::Codex, HarnessKind::Claude] {
            let command = harness_command(
                harness,
                false,
                Path::new("/opt/bin/cli"),
                executable,
                home,
                "uuid",
                None,
            )
            .unwrap();
            let arguments = shell_arguments(&command);
            if harness == HarnessKind::Codex {
                assert_codex_cua_arguments(&arguments, executable, home);
                assert_codex_shell_environment(&arguments, home, "uuid");
            } else {
                let index = arguments
                    .iter()
                    .position(|arg| arg == "--mcp-config")
                    .unwrap();
                let config: serde_json::Value =
                    serde_json::from_str(&arguments[index + 1]).unwrap();
                assert_eq!(
                    config["mcpServers"]["cua-driver"]["command"],
                    executable.to_string_lossy().as_ref()
                );
                assert_eq!(
                    config["mcpServers"]["cua-driver"]["env"]["RIWORK_HOME"],
                    home.to_string_lossy().as_ref()
                );
            }
        }
    }

    #[test]
    fn managed_wrappers_pass_utilities_through_but_integrate_sessions() {
        for harness in [HarnessKind::Codex, HarnessKind::Claude] {
            for argument in ["--help", "--version", "mcp", "login"] {
                assert!(harness_utility_invocation(harness, &[argument.to_owned()]));
            }
            for arguments in [
                vec![],
                vec!["--resume".to_owned(), "session".to_owned()],
                vec!["implement this".to_owned()],
            ] {
                assert!(!harness_utility_invocation(harness, &arguments));
            }
        }
        for argument in ["exec", "review", "resume", "fork"] {
            assert!(!harness_utility_invocation(
                HarnessKind::Codex,
                &[argument.to_owned()]
            ));
        }
    }

    #[test]
    fn codex_proxy_places_mcp_options_with_local_subcommand_overrides() {
        let executable = Path::new("/Applications/RiWork's App/riwork");
        let home = Path::new("/Users/test/RiWork State");
        for original in [
            vec![
                "exec",
                "-c",
                "mcp_servers.cua-driver.enabled_tools=[\"health_report\"]",
                "inspect health",
            ],
            vec![
                "exec",
                "resume",
                "session",
                "-c",
                "model=\"user-model\"",
                "continue the task",
            ],
            vec!["--model", "user-model", "a positional prompt"],
            vec!["exec", "--", "--literal prompt"],
            vec!["exec", "resume", "session", "--", "--help"],
        ] {
            let original = original.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let result = cua_proxy_arguments(
                HarnessKind::Codex,
                &original,
                executable,
                home,
                Some("frontend-pane"),
                None,
            );
            let separator = original
                .iter()
                .position(|argument| argument == "--")
                .unwrap_or(original.len());
            let mut additions = cua_harness_arguments(HarnessKind::Codex, executable, home);
            additions.extend(codex_shell_environment_arguments(
                home,
                Some("frontend-pane"),
            ));
            assert_eq!(&result[..separator], &original[..separator]);
            assert_eq!(&result[separator..separator + additions.len()], additions);
            assert_eq!(
                &result[separator + additions.len()..],
                &original[separator..]
            );
            assert_codex_cua_arguments(&result, executable, home);
            assert_codex_shell_environment(&result, home, "frontend-pane");
            assert!(
                !result
                    .iter()
                    .any(|argument| argument.contains("wait for the user's objective"))
            );
        }
        for original in [
            vec!["mcp", "get", "cua-driver", "--json"],
            vec!["exec", "--help"],
        ] {
            let original = original.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(
                cua_proxy_arguments(
                    HarnessKind::Codex,
                    &original,
                    executable,
                    home,
                    Some("frontend-pane"),
                    None
                ),
                original
            );
        }
    }

    #[test]
    fn codex_proxy_binds_frontend_without_replacing_user_environment_settings() {
        let executable = Path::new("/Applications/RiWork/riwork");
        let home = Path::new("/Users/test/RiWork's \"State\"\n");
        let shell_id = "pane-'quoted'-\"id\"\n";
        let original = [
            "exec",
            "-c",
            "shell_environment_policy.set.USER_FLAG=\"keep-me\"",
            "-c",
            "shell_environment_policy.inherit=\"core\"",
            "-c",
            "shell_environment_policy.set.RIWORK_SHELL_ID=\"stale-pane\"",
            "inspect this",
        ]
        .map(str::to_owned);
        let result = cua_proxy_arguments(
            HarnessKind::Codex,
            &original,
            executable,
            home,
            Some(shell_id),
            None,
        );
        assert_eq!(&result[..original.len()], original);
        assert_codex_shell_environment(&result, home, shell_id);
        assert!(
            !result
                .iter()
                .any(|value| value == "shell_environment_policy.set={}")
        );
        let ownerless = cua_proxy_arguments(HarnessKind::Codex, &[], executable, home, None, None);
        assert_codex_shell_environment(&ownerless, home, "");
        let claude = cua_proxy_arguments(
            HarnessKind::Claude,
            &[],
            executable,
            home,
            Some(shell_id),
            None,
        );
        assert!(
            !claude
                .iter()
                .any(|value| value.starts_with("shell_environment_policy."))
        );
    }

    #[cfg(unix)]
    #[test]
    fn login_zsh_keeps_cua_wrappers_after_profile_path_changes() {
        use std::os::unix::fs::PermissionsExt;
        let Some(tmux) = find_tmux() else {
            return;
        };
        if !Path::new("/bin/zsh").is_file() {
            return;
        }
        struct IsolatedTmux {
            program: PathBuf,
            socket: String,
            directory: PathBuf,
        }
        impl IsolatedTmux {
            fn run(&self, arguments: &[String]) {
                let output = Command::new(&self.program)
                    .args(["-L", &self.socket, "-f", "/dev/null"])
                    .args(arguments)
                    .env_remove("TMUX")
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        impl Drop for IsolatedTmux {
            fn drop(&mut self) {
                let _ = Command::new(&self.program)
                    .args(["-L", &self.socket, "kill-server"])
                    .output();
                let _ = fs::remove_dir_all(&self.directory);
            }
        }
        let temporary = env::temp_dir().join(format!("riwork-zsh-cua-{}", Uuid::new_v4()));
        let test = IsolatedTmux {
            program: tmux,
            socket: format!("riwork-zsh-{}", Uuid::new_v4()),
            directory: temporary.clone(),
        };
        let managed = temporary.join("managed binaries");
        let official = temporary.join("official binaries");
        let initial = temporary.join("initial startup");
        let profile = temporary.join("dynamic profile");
        let final_directory = temporary.join("dynamic login");
        for directory in [&managed, &official, &initial, &profile, &final_directory] {
            fs::create_dir_all(directory).unwrap();
        }
        for (directory, label) in [(&managed, "managed"), (&official, "official")] {
            for harness in ["codex", "claude"] {
                let path = directory.join(harness);
                fs::write(
                    &path,
                    format!("#!/bin/sh\nprintf '%s:%s\\n' '{label}-{harness}' \"$1\"\n"),
                )
                .unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        fs::write(
            initial.join(".zshenv"),
            format!(
                "export RIWORK_TEST_STARTUP_STAGES=env\nZDOTDIR={}\n\
             riwork_test_ghostty_hook() {{ :; }}\nprecmd_functions+=(riwork_test_ghostty_hook)\n",
                quote_arg(&profile.to_string_lossy())
            ),
        )
        .unwrap();
        fs::write(profile.join(".zprofile"), format!(
            "export RIWORK_TEST_STARTUP_STAGES=\"$RIWORK_TEST_STARTUP_STAGES profile\"\nexport ZDOTDIR={}\nexport PATH={}:$PATH\n",
            quote_arg(&final_directory.to_string_lossy()), quote_arg(&official.to_string_lossy())
        )).unwrap();
        fs::write(final_directory.join(".zshrc"), format!(
            "export RIWORK_TEST_STARTUP_STAGES=\"$RIWORK_TEST_STARTUP_STAGES rc\"\nexport PATH={}:$PATH\n",
            quote_arg(&official.to_string_lossy())
        )).unwrap();
        fs::write(final_directory.join(".zlogin"), format!(
            "export RIWORK_TEST_STARTUP_STAGES=\"$RIWORK_TEST_STARTUP_STAGES login\"\nexport PATH={}:$PATH\n",
            quote_arg(&official.to_string_lossy())
        )).unwrap();
        let integration = install_zsh_startup_forwarding(&temporary, &managed).unwrap();
        test.run(&[
            "new-session".to_owned(),
            "-d".to_owned(),
            "-s".to_owned(),
            "anchor".to_owned(),
            "-e".to_owned(),
            format!("ZDOTDIR={}", initial.display()),
            "/bin/sleep 20".to_owned(),
        ]);
        test.run(&[
            "set-option".to_owned(),
            "-g".to_owned(),
            "default-shell".to_owned(),
            "/bin/zsh".to_owned(),
        ]);
        for fixed in [false, true] {
            let name = if fixed { "fixed" } else { "baseline" };
            let mut arguments = vec![
                "new-session".to_owned(),
                "-d".to_owned(),
                "-s".to_owned(),
                name.to_owned(),
                "-e".to_owned(),
                format!(
                    "PATH={}:{}:/usr/bin:/bin",
                    managed.display(),
                    official.display()
                ),
            ];
            let environment = if fixed {
                zsh_startup_environment(&integration, Some(initial.as_os_str()))
            } else {
                vec![("ZDOTDIR", initial.to_string_lossy().into_owned())]
            };
            for (variable, value) in environment {
                arguments.extend(["-e".to_owned(), format!("{variable}={value}")]);
            }
            test.run(&arguments);
            let output_file = temporary.join(format!("{name}.txt"));
            let script = format!(
                "{{ command -v codex; codex 'a b'; command -v claude; claude 'x$y'; \
                 printf '%s\\n' \"$ZDOTDIR\" \"$HISTFILE\" \"$RIWORK_TEST_STARTUP_STAGES\" \
                 \"${{precmd_functions[(I)riwork_test_ghostty_hook]}}\"; }} > {}; exit",
                quote_arg(&output_file.to_string_lossy())
            );
            test.run(&[
                "send-keys".to_owned(),
                "-t".to_owned(),
                format!("{name}:0.0"),
                "-l".to_owned(),
                script,
            ]);
            test.run(&[
                "send-keys".to_owned(),
                "-t".to_owned(),
                format!("{name}:0.0"),
                "Enter".to_owned(),
            ]);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
            let result = loop {
                if let Ok(result) = fs::read_to_string(&output_file) {
                    if result.lines().count() == 8 {
                        break result;
                    }
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "zsh startup test timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            let lines = result.lines().collect::<Vec<_>>();
            let expected_directory = if fixed { &managed } else { &official };
            let expected_label = if fixed { "managed" } else { "official" };
            assert_eq!(lines[0], expected_directory.join("codex").to_string_lossy());
            assert_eq!(lines[1], format!("{expected_label}-codex:a b"));
            assert_eq!(
                lines[2],
                expected_directory.join("claude").to_string_lossy()
            );
            assert_eq!(lines[3], format!("{expected_label}-claude:x$y"));
            assert_eq!(lines[4], final_directory.to_string_lossy());
            assert_eq!(
                lines[5],
                final_directory.join(".zsh_history").to_string_lossy()
            );
            assert_eq!(lines[6], "env profile rc login");
            assert_ne!(
                lines[7], "0",
                "existing Ghostty-style prompt hook must survive"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn zsh_forwarding_restores_unset_and_unexported_user_zdotdir() {
        if !Path::new("/bin/zsh").is_file() {
            return;
        }
        let temporary = env::temp_dir().join(format!("riwork-zsh-environment-{}", Uuid::new_v4()));
        fs::create_dir_all(&temporary).unwrap();
        let shim = temporary.join("managed");
        let original = temporary.join("original");
        fs::create_dir_all(&original).unwrap();
        let integration = install_zsh_startup_forwarding(&temporary, &shim).unwrap();
        for source in ["unset ZDOTDIR\n", "typeset +x ZDOTDIR\n"] {
            fs::write(original.join(".zshenv"), source).unwrap();
            let environment = zsh_startup_environment(&integration, Some(original.as_os_str()));
            // Noninteractive startup only reads .zshenv; it must restore the
            // exact user result even when no profile/rc/login file runs.
            let output = Command::new("/bin/zsh")
                .args([
                    "-c",
                    "printf '%s\\n' ${ZDOTDIR+x} ${parameters[ZDOTDIR]-unset}; command -v codex",
                ])
                .envs(environment)
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            let result = String::from_utf8(output.stdout).unwrap();
            if source.starts_with("unset") {
                assert!(result.starts_with("unset\n"), "{result:?}");
            } else {
                assert!(result.starts_with("x\nscalar\n"), "{result:?}");
            }
        }
        fs::remove_dir_all(temporary).unwrap();
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

    fn assert_codex_user_permissions_preserved(arguments: &[String]) {
        // The final argument is the skill prompt, which discusses permissions
        // but must not be mistaken for a CLI option or configuration override.
        let options = &arguments[..arguments.len() - 1];
        for argument in options {
            assert!(
                !matches!(
                    argument.as_str(),
                    "--add-dir"
                        | "--sandbox"
                        | "-s"
                        | "--ask-for-approval"
                        | "-a"
                        | "--approve-for-me"
                        | "--full-auto"
                        | "--dangerously-bypass-approvals-and-sandbox"
                        | "--yolo"
                ),
                "orchestrator must retain the user's permission policy: {argument}"
            );
        }
        for pair in options.windows(2).filter(|pair| pair[0] == "-c") {
            let key = pair[1].split('=').next().unwrap().trim();
            for permission_key in [
                "sandbox_mode",
                "sandbox_permissions",
                "sandbox_workspace_write",
                "approval_policy",
                "default_permissions",
                "permissions",
            ] {
                assert!(
                    key != permission_key && !key.starts_with(&format!("{permission_key}.")),
                    "orchestrator must retain the user's permission configuration: {key}"
                );
            }
        }
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
            "global-orchestrator-pane",
            None,
        );
        let arguments = shell_arguments(&command);
        assert_eq!(arguments[0], "/opt/bin/codex");
        assert_eq!(arguments[1], "--cd");
        assert_eq!(arguments[2], context.to_string_lossy());
        assert_codex_user_permissions_preserved(&arguments);
        assert_codex_cua_arguments(&arguments, executable, state_home);
        assert_codex_shell_environment(&arguments, state_home, "global-orchestrator-pane");
        let prompt = arguments.last().unwrap();
        assert!(prompt.starts_with("$riwork-orchestrator\n"));
        assert!(prompt.contains(ORCHESTRATOR_SKILL));
        assert!(prompt.contains(CUA_GUIDANCE));
        assert!(prompt.contains("wait for the user's objective"));
        assert!(prompt.contains(&quote_arg(&skill.to_string_lossy())));
        assert!(prompt.contains(&quote_arg(&executable.to_string_lossy())));
        assert!(prompt.contains("Scope: global."));
        assert!(prompt.contains("You have no project or worktree ownership."));
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
    fn project_orchestrator_preserves_root_scope_and_unrestricted_mode_across_serialization() {
        let project = "00000000-0000-4000-8000-000000000010";
        let mut session = scope_session(ShellKind::Orchestrator, Some(project));
        session.orchestrator_project_root = Some(PathBuf::from("/Users/test/wrapper project"));
        session.orchestrator_skill_loaded = true;
        session.orchestrator_skill_version = Some(orchestrator_skill_version());
        session.unrestricted = true;
        let bytes = serde_json::to_vec(&session).unwrap();
        let restored: ShellSession = serde_json::from_slice(&bytes).unwrap();
        assert!(matches_orchestrator_scope(&restored, Some(project)));
        assert!(!matches_orchestrator_scope(&restored, None));
        assert_eq!(restored.worktree_id, None);
        assert!(restored.unrestricted);
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
    fn project_context_prompt_environment_and_unrestricted_mode_are_distinct_from_global() {
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
            "project-orchestrator-pane",
            None,
        );
        let arguments = shell_arguments(&command);
        assert_eq!(arguments[0], "/opt/bin/codex");
        assert_eq!(arguments[1], "--cd");
        assert_eq!(arguments[2], context.to_string_lossy());
        assert_eq!(
            arguments
                .iter()
                .filter(|argument| argument.as_str() == "--dangerously-bypass-approvals-and-sandbox")
                .count(),
            1
        );
        assert_codex_cua_arguments(
            &arguments,
            Path::new("/Applications/RiWork App/Contents/MacOS/riwork"),
            home,
        );
        assert_codex_shell_environment(&arguments, home, "project-orchestrator-pane");
        let prompt = arguments.last().unwrap();
        assert!(prompt.contains("Scope: project."));
        assert!(prompt.contains(alpha));
        assert!(!prompt.contains(beta));
        assert!(prompt.contains(&quote_arg(&project_root.to_string_lossy())));
        assert!(
            prompt.contains(
                "Your isolated orchestration context is not the project's repository root."
            )
        );
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

#[cfg(test)]
mod scheduling_readiness_tests {
    use super::*;
    #[test]
    fn only_an_empty_harness_prompt_accepts_automated_input() {
        assert!(schedule_empty_prompt_at(
            Some(HarnessKind::Codex),
            "ready\n› \nfooter",
            1,
            2
        ));
        assert!(schedule_empty_prompt_at(
            Some(HarnessKind::Claude),
            "ready\n❯ \nfooter",
            1,
            2
        ));
        for screen in [
            "ready\n› \nWorking (1s • esc to interrupt)",
            "ready\nDo you trust this directory?\n❯ Yes, proceed",
            "ready\nApproval required\n❯ Allow once",
            "ready\nSign in to continue\n❯ Log in",
            "ready\n› existing draft\n",
            "ready\n$ \n",
        ] {
            assert!(
                !schedule_empty_prompt_at(Some(HarnessKind::Codex), screen, 1, 2),
                "{screen}"
            );
        }
        assert!(schedule_empty_prompt_at(
            Some(HarnessKind::Codex),
            "\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m",
            0,
            2
        ));
        assert!(!schedule_empty_prompt_at(
            Some(HarnessKind::Codex),
            "› Ask Codex to do anything",
            0,
            2
        ));
        assert!(!schedule_empty_prompt_at(
            Some(HarnessKind::Codex),
            "› ",
            0,
            5
        ));
        assert!(!schedule_empty_prompt_at(None, "› \n", 0, 2));
    }

    #[test]
    fn completed_reply_history_is_not_an_interactive_blocker() {
        let history = "• Thinking about login: approve the login fix.\n  Sign in to continue was the old error.\n  Approval required and esc to interrupt were quoted UI text.\n\n";
        for (harness, composer) in [
            (HarnessKind::Codex, "› "),
            (
                HarnessKind::Codex,
                "\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m",
            ),
            (HarnessKind::Claude, "❯ "),
        ] {
            let screen = format!("{history}{composer}\n? for shortcuts");
            assert!(schedule_empty_prompt_at(Some(harness), &screen, 4, 2));
        }
        for status in [
            "Working (1s • esc to interrupt)",
            "Approval required",
            "Sign in to continue",
            "Do you trust this directory?",
        ] {
            assert!(!schedule_empty_prompt_at(
                Some(HarnessKind::Codex),
                &format!("{history}› \n{status}"),
                4,
                2
            ));
        }
    }
}
