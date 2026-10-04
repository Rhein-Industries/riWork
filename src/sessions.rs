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
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use crate::session_keys::tmux_argument;

mod sample;
pub use sample::SessionSample;
mod watch;
pub(crate) use watch::Woke;

const HISTORY_LINES: usize = 100_000;
const ORCHESTRATOR_SKILL: &str = include_str!("../skills/riwork-orchestrator/SKILL.md");
/// Additive session guidance shared by the supported coding harnesses.
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
    Grok,
}

impl HarnessKind {
    pub fn program(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Grok => "grok",
        }
    }

    /// Scheduled dispatch needs a structured completed-turn signal and a
    /// provider identity proof. The picker, `Target::bind` and the ledger's
    /// `save` all consult this, so a harness cannot be offered but refused.
    pub fn schedulable(self) -> bool {
        matches!(self, Self::Codex | Self::Claude)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionMetrics {
    pub cpu_percent: f32,
    pub ram_bytes: u64,
    pub process_count: usize,
}

/// The sessions this build can represent. Other RiWork builds share the file,
/// so `read_registry` skips entries it cannot parse (for example a harness a
/// newer build added) instead of failing, and `write_registry` carries those
/// entries, unknown session fields, and unknown top-level fields through from
/// the file it replaces. Read it only through `read_registry`.
#[derive(Default, Serialize, Deserialize)]
struct Registry {
    sessions: Vec<ShellSession>,
}

/// How a tmux client attaches to a shell (see `SessionManager::attach_argv`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttachOptions {
    /// `-f ignore-size`: the client never sizes the window, so a second
    /// display (a remote desktop's) does not shrink the one the shell has.
    pub ignore_size: bool,
    /// `-r`: the client only watches; its keys are not sent to the shell.
    pub read_only: bool,
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
        Self::at(crate::paths::riwork_home()?)
    }

    /// Construct a manager with an alternate state directory. Useful for
    /// isolated installations and integration tests.
    pub fn at(home: PathBuf) -> Result<Self, String> {
        crate::paths::create_private_dir(&home)
            .map_err(|error| format!("create {}: {error}", home.display()))?;
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
        line: Option<u32>,
    ) -> Result<ShellSession, String> {
        validate_uuid(&project_id)?;
        if let Some(id) = &worktree_id {
            validate_uuid(id)?;
        }
        let path = crate::file_preview::validated_editor_path(&root, &path, expected)?;
        let vim = find_vim().ok_or("Vim is required to edit files in RiWork.")?;
        let command = editor_command(&vim, &path, line)?;
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
            // A tmux error is not "dead": dropping this row would start a second
            // orchestrator beside one that is still running unregistered.
            if self.is_alive(&existing.id)? {
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
                crate::settings::agent_inline_mode(&self.home),
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
            .list_saved()?
            .into_iter()
            .find(|session| matches_orchestrator_scope(session, None)))
    }

    pub fn orchestrator_get_for_project(
        &self,
        project_id: &str,
    ) -> Result<Option<ShellSession>, String> {
        validate_uuid(project_id)?;
        Ok(self
            .list_saved()?
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
        if !self.is_alive(id)? {
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
                true,
            ),
        )?;
        session.orchestrator_skill_loaded = true;
        session.orchestrator_skill_version = Some(orchestrator_skill_version());
        session.harness = Some(HarnessKind::Codex);
        let session = session.clone();
        self.write_registry(&registry)?;
        Ok(session)
    }

    /// Saved sessions with a live check. A plain shell reports a Codex identity
    /// only while Codex can still be running in it (see
    /// `hide_exited_plain_codex`); `get` returns the saved identity as is.
    pub fn list(&self) -> Result<Vec<ShellSession>, String> {
        Ok(self.list_with_activity()?.0)
    }

    /// `list`, and with it when tmux last saw output in each live session. Both
    /// come out of the one `tmux list-sessions` that finds the live sessions, so
    /// asking costs nothing more. It is a separate answer, not a field of
    /// `ShellSession`, because it moves whenever a shell prints and a window
    /// redraws when the sessions it holds differ (`refresh_sessions`).
    pub fn list_with_activity(&self) -> Result<(Vec<ShellSession>, SessionActivity), String> {
        let (mut sessions, activity) = self.list_saved_with_activity()?;
        self.hide_exited_plain_codex(&mut sessions);
        Ok((sessions, activity))
    }

    fn list_saved(&self) -> Result<Vec<ShellSession>, String> {
        Ok(self.list_saved_with_activity()?.0)
    }

    fn list_saved_with_activity(&self) -> Result<(Vec<ShellSession>, SessionActivity), String> {
        let sessions = self.read_registry()?.sessions;
        let live = self.live_session_activity()?;
        let sessions = self.with_liveness(sessions, |id| live.contains_key(id));
        let activity = sessions
            .iter()
            .filter_map(|session| Some((session.id.clone(), (*live.get(&session.id)?)?)))
            .collect();
        Ok((sessions, activity))
    }

    /// `sessions` marked alive when `is_live` says so. A Vim session that tmux
    /// no longer has is pruned from the registry and from the answer.
    fn with_liveness(
        &self,
        mut sessions: Vec<ShellSession>,
        is_live: impl Fn(&str) -> bool,
    ) -> Vec<ShellSession> {
        for session in &mut sessions {
            session.alive = is_live(&session.id);
        }
        if sessions
            .iter()
            .any(|session| !session.alive && session.editor_path.is_some())
        {
            let pruned = self.prune_exited_editors();
            sessions.retain(|session| !pruned.contains(&session.id));
        }
        sessions
    }

    /// `record_codex_launch` labels a plain shell as Codex when its user types
    /// `codex`, but the launcher `exec`s Codex, so nothing runs afterwards to
    /// take the label back. Report the label only while something other than
    /// the shell itself owns the pane, so an idle shell neither counts toward
    /// the tabs' account numbers nor keeps an old account in the status item.
    /// The saved row is untouched: `codex resume` in the pane still finds its
    /// account, and an unreadable tmux leaves the label as it was.
    fn hide_exited_plain_codex(&self, sessions: &mut [ShellSession]) {
        let candidates = plain_codex_candidates(sessions);
        if candidates.is_empty() {
            return;
        }
        let shell = self.default_command_shell();
        let Some(shell) = shell.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let Ok(output) = self.tmux_checked(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{window_index}\t#{pane_index}\t#{pane_current_command}",
        ]) else {
            return;
        };
        let at_prompt =
            panes_at_shell_prompt(&String::from_utf8_lossy(&output.stdout), &candidates, shell);
        forget_codex_labels(sessions, &at_prompt);
    }

    /// `hide_exited_plain_codex` for a sample, which already holds what the
    /// panes run. The server's default shell changes about never, so it is asked
    /// for once in a while, not every tick.
    fn hide_exited_plain_codex_in(&self, sessions: &mut [ShellSession], panes: &PaneTable) {
        let candidates = plain_codex_candidates(sessions);
        if candidates.is_empty() {
            return;
        }
        let shell =
            sample::cache_for(&self.socket_name).default_shell(|| self.default_command_shell());
        let Some(shell) = shell.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let at_prompt = panes.at_shell_prompt(&candidates, shell);
        forget_codex_labels(sessions, &at_prompt);
    }

    /// A Vim session has no `remain-on-exit`: `:q` destroys its tmux session,
    /// so its registry row can never be revived. Only editor rows are pruned;
    /// an exited agent or shell keeps its row for saved attribution. This is
    /// housekeeping run from `list`, so it never waits: callers such as
    /// scheduled delivery already hold the registry lock and a blocking
    /// acquire would deadlock. A skipped or failed prune is retried by the
    /// next `list`.
    fn prune_exited_editors(&self) -> HashSet<String> {
        let path = self.home.join("sessions.lock");
        let Ok(lock) = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
        else {
            return HashSet::new();
        };
        if lock.try_lock().is_err() {
            return HashSet::new();
        }
        // Creation holds this lock from `new-session` until the row is
        // written, so a fresh liveness check under it cannot mistake a session
        // that is still being registered for a dead one.
        let (Ok(mut registry), Ok(live)) = (self.read_registry(), self.live_session_names()) else {
            return HashSet::new();
        };
        let pruned: HashSet<String> = registry
            .sessions
            .iter()
            .filter(|session| session.editor_path.is_some() && !live.contains(&session.id))
            .map(|session| session.id.clone())
            .collect();
        if pruned.is_empty() {
            return pruned;
        }
        registry
            .sessions
            .retain(|session| !pruned.contains(&session.id));
        if self.write_registry(&registry).is_err() {
            return HashSet::new();
        }
        pruned
    }

    pub fn get(&self, id: &str) -> Result<ShellSession, String> {
        validate_uuid(id)?;
        self.list_saved()?
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
        // An agent in a Claude or Grok pane may itself run `codex exec` through
        // the PATH shim. That child must not relabel the pane's harness, drop
        // its activity binding or replace its account.
        if !matches!(session.harness, None | Some(HarnessKind::Codex)) {
            return Ok(());
        }
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
        if session.harness != Some(HarnessKind::Codex) || !self.is_alive(id)? {
            return Err("session reload requires a live RiWork Codex pane".to_owned());
        }
        let cwd = self.current_directory(id)?;
        let executable =
            env::current_exe().map_err(|error| format!("resolve RiWork executable: {error}"))?;
        let cua = crate::cua::CuaManager::at(self.home.clone())?;
        cua.driver_path()?;
        let shims = cua.ensure_harness_shims(&executable)?;
        let managed_path = path_with_harness_shims(&shims, login_shell_dirs())?;
        let args = respawn_arguments(
            id,
            &cwd.to_string_lossy(),
            &self.home,
            &managed_path,
            session.codex_home.as_deref(),
            command,
        );
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
        let argv = self.attach_argv(id, AttachOptions::default())?;
        Ok(argv
            .iter()
            .map(|argument| quote_arg(argument))
            .collect::<Vec<_>>()
            .join(" "))
    }

    /// The argument vector of a tmux client attached to shell `id`, after
    /// checking that it is live and configuring its scrolling. `attach_command`
    /// is this, quoted for Ghostty to parse; `attach_exec_command` is this,
    /// run by `riwork shell attach ID --exec`.
    pub fn attach_argv(&self, id: &str, options: AttachOptions) -> Result<Vec<String>, String> {
        self.require_live(id)?;
        // Ghostty renders a tmux client: scrollback belongs to tmux, so mouse
        // reporting is needed for wheel/trackpad scrolling and copy mode.
        // Set this per session as well, upgrading sessions made by older builds.
        let configured = self.configure_scrolling(id);
        self.invalidate_sample();
        configured?;
        let mut argv: Vec<String> = [
            "/usr/bin/env",
            "-u",
            "TMUX",
            "-u",
            "TMUX_TMPDIR",
            &self.tmux.to_string_lossy(),
            "-L",
            &self.socket_name,
            "attach-session",
        ]
        .map(str::to_owned)
        .into();
        if options.read_only {
            argv.push("-r".to_owned());
        }
        if options.ignore_size {
            argv.extend(["-f".to_owned(), "ignore-size".to_owned()]);
        }
        argv.extend(["-t".to_owned(), id.to_owned()]);
        Ok(argv)
    }

    /// The process `riwork shell attach ID --exec` turns into: `attach_argv`
    /// with the terminal type its tmux client should announce (see
    /// `attach_terminal`). The caller replaces itself with it, so the tmux
    /// client owns the terminal it was started on, which is what a remote
    /// desktop's pseudo-terminal needs.
    pub fn attach_exec_command(&self, id: &str, options: AttachOptions) -> Result<Command, String> {
        let argv = self.attach_argv(id, options)?;
        let inherited = env::var_os("TERMINFO").map(PathBuf::from);
        let terminal = attach_terminal(
            env::var("TERM").ok().as_deref(),
            inherited.as_deref(),
            &bundled_terminfo_dirs(),
        );
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).env("TERM", &terminal.term);
        match &terminal.terminfo {
            Some(dir) => command.env("TERMINFO", dir),
            None => command.env_remove("TERMINFO"),
        };
        Ok(command)
    }

    /// Mouse reporting and wheel scrolling for the shell's session, and hyperlinks for the server.
    /// Done on every attach, so servers and sessions made by older builds catch up.
    fn configure_scrolling(&self, id: &str) -> Result<(), String> {
        self.tmux_checked(&["set-option", "-t", id, "mouse", "on"])?;
        self.allow_hyperlinks();
        // RiWork owns this isolated tmux server. A pane on the main screen
        // (a shell, or an agent run inline) and an alternate-screen application
        // without mouse support both scroll through tmux's history.
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

    /// Let OSC 8 hyperlinks through tmux to the terminal. Claude Code and Codex print their URLs
    /// and file names that way, so that Ghostty can open them, but tmux drops them unless the
    /// client's terminal is declared to support them. A client picks this up when it attaches.
    ///
    /// The fixed index makes a repeated call replace the entry, not add another. A tmux that does
    /// not know the feature refuses it, and the only loss is the pass-through.
    fn allow_hyperlinks(&self) {
        let _ = self.tmux_command(&[
            "set-option",
            "-s",
            "terminal-features[100]",
            "xterm*:hyperlinks",
        ]);
    }

    /// What clicking a link in the shell's terminal needs, in one tmux call: the pane's size,
    /// scroll position, mode and working directory, the attached clients' sizes, and the screen
    /// twice (every row at full width, and with wrapped rows joined) so that a link broken across
    /// rows can be put back together, and a third time with its escape sequences, which is where
    /// the target of an OSC 8 hyperlink is. A few rows beyond the screen come with it, because a
    /// link can start above the first row.
    ///
    /// The screen is the one on display: the alternate screen of a full-screen program, or the
    /// part of the scrollback copy mode is showing. See `terminal_links::PaneView::parse`.
    pub fn capture_link_view(&self, id: &str) -> Result<crate::terminal_links::RawCapture, String> {
        use crate::terminal_links::CONTEXT_ROWS;
        validate_uuid(id)?;
        // Empty outside a mode, so 0 there.
        const SCROLL: &str = "#{?scroll_position,#{scroll_position},0}";
        // Counted from the top of the live screen: history is negative. tmux clamps both ends.
        let first = format!("-#{{e|+:{SCROLL},{CONTEXT_ROWS}}}");
        let last = format!("#{{e|-:#{{e|+:#{{e|-:#{{pane_height}},1}},{CONTEXT_ROWS}}},{SCROLL}}}");
        let header = format!(
            "#{{pane_width}}\t#{{pane_height}}\t#{{history_size}}\t{SCROLL}\t#{{pane_mode}}\t\
             #{{alternate_on}}\t#{{pane_current_path}}"
        );
        // Marks that screen text cannot contain by accident.
        let nonce = Uuid::new_v4().simple().to_string();
        let rows_mark = format!("riwork-rows-{nonce}");
        let joined_mark = format!("riwork-joined-{nonce}");
        let escaped_mark = format!("riwork-escaped-{nonce}");
        let pane = pane_target(id);
        let output = self.tmux_checked(&[
            "display-message",
            "-p",
            "-t",
            &pane,
            &header,
            ";",
            "list-clients",
            "-t",
            id,
            "-F",
            "#{client_control_mode}\t#{client_readonly}\t#{client_width}\t#{client_height}\t#{client_activity}",
            ";",
            "display-message",
            "-p",
            &rows_mark,
            ";",
            "capture-pane",
            "-p",
            "-N",
            "-t",
            &pane,
            "-S",
            &first,
            "-E",
            &last,
            ";",
            "display-message",
            "-p",
            &joined_mark,
            ";",
            "capture-pane",
            "-p",
            "-J",
            "-t",
            &pane,
            "-S",
            &first,
            "-E",
            &last,
            ";",
            "display-message",
            "-p",
            &escaped_mark,
            ";",
            "capture-pane",
            "-p",
            "-e",
            "-N",
            "-t",
            &pane,
            "-S",
            &first,
            "-E",
            &last,
        ])?;
        let text = String::from_utf8_lossy(&output.stdout);
        let malformed = || "tmux answered the screen request in an unexpected shape".to_owned();
        let (head, rest) = text
            .split_once(&format!("{rows_mark}\n"))
            .ok_or_else(malformed)?;
        let (rows, rest) = rest
            .split_once(&format!("{joined_mark}\n"))
            .ok_or_else(malformed)?;
        let (joined, escaped) = rest
            .split_once(&format!("{escaped_mark}\n"))
            .ok_or_else(malformed)?;
        let (header, clients) = head.split_once('\n').unwrap_or((head, ""));
        Ok(crate::terminal_links::RawCapture {
            header: header.to_owned(),
            clients: clients.to_owned(),
            rows: rows.to_owned(),
            joined: joined.to_owned(),
            escaped: escaped.to_owned(),
        })
    }

    /// Capture scrollback and the visible screen as plain text.
    pub fn capture(&self, id: &str, lines: usize) -> Result<String, String> {
        self.require_live(id)?;
        self.capture_text(id, lines, false)
    }

    /// The scrollback capture of a shell known to be live. `styled` keeps the
    /// SGR sequences of `capture-pane -e` and removes every other escape.
    fn capture_text(&self, id: &str, lines: usize, styled: bool) -> Result<String, String> {
        let start = format!("-{}", lines.clamp(1, HISTORY_LINES));
        let mut args = vec!["capture-pane", "-p"];
        if styled {
            args.push("-e");
        }
        let pane = pane_target(id);
        args.extend(["-t", &pane, "-S", &start]);
        let output = self.tmux_checked(&args)?;
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(if styled {
            crate::sgr::keep_sgr_only(&text)
        } else {
            text.into_owned()
        })
    }

    /// The plain `capture_screen_styled`.
    #[cfg(test)]
    pub fn capture_screen(&self, id: &str, lines: usize) -> Result<Capture, String> {
        self.capture_screen_styled(id, lines, false)
    }

    /// Like `capture`, plus what a remote screen needs to place a cursor: the
    /// pane size, the cursor cell and whether the pane is in a mode.
    ///
    /// The capture and the pane report run as one tmux command list, so no
    /// pane output can land between them. The last `rows` lines of
    /// `Capture::output` are exactly the visible screen, trailing blank rows
    /// included; see `align_screen`. If the report cannot be read, the output
    /// is returned as `capture` would return it, without a screen.
    ///
    /// With `styled` the output also holds the colors and text attributes as
    /// SGR sequences and no other escape or control sequence (see
    /// `sgr::keep_sgr_only`); the lines are the same ones.
    pub fn capture_screen_styled(
        &self,
        id: &str,
        lines: usize,
        styled: bool,
    ) -> Result<Capture, String> {
        self.require_live(id)?;
        self.capture_live_screen(id, lines, styled)
    }

    /// `capture_screen_styled` for a shell that was just checked to be live;
    /// a wait loop asks tmux once per poll instead of twice.
    fn capture_live_screen(&self, id: &str, lines: usize, styled: bool) -> Result<Capture, String> {
        let lines = lines.clamp(1, HISTORY_LINES);
        let pane = pane_target(id);
        let start = format!("-{lines}");
        if let Ok((output, report)) =
            self.capture_with_report(&pane, styled, &["-S", &start], SCREEN_REPORT)
            // Filtering keeps every newline, so the alignment still holds.
            && let Some(capture) = align_screen(&output, &report, lines)
        {
            return Ok(capture);
        }
        // The plain capture again, this time also failing for a shell that
        // has exited since the caller looked.
        self.require_live(id)?;
        Ok(Capture {
            output: self.capture_text(id, lines, styled)?,
            screen: None,
        })
    }

    /// `capture-pane -p` of `pane` over `range` (`-S`/`-E` arguments) and a
    /// `display-message` of `format`, as one tmux command list, so no pane
    /// output can land between the two. Returns the capture (with `styled`,
    /// reduced to SGR by `sgr::keep_sgr_only`, which keeps every newline) and
    /// what the format expanded to. This is the capture `shell output` and
    /// `shell history` share.
    fn capture_with_report(
        &self,
        pane: &str,
        styled: bool,
        range: &[&str],
        format: &str,
    ) -> Result<(String, String), String> {
        let marker = format!("riwork-screen-{}:", Uuid::new_v4().simple());
        let report = format!("{marker}{format}");
        let mut args = vec!["capture-pane", "-p"];
        if styled {
            args.push("-e");
        }
        args.extend(["-t", pane]);
        args.extend(range);
        args.extend([";", "display-message", "-p", "-t", pane, &report]);
        let output = self.tmux_checked(&args)?;
        let text = String::from_utf8_lossy(&output.stdout);
        let (captured, report) = text
            .rsplit_once(&marker)
            .ok_or_else(|| "tmux did not report on the pane".to_owned())?;
        let captured = if styled {
            crate::sgr::keep_sgr_only(captured)
        } else {
            captured.to_owned()
        };
        Ok((captured, report.to_owned()))
    }

    /// What `shell output --json` answers. Without `if_changed` this is one
    /// capture with its hash. With it, a capture whose hash is still
    /// `if_changed` is not returned: the shell is captured again, inside this
    /// call, whenever tmux reports that its pane changed (and in any case every
    /// `watch::SAFETY_POLL`; every `OUTPUT_POLL` where tmux cannot report), until
    /// the hash differs or `wait` has passed, and only then is `Unchanged` the
    /// answer. Each capture is one bounded tmux call; `wait` is capped at
    /// `MAX_OUTPUT_WAIT`.
    pub fn read_output(&self, id: &str, query: &OutputQuery<'_>) -> Result<OutputRead, String> {
        // Only the registry is read here. tmux is asked by the capture, and a
        // capture that fails asks again for the reason (`capture_live_screen`):
        // one tmux process fewer for every call, with the same errors.
        self.registered_session(id)?;
        let lines = query.lines.clamp(1, HISTORY_LINES);
        let mut waiter = Waiter::new(self, id);
        poll_output(
            query,
            |remaining| waiter.pause(remaining),
            || self.capture_live_screen(id, lines, query.styled),
        )
    }

    /// What `error`, a failure of a call on a shell that was not checked for
    /// life first, really was: the shell having exited, or `error` itself.
    fn failure_reason(&self, id: &str, error: String) -> String {
        self.require_live(id).err().unwrap_or(error)
    }

    /// One page of a shell's scrollback, see `HistoryPage`. `end` scrollback
    /// lines directly above the screen are skipped and the page holds the
    /// `lines` (1 to `HISTORY_PAGE_MAX`) above those, or fewer at the top of
    /// the history. `shell history` never waits.
    ///
    /// The page is `capture-pane -p [-e] -S -(end+lines) -E -(end+1)`. tmux
    /// does not clamp `-E` like `-S`: a page that ends above the top of the
    /// history would print line 0 again. The history size therefore comes from
    /// the same tmux command list (so no output can land between the two), and
    /// decides what the page is: nothing for `end >= history_size`, otherwise
    /// exactly `min(lines, history_size - end)` lines, which the capture has
    /// to match.
    pub fn read_history(
        &self,
        id: &str,
        end: u32,
        lines: u32,
        styled: bool,
    ) -> Result<HistoryPage, String> {
        if !(1..=HISTORY_PAGE_MAX).contains(&lines) {
            return Err(format!("--lines must be from 1 to {HISTORY_PAGE_MAX}"));
        }
        // The registry, not tmux, says whether this is a shell; tmux's own
        // answer to the capture says whether it is alive, and a failed capture
        // asks for the reason, so errors stay the same with one tmux process
        // fewer.
        self.registered_session(id)?;
        // tmux misreads line numbers that do not fit its 32 bits; nothing is
        // that deep, and a page that far up is empty either way.
        let line = |n: u64| format!("-{}", n.min(TMUX_LINE_LIMIT));
        let start = line(u64::from(end) + u64::from(lines));
        let stop = line(u64::from(end) + 1);
        let (page, report) = self
            .capture_with_report(
                &pane_target(id),
                styled,
                &["-S", &start, "-E", &stop],
                "#{history_size}",
            )
            .map_err(|error| self.failure_reason(id, error))?;
        let history_size: u32 = report
            .trim()
            .parse()
            .map_err(|_| format!("tmux reported the history size as {:?}", report.trim()))?;
        history_page(&page, history_size, end, lines)
    }

    /// Type literal text and named keys into an existing shell's pane, in
    /// order, as one batch under the shell's input lock. Errors that begin
    /// with a `session_keys` token happened before any key was sent.
    pub fn send_keys(&self, id: &str, items: &[crate::session_keys::Item]) -> Result<(), String> {
        use crate::session_keys::{INVALID_REQUEST, NOT_FOUND, NOT_SENT};
        let token = |error: String| {
            if error.starts_with("invalid UUID") {
                format!("{INVALID_REQUEST}{error}")
            } else if error.starts_with("unknown shell") || error.ends_with("has exited") {
                format!("{NOT_FOUND}{error}")
            } else {
                format!("{NOT_SENT}{error}")
            }
        };
        // The registry says whether this is a shell. Whether tmux still has it
        // is learned from the first tmux call of the batch; if that fails
        // before anything was typed, the reason is looked up then. One tmux
        // process fewer on every batch, the same errors.
        self.registered_session(id).map_err(token)?;
        crate::session_keys::send(&self.home, id, items, &|args| self.tmux_text(args)).map_err(
            |error| match error.strip_prefix(NOT_SENT) {
                Some(cause) => token(self.failure_reason(id, cause.to_owned())),
                None => error,
            },
        )
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
        self.schedule_provider_proof(shell)
            .map_err(IdentityError::into_message)
    }

    /// The provider conversation this pane demonstrably hosts right now. Only
    /// `Changed` says the pane hosts something else; a missing or unreadable
    /// proof is `Unproven`, which callers retry instead of pausing the schedule.
    pub(crate) fn schedule_provider_proof(
        &self,
        shell: &ShellSession,
    ) -> Result<String, IdentityError> {
        use IdentityError::{Changed, Unproven};
        match shell.harness {
            Some(HarnessKind::Codex) => {
                let tracker = crate::activity::ActivityTracker::at(self.home.clone());
                let bound = tracker.schedule_identity(shell);
                // Normal RiWork launches exec Codex in the pane. A stale binding
                // is not evidence that Codex is still running: without a live
                // descriptor proof (Codex exited to a shell, a wrapper or shared
                // server) the identity stays unknown.
                let descriptor = self.schedule_codex_rollout(&shell.id).map_err(Unproven)?;
                match (bound, descriptor) {
                    (Some(bound), Some(path)) => {
                        let home = shell
                            .codex_home
                            .as_ref()
                            .ok_or_else(|| Unproven("Codex account home is unknown".into()))?;
                        if !path.starts_with(home.join("sessions")) {
                            return Err(Changed(
                                "Codex rollout moved outside the pinned account home".into(),
                            ));
                        }
                        let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("");
                        if !name.ends_with(&bound) {
                            return Err(Changed(
                                "Codex thread changed in the selected pane".into(),
                            ));
                        }
                        Ok(bound)
                    }
                    (Some(_), None) => Err(Unproven(
                        "Codex is not the pane's foreground process, so its thread is unproven; deferring".into(),
                    )),
                    (None, Some(path)) => tracker
                        .bind_schedule_rollout(shell, &path)
                        .map_err(Unproven),
                    (None, None) => Err(Unproven(
                        "Codex thread identity is not yet known; complete a turn or use a directly launched RiWork Codex pane".into(),
                    )),
                }
            }
            Some(HarnessKind::Claude) => crate::agent_hooks::schedule_state(&self.home, &shell.id)
                .map(|(id, _)| id)
                .ok_or_else(|| {
                    Unproven(
                        "Claude session identity is not yet known; wait for a completed turn"
                            .into(),
                    )
                }),
            Some(harness) if !harness.schedulable() => Err(Unproven(format!(
                "{} sessions cannot be scheduled yet",
                harness.program()
            ))),
            _ => Err(Unproven("Scheduling requires Codex or Claude".into())),
        }
    }

    /// `(pane_pid, pane_current_command)` of the pane's foreground process.
    fn schedule_pane_process(&self, id: &str) -> Result<(String, String), String> {
        let pane = self.schedule_pane_report(id)?;
        let (pid, command) = pane
            .trim()
            .split_once('|')
            .ok_or("Cannot identify the harness process")?;
        Ok((pid.to_owned(), command.to_owned()))
    }

    fn schedule_pane_report(&self, id: &str) -> Result<String, String> {
        #[cfg(test)]
        if let Some(probe) = schedule_probe::lookup(&self.home) {
            return (probe.pane)(id);
        }
        self.tmux_text(&[
            "display-message",
            "-p",
            "-t",
            &pane_target(id),
            "#{pane_pid}|#{pane_current_command}",
        ])
    }

    fn schedule_lsof(&self) -> PathBuf {
        #[cfg(test)]
        if let Some(probe) = schedule_probe::lookup(&self.home) {
            return probe.lsof;
        }
        PathBuf::from("/usr/sbin/lsof")
    }

    /// `Ok(None)` means the foreground process cannot be Codex at all. Otherwise
    /// the pane's own process group must hold exactly one open primary rollout.
    fn schedule_codex_rollout(&self, id: &str) -> Result<Option<PathBuf>, String> {
        let (pid, command) = self.schedule_pane_process(id)?;
        // `node` is the npm launcher's name. It earns no trust of its own: the
        // same single-rollout descriptor proof must hold.
        if !matches!(command.as_str(), "codex" | "node") || pid.parse::<u32>().is_err() {
            return Ok(None);
        }
        codex_open_rollout(&self.schedule_lsof(), &pid).map(Some)
    }

    /// The registry lock excludes RiWork close/respawn for the whole attempt.
    /// The readiness sample and the submission also share `send`'s input lock.
    /// Only a target that is truly gone or demonstrably different fails (and
    /// pauses); a check that could not be completed defers and is retried.
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
        let gone = || {
            Delivery::Failed(
                "Target session no longer exists or has exited; select an existing target explicitly."
                    .into(),
            )
        };
        let changed = || {
            Delivery::Failed(
                "Target identity changed; edit and explicitly select the intended session.".into(),
            )
        };
        let shell = match self.get(&target.shell_id) {
            Ok(shell) if shell.alive => shell,
            Ok(_) => return Ok(gone()),
            Err(error) if error.starts_with("unknown shell") => return Ok(gone()),
            Err(error) => {
                return Ok(Delivery::Deferred(format!(
                    "Cannot read the target session: {error}"
                )));
            }
        };
        if !target.matches(state, &shell) {
            return Ok(changed());
        }
        match self.schedule_pane_identity(&shell.id) {
            Ok(pane) if pane == target.pane_identity => {}
            Ok(_) => return Ok(changed()),
            Err(error) => {
                return Ok(Delivery::Deferred(format!(
                    "Cannot read the target pane: {error}"
                )));
            }
        }
        match self.schedule_provider_proof(&shell) {
            Ok(provider) if provider == target.provider_session => {}
            Ok(_) | Err(IdentityError::Changed(_)) => return Ok(changed()),
            Err(IdentityError::Unproven(reason)) => {
                return Ok(Delivery::Deferred(format!(
                    "Provider identity is unproven: {reason}"
                )));
            }
        }
        if let Scope::Workspace { worktree_id, .. } = &target.scope {
            let workspace = state
                .worktrees
                .iter()
                .find(|w| &w.id == worktree_id)
                .ok_or("Workspace is missing")?;
            let directory = match self.current_directory(&shell.id) {
                Ok(directory) => directory,
                Err(error) => {
                    return Ok(Delivery::Deferred(format!(
                        "Cannot read the worker's directory: {error}"
                    )));
                }
            };
            if !directory.starts_with(&workspace.path) {
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
            &|args, input| self.tmux_text_input(args, input),
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
                    Some(HarnessKind::Grok) => None,
                    None => None,
                };
                let ready = (|| {
                    // Claude's identity comes from hooks alone, which outlive the
                    // process. A shell in the foreground means Claude exited, and
                    // shell prompts often draw the same `❯` as its composer.
                    if shell.harness == Some(HarnessKind::Claude) {
                        let (_, command) = self.schedule_pane_process(&shell.id)?;
                        if is_interactive_shell(&command) {
                            return Err(format!("The pane is running {command}, not Claude"));
                        }
                    }
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
                        Some(HarnessKind::Grok) => None,
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

    /// Give an existing shell pastes or keys without Return, chosen by `input` from the shell's
    /// harness, the name of its pane's foreground program and the pane's terminal (see `session_input::paste`).
    pub fn paste(
        &self,
        id: &str,
        input: impl FnOnce(Option<HarnessKind>, &str, &str) -> Vec<crate::session_input::Input>,
    ) -> Result<(), String> {
        let harness = self.registered_session(id)?.harness;
        crate::session_input::paste(
            &self.home,
            id,
            |command, tty| input(harness, command, tty),
            &|args| self.tmux_text(args),
            &|args, input| self.tmux_text_input(args, input),
        )
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
        self.metrics_for(&self.list_saved()?)
    }

    /// `metrics_snapshot` for sessions the caller has just listed, which spares
    /// it a second liveness query.
    fn metrics_for(
        &self,
        sessions: &[ShellSession],
    ) -> Result<BTreeMap<String, SessionMetrics>, String> {
        let live: HashSet<&str> = sessions
            .iter()
            .filter(|session| session.alive)
            .map(|session| session.id.as_str())
            .collect();
        if live.is_empty() {
            return Ok(BTreeMap::new());
        }
        let roots = self.pane_roots(&live)?;
        metrics_under(roots)
    }

    /// The process each named live shell's pane runs, from one tmux query.
    /// Shells tmux does not list are left out.
    pub fn pane_pids(&self, ids: &[String]) -> Result<BTreeMap<String, u32>, String> {
        let live: HashSet<&str> = ids.iter().map(String::as_str).collect();
        if live.is_empty() {
            return Ok(BTreeMap::new());
        }
        self.pane_roots(&live)
    }

    fn pane_roots(&self, live: &HashSet<&str>) -> Result<BTreeMap<String, u32>, String> {
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
        Ok(roots)
    }

    /// Every pane of the server in one query: which sessions exist, the
    /// directory, process and foreground command of each pane. A server that is
    /// not running has no panes.
    fn pane_table(&self) -> Result<PaneTable, String> {
        let output = self.tmux_command(&["list-panes", "-a", "-F", PANE_TABLE_FORMAT])?;
        if !output.status.success() {
            if no_tmux_server(&output) {
                return Ok(PaneTable::default());
            }
            return Err(tmux_error(&output));
        }
        Ok(PaneTable::parse(&String::from_utf8_lossy(&output.stdout)))
    }

    /// What a window's periodic refresh shows: every shell with its liveness,
    /// the directories of the live ones and, when `want_metrics`, their CPU and
    /// memory. The whole process shares one sample for about a tick, so this
    /// can be up to 1.5 s old; anything that changes sessions in this process
    /// discards it. Everything else here always asks tmux.
    pub fn sample(&self, want_metrics: bool) -> Result<SessionSample, String> {
        sample::cache_for(&self.socket_name).get(self, want_metrics)
    }

    /// Called wherever this process changes sessions, so the next `sample` reads
    /// tmux again instead of showing the old state for the rest of its TTL.
    fn invalidate_sample(&self) {
        sample::cache_for(&self.socket_name).invalidate();
    }

    /// Explicitly end a shell. Closing its GPUI tab must not call this method.
    pub fn close(&self, id: &str) -> Result<(), String> {
        validate_uuid(id)?;
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        if !registry.sessions.iter().any(|session| session.id == id) {
            return Err(format!("unknown shell {id}"));
        }
        // A tmux failure (for example a timeout) is not "already exited":
        // dropping the row would orphan a session that may still be running.
        if self.live_session_names()?.contains(id) {
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
        // A lossy conversion would name a different directory and tmux would
        // fall back to $HOME without saying so.
        let cwd_text = cwd
            .to_str()
            .ok_or_else(|| {
                format!(
                    "{} is not valid UTF-8 and cannot be passed to tmux",
                    cwd.display()
                )
            })?
            .to_owned();
        let plain_shell = binding.is_none()
            && harness.is_none()
            && kind == ShellKind::Project
            && command.is_none();
        let codex_home = if plain_shell {
            // Best effort: a saved account that vanished (Orca removed, the
            // profile moved, the account deleted) must not stop a terminal
            // from opening. Codex launches and the `codex` shim still fail
            // closed with the reason; the shell just starts without an
            // injected home, and never inherits another session's.
            match selected_codex_binding(&self.home, project_id.as_deref()) {
                Ok(binding) => Some(binding.home.into_os_string()),
                Err(_) => crate::codex_accounts::user_codex_home_variable(),
            }
        } else {
            binding
                .as_ref()
                .map(|binding| binding.home.clone().into_os_string())
                .or_else(|| env::var_os("CODEX_HOME"))
        };
        let profile_locations = [
            ("CODEX_HOME", codex_home),
            ("CLAUDE_CONFIG_DIR", env::var_os("CLAUDE_CONFIG_DIR")),
            ("GROK_HOME", env::var_os("GROK_HOME")),
        ];
        let executable =
            env::current_exe().map_err(|error| format!("resolve RiWork executable: {error}"))?;
        let cua = crate::cua::CuaManager::at(self.home.clone())?;
        let shim_directory = cua.ensure_harness_shims(&executable)?;
        // A plain shell's login startup files build its PATH; a harness runs
        // under `zsh -c`, which reads only `.zshenv`.
        let launches_harness =
            harness.is_some() || (kind == ShellKind::Orchestrator && binding.is_some());
        let managed_path = path_with_harness_shims(
            &shim_directory,
            if launches_harness {
                login_shell_dirs()
            } else {
                &[]
            },
        )?;
        let shell = self.default_command_shell();
        let zsh_environment = if shell.file_name().is_some_and(|name| name == "zsh") {
            let directory = install_zsh_startup_forwarding(&self.home, &shim_directory)?;
            zsh_startup_environment(&directory, env::var_os("ZDOTDIR").as_deref())
        } else {
            Vec::new()
        };
        // Read at launch, so a change in Settings reaches the next session and
        // leaves the running ones as they are.
        let inline = harness.is_some() && crate::settings::agent_inline_mode(&self.home);
        let mut command = match harness {
            Some(harness) => {
                let program = find_harness_program(harness, &shim_directory)
                    .ok_or_else(|| harness_missing(harness))?;
                if harness == HarnessKind::Grok {
                    // The driver's start can outlast Grok's 30-second MCP limit,
                    // so it finishes before Grok is created.
                    cua.prepare_for_grok()?;
                    ensure_grok_agent(&self.home, &executable)?;
                } else {
                    cua.driver_path()?;
                }
                Some(harness_command(
                    harness,
                    HarnessOptions {
                        unrestricted,
                        inline,
                    },
                    &program,
                    &executable,
                    &self.home,
                    &id,
                    binding.as_ref().map(|binding| binding.home.as_path()),
                )?)
            }
            None => command.map(|command| match &binding {
                Some(binding) => with_codex_home(&command, &binding.home),
                None => command,
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
            "-P".to_owned(),
            "-F".to_owned(),
            "#{pane_start_path}".to_owned(),
            "-s".to_owned(),
            id.clone(),
            "-c".to_owned(),
            tmux_directory(&cwd_text),
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
        // Panes get their environment from the server, not from this client, so
        // a plain shell that runs `codex` needs the override named explicitly.
        let cua_driver = crate::cua::driver_override_for_harness();
        if let Some(driver) = &cua_driver {
            args.push("-e".to_owned());
            args.push(format!("RIWORK_CUA_DRIVER={}", driver.to_string_lossy()));
        }
        // The tmux server does not inherit this client's environment, so Grok's
        // timeout has to be named on the pane. A value the user already set is
        // forwarded unchanged.
        let grok_timeouts = if harness == Some(HarnessKind::Grok) {
            grok_mcp_timeout_from_process()
        } else {
            Vec::new()
        };
        for (name, value) in &grok_timeouts {
            args.push("-e".to_owned());
            args.push(format!("{name}={value}"));
        }
        // Claude Code takes its screen from the environment, not from a flag.
        if inline && harness == Some(HarnessKind::Claude) {
            args.push("-e".to_owned());
            args.push(format!("{}={}", CLAUDE_MAIN_SCREEN.0, CLAUDE_MAIN_SCREEN.1));
        }
        if let Some(command) = &command {
            if command.trim().is_empty() {
                return Err("shell command cannot be empty".to_owned());
            }
            args.push(command.clone());
        }
        // Variables absent from this launch must not come back from the server's
        // environment. Clearing them there, and not with `env -u` in the command,
        // leaves the user's own startup files free to set them.
        let mut absent = stale_profile_variables(&profile_locations);
        if cua_driver.is_none() {
            absent.push("RIWORK_CUA_DRIVER");
        }
        if harness == Some(HarnessKind::Grok) {
            for name in ["GROK_MCP_STARTUP_TIMEOUT_SECS", "MCP_TIMEOUT"] {
                if !grok_timeouts.iter().any(|(key, _)| *key == name) {
                    absent.push(name);
                }
            }
        }
        // history-limit applies only to panes created afterwards, so it is set
        // in the same tmux invocation, ahead of the session's first pane.
        let mut invocation = vec!["start-server".to_owned()];
        for variable in &absent {
            invocation.extend(
                [";", "set-environment", "-gu", variable]
                    .into_iter()
                    .map(str::to_owned),
            );
        }
        invocation.extend(
            [";", "set-option", "-g", "history-limit"]
                .into_iter()
                .map(str::to_owned),
        );
        invocation.push(HISTORY_LINES.to_string());
        invocation.push(";".to_owned());
        invocation.extend(args.iter().map(|argument| tmux_argument(argument)));
        let borrowed: Vec<&str> = invocation.iter().map(String::as_str).collect();
        let created = self.tmux_checked_without(&borrowed, &absent);
        self.invalidate_sample();
        let created = created?;
        // tmux silently starts the shell in $HOME when it cannot use the
        // directory it was given. An empty report means this tmux cannot say.
        let reported = String::from_utf8_lossy(&created.stdout);
        let reported = reported.strip_suffix('\n').unwrap_or(&reported);
        if !reported.is_empty() && reported != cwd_text {
            let _ = self.kill_tmux_session(&id);
            return Err(format!(
                "tmux started the shell in {reported} instead of {cwd_text}; \
                 this directory name cannot be passed to tmux"
            ));
        }
        let configured = self
            .tmux_checked(&["set-option", "-g", "status", "off"])
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

    /// Only a definite "no such session" (or no server at all) is "dead". A
    /// tmux that cannot answer, for example because it timed out, is an error.
    fn is_alive(&self, id: &str) -> Result<bool, String> {
        Ok(self.live_session_names()?.contains(id))
    }

    fn live_session_names(&self) -> Result<HashSet<String>, String> {
        Ok(self.live_session_activity()?.into_keys().collect())
    }

    /// The sessions the server has, each with the Unix second its window last
    /// had output, or `None` when tmux gave no time. One `list-sessions` answers
    /// both. `#{window_activity}` is the session's current window, which is its
    /// only one: RiWork never makes another. It moves when the pane prints,
    /// which covers an agent working with no client attached and text sent by
    /// `shell send`, `shell keys` or the phone, once the shell echoes it.
    /// `#{session_activity}` is not used: tmux moves it for what an attached
    /// client does (attaching, detaching, a key, the pointer, focus) and never
    /// for output or `send-keys`, so a working agent nobody looks at would read
    /// as idle and a hovering pointer as work.
    fn live_session_activity(&self) -> Result<HashMap<String, Option<u64>>, String> {
        let output = self.tmux_command(&["list-sessions", "-F", SESSION_ACTIVITY_FORMAT])?;
        if !output.status.success() {
            if no_tmux_server(&output) {
                return Ok(HashMap::new());
            }
            return Err(tmux_error(&output));
        }
        Ok(parse_session_activity(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn kill_tmux_session(&self, id: &str) -> Result<(), String> {
        let killed = self.tmux_checked(&["kill-session", "-t", &format!("={id}")]);
        self.invalidate_sample();
        killed.map(|_| ())
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
        crate::session_input::submit(
            &self.home,
            id,
            text,
            &|args| self.tmux_text(args),
            &|args, input| self.tmux_text_input(args, input),
        )
    }

    fn tmux_command(&self, args: &[&str]) -> Result<Output, String> {
        self.tmux_run(args, None)
    }

    /// Every tmux client is bounded: a wedged server must fail a call, not
    /// freeze the caller (often the UI thread). Nothing sent through here is
    /// expected to block; no `wait-for`, `run-shell` or interactive attach.
    fn tmux_run(&self, args: &[&str], input: Option<&[u8]>) -> Result<Output, String> {
        self.tmux_run_without(args, input, &[])
    }

    /// `removed` variables are hidden from the client. A client that starts the
    /// server hands it its own environment as the server's global one.
    fn tmux_run_without(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        removed: &[&str],
    ) -> Result<Output, String> {
        let mut command = self.tmux_client();
        command.args(args);
        for variable in removed {
            command.env_remove(variable);
        }
        run_bounded(command, input, TMUX_TIMEOUT, &tmux_label(args))
    }

    /// Stop this manager's whole tmux server and remove its socket file, which tmux
    /// leaves behind. Test cleanup: a fixture that only closes its sessions one by
    /// one leaves the server running when a close is refused or setup panics halfway.
    #[cfg(test)]
    pub(crate) fn kill_server(&self) {
        let socket = self
            .tmux_text(&["display-message", "-p", "#{socket_path}"])
            .ok()
            .map(|path| PathBuf::from(path.trim()));
        let _ = self.tmux_command(&["kill-server"]);
        if let Some(socket) = socket.filter(|path| path.is_absolute()) {
            let _ = fs::remove_file(socket);
        }
    }

    /// Whether this manager's tmux server answers at all (tests).
    #[cfg(test)]
    pub(crate) fn server_running(&self) -> bool {
        self.tmux_command(&["list-sessions"])
            .is_ok_and(|output| output.status.success())
    }

    /// A tmux client for this manager's server, before any command.
    fn tmux_client(&self) -> Command {
        let mut command = Command::new(&self.tmux);
        command
            // UTF-8 whatever the locale: without one (the connector's LaunchAgent sets
            // none) tmux prints a tab inside a format as `_`, and every tab-separated
            // answer read here (list-sessions, list-panes, the link view) would fail to
            // parse, so live shells read as gone.
            .arg("-u")
            .arg("-L")
            .arg(&self.socket_name)
            .arg("-f")
            .arg("/dev/null")
            .env_remove("TMUX")
            // The socket directory must not depend on who launched this
            // process, or a terminal and the app would run separate servers.
            // `attach_command` clears it for the same reason.
            .env_remove("TMUX_TMPDIR")
            .env_remove("RIWORK_RESTORE_TICKET")
            .env("PATH", effective_path());
        command
    }

    fn tmux_checked(&self, args: &[&str]) -> Result<Output, String> {
        self.tmux_checked_without(args, &[])
    }

    fn tmux_checked_without(&self, args: &[&str], removed: &[&str]) -> Result<Output, String> {
        let output = self.tmux_run_without(args, None, removed)?;
        if !output.status.success() {
            return Err(tmux_error(&output));
        }
        Ok(output)
    }

    /// Run a tmux command whose argument list must stay free of user text,
    /// passing that text on stdin instead (`load-buffer -`).
    fn tmux_text_input(&self, args: &[&str], input: &[u8]) -> Result<String, String> {
        let output = self.tmux_run(args, Some(input))?;
        if !output.status.success() {
            return Err(tmux_error(&output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn lock_registry(&self) -> Result<File, String> {
        let path = self.home.join("sessions.lock");
        let mut options = OpenOptions::new();
        // A pure lock file: never truncate it, and keep other users from
        // opening it to hold the lock.
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        file.lock()
            .map_err(|error| format!("lock {}: {error}", path.display()))?;
        Ok(file)
    }

    fn read_registry(&self) -> Result<Registry, String> {
        let path = self.home.join("sessions.json");
        match fs::read(&path) {
            Ok(bytes) => {
                parse_registry(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(error) => Err(format!("read {}: {error}", path.display())),
        }
    }

    fn write_registry(&self, registry: &Registry) -> Result<(), String> {
        let path = self.home.join("sessions.json");
        let temporary = self.home.join(format!(".sessions-{}.tmp", Uuid::new_v4()));
        let document = registry_document(registry, &path)?;
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|error| format!("serialize shell registry: {error}"))?;
        let result = (|| -> Result<(), String> {
            // Account labels and emails live here, so keep it owner-only.
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
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
        self.invalidate_sample();
        result
    }
}

impl sample::SampleSource for SessionManager {
    fn saved(&self) -> Result<Vec<ShellSession>, String> {
        Ok(self.read_registry()?.sessions)
    }

    fn panes(&self) -> Result<PaneTable, String> {
        self.pane_table()
    }

    fn shells(&self, saved: Vec<ShellSession>, panes: &PaneTable) -> Vec<ShellSession> {
        let live = panes.sessions();
        let mut sessions = self.with_liveness(saved, |id| live.contains(id));
        self.hide_exited_plain_codex_in(&mut sessions, panes);
        sessions
    }

    fn directories(&self, shells: &[ShellSession], panes: &PaneTable) -> BTreeMap<String, PathBuf> {
        panes.directories(&live_shells(shells))
    }

    fn metrics(
        &self,
        shells: &[ShellSession],
        panes: &PaneTable,
    ) -> Result<BTreeMap<String, SessionMetrics>, String> {
        let live = live_shells(shells);
        if live.is_empty() {
            return Ok(BTreeMap::new());
        }
        metrics_under(panes.roots(&live))
    }
}

/// The ids of the shells that are alive.
fn live_shells(shells: &[ShellSession]) -> HashSet<&str> {
    shells
        .iter()
        .filter(|shell| shell.alive)
        .map(|shell| shell.id.as_str())
        .collect()
}

/// Parse per entry: one session this build cannot represent must not hide the
/// others, and a file that is not a `sessions` list is still an error.
fn parse_registry(bytes: &[u8]) -> Result<Registry, serde_json::Error> {
    #[derive(Deserialize)]
    struct Document {
        sessions: Vec<serde_json::Value>,
    }
    let document: Document = serde_json::from_slice(bytes)?;
    Ok(Registry {
        sessions: document
            .sessions
            .into_iter()
            .filter_map(|entry| serde_json::from_value(entry).ok())
            .collect(),
    })
}

/// The JSON to save: `registry` plus whatever the file it replaces holds that
/// this build cannot represent. Callers hold the registry lock, so the file is
/// the one `registry` was read from. A file that is not valid JSON has nothing
/// to keep.
fn registry_document(registry: &Registry, path: &Path) -> Result<serde_json::Value, String> {
    use serde_json::{Map, Value};
    let original = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let mut sessions = registry
        .sessions
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("serialize shell registry: {error}"))?;
    let mut extra = Map::new();
    if let Some(Value::Object(original)) = &original {
        let original_sessions = original
            .get("sessions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for session in sessions.iter_mut().filter_map(Value::as_object_mut) {
            let known = original_sessions
                .iter()
                .filter_map(Value::as_object)
                .find(|entry| entry.get("id").is_some() && entry.get("id") == session.get("id"));
            if let Some(known) = known {
                crate::store::restore_missing_fields(known, session);
            }
        }
        sessions.extend(
            original_sessions
                .iter()
                .filter(|entry| serde_json::from_value::<ShellSession>((*entry).clone()).is_err())
                .cloned(),
        );
        extra.extend(
            original
                .iter()
                .filter(|(key, _)| *key != "sessions")
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    let mut document = Map::new();
    document.insert("sessions".to_owned(), Value::Array(sessions));
    document.extend(extra);
    Ok(Value::Object(document))
}

#[cfg(test)]
mod compat_tests {
    use super::*;
    use serde_json::{Value, json};

    struct Home(PathBuf);

    impl Home {
        fn new() -> Self {
            let path = env::temp_dir().join(format!("riwork-registry-test-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn manager(&self) -> SessionManager {
            SessionManager {
                home: self.0.clone(),
                tmux: PathBuf::from("/unused/tmux"),
                socket_name: "unused".into(),
            }
        }

        fn registry_path(&self) -> PathBuf {
            self.0.join("sessions.json")
        }

        fn saved(&self) -> Value {
            serde_json::from_slice(&fs::read(self.registry_path()).unwrap()).unwrap()
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn session(id: &str) -> Value {
        json!({
            "id": id, "project_id": null, "worktree_id": null, "kind": "project",
            "cwd": "/work", "command": null, "created_at_unix": 1
        })
    }

    #[test]
    fn tmux_is_asked_for_utf8_whatever_the_locale() {
        // The connector's LaunchAgent sets no locale. tmux then prints a tab inside a
        // format as `_`, every tab-separated answer fails to parse, and live shells read
        // as gone (the phone saw no shells). `-u` makes the answers the same everywhere.
        let home = Home::new();
        let manager = home.manager();
        let command = manager.tmux_client();
        let args: Vec<_> = command.get_args().map(|arg| arg.to_owned()).collect();
        assert_eq!(
            args.first().map(|arg| arg.as_os_str()),
            Some(std::ffi::OsStr::new("-u"))
        );
        assert!(args.iter().any(|arg| arg == "-L"));
    }
    #[test]
    fn unparsed_entries_and_unknown_fields_survive_a_load_modify_save_cycle() {
        let home = Home::new();
        let manager = home.manager();
        let mut known = session("known");
        known["harness"] = json!("codex");
        known["future_session_flag"] = json!({"nested": [1, 2]});
        // A build that predates a harness cannot represent its sessions, and a
        // damaged entry is not even an object; neither may break the others.
        let mut other_harness = session("other-harness");
        other_harness["harness"] = json!("future_agent");
        other_harness["future_session_flag"] = json!(true);
        let damaged = json!("not a session");
        let original = json!({
            "sessions": [other_harness, known, damaged],
            "future_registry_field": {"epoch": 3}
        });
        fs::write(home.registry_path(), serde_json::to_vec(&original).unwrap()).unwrap();

        let mut registry = manager.read_registry().unwrap();
        assert_eq!(registry.sessions.len(), 1);
        assert_eq!(registry.sessions[0].id, "known");
        registry.sessions[0].unrestricted = true;
        registry
            .sessions
            .push(serde_json::from_value(session("added")).unwrap());
        manager.write_registry(&registry).unwrap();

        let saved = home.saved();
        assert_eq!(saved["future_registry_field"], json!({"epoch": 3}));
        let entries = saved["sessions"].as_array().unwrap();
        assert_eq!(entries.len(), 4);
        let entry = |id: &str| entries.iter().find(|entry| entry["id"] == id).unwrap();
        assert_eq!(entry("known")["unrestricted"], true);
        assert_eq!(
            entry("known")["future_session_flag"],
            json!({"nested": [1, 2]})
        );
        assert_eq!(entry("added")["kind"], "project");
        assert_eq!(entry("other-harness"), &original["sessions"][0]);
        assert!(entries.contains(&damaged));

        // Removing a session this build owns still removes it, and the entries
        // it cannot read stay put.
        let mut registry = manager.read_registry().unwrap();
        assert_eq!(registry.sessions.len(), 2);
        registry.sessions.retain(|session| session.id != "known");
        manager.write_registry(&registry).unwrap();
        let entries = home.saved()["sessions"].as_array().unwrap().clone();
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().all(|entry| entry["id"] != "known"));
        assert!(entries.contains(&original["sessions"][0]));
        assert!(entries.contains(&damaged));
    }

    #[test]
    fn a_registry_that_is_not_a_session_list_is_still_refused_and_untouched() {
        let home = Home::new();
        let manager = home.manager();
        for content in ["not json", "[]", r#"{"sessions": {"id": "x"}}"#, "{}"] {
            fs::write(home.registry_path(), content).unwrap();
            assert!(manager.read_registry().is_err(), "{content}");
            assert_eq!(fs::read_to_string(home.registry_path()).unwrap(), content);
        }
    }

    #[cfg(unix)]
    #[test]
    fn registry_and_lock_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new();
        let manager = home.manager();
        // A registry written by an earlier build is 0644 until it is replaced.
        fs::write(home.registry_path(), r#"{"sessions": []}"#).unwrap();
        fs::set_permissions(home.registry_path(), fs::Permissions::from_mode(0o644)).unwrap();
        let _lock = manager.lock_registry().unwrap();
        let registry = manager.read_registry().unwrap();
        manager.write_registry(&registry).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&home.registry_path()), 0o600);
        assert_eq!(mode(&home.0.join("sessions.lock")), 0o600);
    }

    #[test]
    fn damaged_unrelated_settings_do_not_block_codex_launches_but_a_bad_selection_does() {
        let home = Home::new();
        let settings = |content: &str| fs::write(home.0.join("settings.json"), content).unwrap();
        // Values from another build in unrelated settings are ignored.
        settings(
            r#"{"theme":"future_theme","project_order":{"by":"future_sort"},"remember_window_size":[1],"future_key":true}"#,
        );
        let binding = selected_codex_binding(&home.0, None).unwrap();
        assert_eq!(binding.id, None);
        // The account selection itself is never guessed.
        settings(r#"{"selected_codex_account":5}"#);
        assert!(selected_codex_binding(&home.0, None).is_err());
        settings(r#"{"selected_codex_account":"missing-account","theme":"future_theme"}"#);
        assert!(
            selected_codex_binding(&home.0, None)
                .unwrap_err()
                .contains("unavailable")
        );
        settings("not json");
        assert!(selected_codex_binding(&home.0, None).is_err());
    }
}

/// The terminal type an attached tmux client announces, and the terminfo
/// directory that describes it.
#[derive(Debug, PartialEq, Eq)]
struct AttachTerminal {
    term: String,
    /// `None` leaves the lookup to the system.
    terminfo: Option<PathBuf>,
}

/// `TERM` for a tmux client started by `shell attach --exec`. `requested` is
/// the `TERM` it was started with: a remote desktop's host asks for
/// `xterm-ghostty` or `xterm-256color` and the display there is a Ghostty.
///
/// - `xterm-ghostty` is kept when its terminfo can be found: in the app
///   bundle (`bundled`: `Contents/Resources/terminfo`, which RiWork ships
///   because the system has no entry for it) or in the `inherited` `TERMINFO`
///   (a Ghostty that started this). `TERMINFO` is then set to that directory.
///   Without one tmux would refuse the terminal, so it becomes
///   `xterm-256color`, which every system describes.
/// - A missing, empty or `dumb` `TERM` is `xterm-256color` too.
/// - Anything else is the caller's own terminal and stays.
fn attach_terminal(
    requested: Option<&str>,
    inherited: Option<&Path>,
    bundled: &[PathBuf],
) -> AttachTerminal {
    const GHOSTTY: &str = "xterm-ghostty";
    const FALLBACK: &str = "xterm-256color";
    match requested.map(str::trim) {
        Some(GHOSTTY) => {
            let found = bundled
                .iter()
                .map(PathBuf::as_path)
                .chain(inherited)
                .find(|dir| terminfo_entry(dir, GHOSTTY));
            match found {
                Some(dir) => AttachTerminal {
                    term: GHOSTTY.to_owned(),
                    terminfo: Some(dir.to_path_buf()),
                },
                None => AttachTerminal {
                    term: FALLBACK.to_owned(),
                    terminfo: None,
                },
            }
        }
        None | Some("" | "dumb") => AttachTerminal {
            term: FALLBACK.to_owned(),
            terminfo: None,
        },
        Some(other) => AttachTerminal {
            term: other.to_owned(),
            terminfo: inherited.map(Path::to_path_buf),
        },
    }
}

/// Whether `dir` holds a compiled entry for `term`, in the layout of macOS's
/// ncurses (`78/xterm-ghostty`, a hex directory) or of the others
/// (`x/xterm-ghostty`).
fn terminfo_entry(dir: &Path, term: &str) -> bool {
    let Some(first) = term.bytes().next() else {
        return false;
    };
    [format!("{first:02x}"), char::from(first).to_string()]
        .iter()
        .any(|folder| dir.join(folder).join(term).is_file())
}

/// Where this executable's app bundle keeps Ghostty's terminfo: next to the
/// executable inside `RiWork.app/Contents/MacOS`, or, for a build that sits
/// beside the app (a `target/release/riwork` next to a packaged `RiWork.app`),
/// inside that app.
fn bundled_terminfo_dirs() -> Vec<PathBuf> {
    let Ok(exe) = env::current_exe() else {
        return Vec::new();
    };
    let exe = exe.canonicalize().unwrap_or(exe);
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = dir
        .parent()
        .map(|contents| contents.join("Resources/terminfo"))
        .into_iter()
        .collect();
    dirs.push(dir.join("RiWork.app/Contents/Resources/terminfo"));
    dirs
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

/// `exec vim [+LINE] -- FILE`. The line is a number Vim reads as its own argument, never text
/// from the file name.
fn editor_command(vim: &Path, path: &Path, line: Option<u32>) -> Result<String, String> {
    let vim = vim
        .to_str()
        .ok_or("The Vim executable path cannot be passed to the shell.")?;
    let path = path
        .to_str()
        .ok_or("This file name cannot be passed to Vim.")?;
    let line = line
        .filter(|line| *line > 0)
        .map(|line| format!(" +{line}"))
        .unwrap_or_default();
    Ok(format!(
        "exec {}{line} -- {}",
        quote_arg(vim),
        quote_arg(path)
    ))
}

pub(crate) fn selected_codex_binding(
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

/// The tmux arguments that restart a Codex pane in the directory it is in.
fn respawn_arguments(
    id: &str,
    cwd: &str,
    state_home: &Path,
    managed_path: &std::ffi::OsStr,
    codex_home: Option<&Path>,
    command: &str,
) -> Vec<String> {
    let mut args = vec![
        "respawn-pane".to_owned(),
        "-k".to_owned(),
        "-t".to_owned(),
        pane_target(id),
        "-c".to_owned(),
        tmux_directory(cwd),
        "-e".to_owned(),
        format!("RIWORK_HOME={}", state_home.display()),
        "-e".to_owned(),
        format!("RIWORK_SHELL_ID={id}"),
        "-e".to_owned(),
        format!("PATH={}", managed_path.to_string_lossy()),
    ];
    if let Some(home) = codex_home {
        args.extend([
            "-e".to_owned(),
            format!("CODEX_HOME={}", home.display()),
            "-e".to_owned(),
            format!("RIWORK_CODEX_ACCOUNT_HOME={}", home.display()),
        ]);
    }
    args.push(command.to_owned());
    args.iter()
        .map(|argument| tmux_argument(argument))
        .collect()
}

/// What a harness launch asks for beyond the program and the profile.
#[derive(Clone, Copy)]
struct HarnessOptions {
    /// Skip the CLI's permission prompts, as the caller asked.
    unrestricted: bool,
    /// Keep the agent on the main screen (see `inline_arguments`).
    inline: bool,
}

fn harness_command(
    harness: HarnessKind,
    options: HarnessOptions,
    program: &Path,
    executable: &Path,
    state_home: &Path,
    shell_id: &str,
    codex_home: Option<&Path>,
) -> Result<String, String> {
    let HarnessOptions {
        unrestricted,
        inline,
    } = options;
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
            if inline {
                arguments.extend(inline_arguments(harness));
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
            // Passed per invocation with --settings; the user's Claude config is never edited.
            let hooks: serde_json::Map<_, _> = crate::agent_hooks::CLAUDE_HOOK_EVENTS
                .iter()
                .map(|event| {
                    (
                        (*event).to_owned(),
                        serde_json::json!([{"hooks":[{"type":"command","command":hook_command,"timeout":10}]}]),
                    )
                })
                .collect();
            let settings = serde_json::json!({
                "statusLine": { "type": "command", "command": telemetry_command },
                "hooks": hooks
            });
            arguments.push("--settings".to_owned());
            arguments.push(settings.to_string());
        }
        HarnessKind::Grok => {
            if unrestricted {
                arguments.push("--always-approve".to_owned());
            }
            if inline {
                arguments.extend(inline_arguments(harness));
            }
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

/// How a chat starts `harness`: the program, the Cua MCP wiring and the PATH
/// that a terminal launch of it gets (`harness_command`), without a terminal.
/// `codex_home` is the Codex account's home, for the Codex launch.
pub(crate) struct ChatLaunch {
    pub program: PathBuf,
    /// Goes before the harness's own arguments.
    pub arguments: Vec<String>,
    /// RiWork's harness shims first, then the login shell's directories, so the
    /// agent's own commands find what a terminal's would.
    pub path: std::ffi::OsString,
}

pub(crate) fn chat_launch(
    harness: HarnessKind,
    state_home: &Path,
    codex_home: Option<&Path>,
) -> Result<ChatLaunch, String> {
    let cua = crate::cua::CuaManager::at(state_home.to_path_buf())?;
    cua.driver_path()?;
    let state_home = state_home
        .canonicalize()
        .map_err(|error| format!("resolve RiWork state: {error}"))?;
    let executable =
        env::current_exe().map_err(|error| format!("resolve RiWork executable: {error}"))?;
    let shim_directory = cua.ensure_harness_shims(&executable)?;
    let program =
        find_harness_program(harness, &shim_directory).ok_or_else(|| harness_missing(harness))?;
    let mut arguments = cua_harness_arguments(harness, &executable, &state_home);
    if harness == HarnessKind::Codex
        && let Some(home) = codex_home
    {
        arguments.extend(codex_account_environment_arguments(home));
    }
    Ok(ChatLaunch {
        program,
        arguments,
        path: path_with_harness_shims(&shim_directory, login_shell_dirs())?,
    })
}

/// Claude Code reads this ahead of its own `tui` setting, so a user who has
/// chosen the fullscreen renderer still gets the main screen when RiWork asks
/// for inline agents.
const CLAUDE_MAIN_SCREEN: (&str, &str) = ("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN", "1");

/// The flags that keep an agent off the terminal's alternate screen. On the
/// main screen its transcript scrolls into tmux's history, which RiWork can
/// capture and a remote viewer can page through; on the alternate screen tmux
/// keeps no history at all. Claude Code has no such flag: see `CLAUDE_MAIN_SCREEN`.
///
/// Grok's `--no-alt-screen` is not enough: it still redraws a fixed full-height
/// canvas and reports the mouse, so tmux collects no history of the transcript
/// and stacks old frames on each resize. `--minimal` prints finished blocks
/// into the terminal's own scrollback.
fn inline_arguments(harness: HarnessKind) -> Vec<String> {
    match harness {
        HarnessKind::Codex => vec!["--no-alt-screen".to_owned()],
        HarnessKind::Grok => vec!["--minimal".to_owned()],
        HarnessKind::Claude => Vec::new(),
    }
}

/// Flags with which the caller has already chosen how the agent draws; adding
/// ours would repeat a switch (clap rejects that) or override the choice.
fn screen_choice_arguments(harness: HarnessKind) -> &'static [&'static str] {
    match harness {
        HarnessKind::Codex => &["--no-alt-screen"],
        HarnessKind::Grok => &["--minimal", "--fullscreen", "--no-alt-screen"],
        HarnessKind::Claude => &[],
    }
}

/// Whether a wrapper's invocation starts the agent's interactive screen, the
/// only one a screen flag may be added to: `codex exec` or `grok -p` would
/// reject or ignore it. A single word after the options may be a subcommand
/// this build does not know, so only a prompt with spaces counts as one.
fn starts_interactive_screen(harness: HarnessKind, arguments: &[String]) -> bool {
    if harness_utility_invocation(harness, arguments) {
        return false;
    }
    match harness {
        HarnessKind::Codex => match codex_first_command(arguments) {
            None | Some(("resume" | "fork", _)) => true,
            Some((word, _)) => word.contains(char::is_whitespace),
        },
        HarnessKind::Grok => !arguments
            .iter()
            .take_while(|argument| argument.as_str() != "--")
            .any(|argument| {
                let name = argument.split('=').next().unwrap_or(argument);
                matches!(name, "-p" | "--single" | "--prompt-file" | "--prompt-json")
            }),
        HarnessKind::Claude => false,
    }
}

/// The wrapper's final arguments, with the inline flags where the agent's own
/// options end. An invocation that already names its screen is left alone.
fn with_inline_arguments(
    harness: HarnessKind,
    original: &[String],
    mut proxied: Vec<String>,
) -> Vec<String> {
    if !starts_interactive_screen(harness, original) {
        return proxied;
    }
    let end = proxied
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(proxied.len());
    let chosen = screen_choice_arguments(harness);
    if proxied[..end]
        .iter()
        .any(|argument| chosen.contains(&argument.as_str()))
    {
        return proxied;
    }
    proxied.splice(end..end, inline_arguments(harness));
    proxied
}

/// The environment variable that puts Claude Code on the main screen, for a
/// launch the wrapper is about to make.
fn inline_environment(
    harness: HarnessKind,
    arguments: &[String],
) -> Option<(&'static str, &'static str)> {
    (harness == HarnessKind::Claude && !harness_utility_invocation(harness, arguments))
        .then_some(CLAUDE_MAIN_SCREEN)
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
    cua_harness_arguments_with(
        harness,
        executable,
        state_home,
        crate::cua::driver_override_for_harness().as_deref(),
    )
}

/// The harnesses filter their MCP servers' environments, so a custom driver
/// that the launcher accepted reaches `riwork cua mcp` only if named here.
fn cua_harness_arguments_with(
    harness: HarnessKind,
    executable: &Path,
    state_home: &Path,
    custom_driver: Option<&Path>,
) -> Vec<String> {
    match harness {
        HarnessKind::Codex => {
            let mut overrides = vec![
                format!(
                    "mcp_servers.cua-driver.command={}",
                    toml_string(&executable.to_string_lossy())
                ),
                "mcp_servers.cua-driver.args=[\"cua\",\"mcp\"]".to_owned(),
                format!(
                    "mcp_servers.cua-driver.env.RIWORK_HOME={}",
                    toml_string(&state_home.to_string_lossy())
                ),
            ];
            if let Some(driver) = custom_driver {
                overrides.push(format!(
                    "mcp_servers.cua-driver.env.RIWORK_CUA_DRIVER={}",
                    toml_string(&driver.to_string_lossy())
                ));
            }
            overrides.extend([
                "mcp_servers.cua-driver.enabled=true".to_owned(),
                "mcp_servers.cua-driver.required=true".to_owned(),
                "mcp_servers.cua-driver.startup_timeout_sec=120".to_owned(),
            ]);
            let mut arguments = vec!["--disable".to_owned(), "computer_use".to_owned()];
            for value in overrides {
                arguments.extend(["-c".to_owned(), value]);
            }
            arguments
        }
        HarnessKind::Claude => {
            let mut environment =
                serde_json::json!({ "RIWORK_HOME": state_home.to_string_lossy() });
            if let Some(driver) = custom_driver {
                environment["RIWORK_CUA_DRIVER"] = driver.to_string_lossy().into();
            }
            let config = serde_json::json!({
                "mcpServers": {
                    "cua-driver": {
                        "type": "stdio",
                        "command": executable.to_string_lossy(),
                        "args": ["cua", "mcp"],
                        "env": environment
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
        HarnessKind::Grok => vec![
            "--agent".to_owned(),
            grok_agent_path(state_home, executable, custom_driver)
                .to_string_lossy()
                .into_owned(),
        ],
    }
}

/// Launches with different custom drivers must not overwrite one another's definition.
fn grok_agent_path(state_home: &Path, executable: &Path, custom_driver: Option<&Path>) -> PathBuf {
    let mut identity = executable.to_string_lossy().into_owned();
    if let Some(driver) = custom_driver {
        identity.push('\0');
        identity.push_str(&driver.to_string_lossy());
    }
    state_home.join(format!(
        "cua/grok-agent-{:016x}.md",
        stable_hash(identity.as_bytes())
    ))
}

/// Grok's active agent can supply an MCP server for this session. An agent
/// definition in RiWork state avoids modifying the user's Grok config or the
/// project repository, while its ordinary config and login remain available.
/// Grok documents `startup_timeout_sec` for `config.toml` servers. Adding it
/// to this frontmatter does not change the handshake limit, so the definition
/// leaves the timeout unset. Launch code raises the default with
/// `GROK_MCP_STARTUP_TIMEOUT_SECS` when the user has not set that or
/// `MCP_TIMEOUT`.
fn grok_agent_definition(
    executable: &Path,
    state_home: &Path,
    custom_driver: Option<&Path>,
) -> String {
    let custom_driver = custom_driver
        .map(|driver| {
            format!(
                "      RIWORK_CUA_DRIVER: {}\n",
                toml_string(&driver.to_string_lossy())
            )
        })
        .unwrap_or_default();
    format!(
        concat!(
            "---\n",
            "name: riwork-cua\n",
            "description: Grok Build with RiWork desktop automation\n",
            "mcpServers:\n",
            "  - name: cua-driver\n",
            "    command: {}\n",
            "    args: [\"cua\", \"mcp\"]\n",
            "    env:\n",
            "      RIWORK_HOME: {}\n",
            "{}",
            "---\n",
            "{}\n"
        ),
        toml_string(&executable.to_string_lossy()),
        toml_string(&state_home.to_string_lossy()),
        custom_driver,
        CUA_GUIDANCE
    )
}

/// Seconds, matching the startup budget Codex receives. Grok's own default is 30.
const GROK_MCP_STARTUP_BUDGET_SECS: &str = "120";

/// How a documented Grok timeout variable is present in this process.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EnvText {
    Absent,
    Value(String),
    /// Set, but not valid UTF-8, so it cannot be forwarded through tmux.
    Unreadable,
}

fn env_text(name: &str) -> EnvText {
    match env::var_os(name) {
        None => EnvText::Absent,
        Some(value) if value.is_empty() => EnvText::Absent,
        Some(value) => match value.into_string() {
            Ok(text) => EnvText::Value(text),
            Err(_) => EnvText::Unreadable,
        },
    }
}

/// Environment for a RiWork-launched Grok. `MCP_TIMEOUT` (milliseconds) is
/// Grok's overriding default when both it and `GROK_MCP_STARTUP_TIMEOUT_SECS`
/// are set, so a user value is passed through and the 120-second budget is
/// added only when neither is set. A per-server `startup_timeout_sec` in the
/// user's config still wins inside Grok; this does not write that config.
fn grok_mcp_timeout_environment(
    startup: EnvText,
    mcp_timeout: EnvText,
) -> Vec<(&'static str, String)> {
    let mut variables = Vec::new();
    let mcp_set = matches!(mcp_timeout, EnvText::Value(_) | EnvText::Unreadable);
    match startup {
        EnvText::Value(value) => {
            variables.push(("GROK_MCP_STARTUP_TIMEOUT_SECS", value));
        }
        EnvText::Absent if !mcp_set => {
            variables.push((
                "GROK_MCP_STARTUP_TIMEOUT_SECS",
                GROK_MCP_STARTUP_BUDGET_SECS.to_owned(),
            ));
        }
        EnvText::Absent | EnvText::Unreadable => {}
    }
    if let EnvText::Value(value) = mcp_timeout {
        variables.push(("MCP_TIMEOUT", value));
    }
    variables
}

fn grok_mcp_timeout_from_process() -> Vec<(&'static str, String)> {
    grok_mcp_timeout_environment(
        env_text("GROK_MCP_STARTUP_TIMEOUT_SECS"),
        env_text("MCP_TIMEOUT"),
    )
}

fn ensure_grok_agent(state_home: &Path, executable: &Path) -> Result<(), String> {
    ensure_grok_agent_with(
        state_home,
        executable,
        crate::cua::driver_override_for_harness().as_deref(),
        &|path, content| fs::write(path, content),
        GROK_AGENT_MAX_AGE,
    )
}

/// Definitions older than this are removed when a launch installs its own.
/// Every launch refreshes the modification time of the one it uses, so only
/// definitions of executables or drivers that have not been launched for this
/// long can go; a build that returns simply writes its definition again.
const GROK_AGENT_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

fn ensure_grok_agent_with(
    state_home: &Path,
    executable: &Path,
    custom_driver: Option<&Path>,
    write: &dyn Fn(&Path, &str) -> std::io::Result<()>,
    max_age: Duration,
) -> Result<(), String> {
    let path = grok_agent_path(state_home, executable, custom_driver);
    let directory = path.parent().expect("Grok agent has a parent directory");
    fs::create_dir_all(directory)
        .map_err(|error| format!("create Grok Cua agent directory: {error}"))?;
    let content = grok_agent_definition(executable, state_home, custom_driver);
    if fs::read_to_string(&path).ok().as_deref() == Some(&content) {
        touch(&path);
    } else {
        let temporary = directory.join(format!("grok-agent-{}.tmp", Uuid::new_v4()));
        if let Err(error) = write(&temporary, &content) {
            // A failed write can still have created the file.
            let _ = fs::remove_file(&temporary);
            return Err(format!("write Grok Cua agent: {error}"));
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(format!("install Grok Cua agent: {error}"));
        }
    }
    prune_grok_agents(directory, &path, max_age);
    Ok(())
}

fn touch(path: &Path) {
    if let Ok(file) = OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// Remove definitions (and temporary files a crash left behind) that were last
/// used more than `max_age` ago, except `keep`. Housekeeping: errors are ignored.
fn prune_grok_agents(directory: &Path, keep: &Path, max_age: Duration) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ours =
            name.starts_with("grok-agent-") && (name.ends_with(".md") || name.ends_with(".tmp"));
        if !ours || path == keep {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > max_age);
        if expired {
            let _ = fs::remove_file(path);
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

/// The columns `live_session_activity` asks for.
const SESSION_ACTIVITY_FORMAT: &str = "#{session_name}\t#{window_activity}";

/// When tmux last saw output in each live shell, in Unix seconds, by shell id.
/// A session whose time tmux did not give is left out.
pub type SessionActivity = BTreeMap<String, u64>;

/// `SESSION_ACTIVITY_FORMAT` lines. A line without a time (or with zero) is a
/// live session of unknown activity, so a tmux that answers with names only
/// still says which sessions exist.
fn parse_session_activity(output: &str) -> HashMap<String, Option<u64>> {
    output
        .lines()
        .map(|line| match line.split_once('\t') {
            Some((name, time)) => (
                name.to_owned(),
                time.trim().parse().ok().filter(|time| *time > 0),
            ),
            None => (line.to_owned(), None),
        })
        .collect()
}

/// The columns of `PaneTable`. The directory comes last because it may hold a
/// tab.
const PANE_TABLE_FORMAT: &str = "#{session_name}\t#{window_index}\t#{pane_index}\t#{pane_pid}\t#{pane_current_command}\t#{pane_current_path}";

/// What one `tmux list-panes -a` shows, one row per pane. A periodic sample
/// reads the live sessions, the directories, the pane processes and the
/// foreground commands from this instead of asking tmux for each.
#[derive(Clone, Debug, Default)]
struct PaneTable {
    rows: Vec<PaneRow>,
}

#[derive(Clone, Debug)]
struct PaneRow {
    session: String,
    window: String,
    pane: String,
    pid: Option<u32>,
    command: String,
    path: String,
}

impl PaneTable {
    fn parse(output: &str) -> Self {
        let rows = output
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(6, '\t');
                Some(PaneRow {
                    session: fields.next()?.to_owned(),
                    window: fields.next()?.to_owned(),
                    pane: fields.next()?.to_owned(),
                    pid: fields.next()?.parse().ok(),
                    command: fields.next()?.to_owned(),
                    path: fields.next()?.to_owned(),
                })
            })
            .collect();
        Self { rows }
    }

    /// The sessions that have a pane, which is every session tmux has.
    fn sessions(&self) -> HashSet<&str> {
        self.rows.iter().map(|row| row.session.as_str()).collect()
    }

    /// The directory of each live shell's owned pane (window 0, pane 0).
    fn directories(&self, live: &HashSet<&str>) -> BTreeMap<String, PathBuf> {
        self.rows
            .iter()
            .filter(|row| {
                live.contains(row.session.as_str())
                    && row.window == "0"
                    && row.pane == "0"
                    && !row.path.is_empty()
            })
            .map(|row| (row.session.clone(), PathBuf::from(&row.path)))
            .collect()
    }

    /// The process each live shell's pane runs. A session with several panes
    /// reports the last one listed.
    fn roots(&self, live: &HashSet<&str>) -> BTreeMap<String, u32> {
        self.rows
            .iter()
            .filter(|row| live.contains(row.session.as_str()))
            .filter_map(|row| Some((row.session.clone(), row.pid?)))
            .collect()
    }

    /// Sessions among `candidates` whose window 0 pane is at the shell's own
    /// prompt, that is, nothing else is in its foreground.
    fn at_shell_prompt(&self, candidates: &HashSet<String>, shell: &str) -> HashSet<String> {
        self.rows
            .iter()
            .filter(|row| {
                candidates.contains(&row.session)
                    && row.window == "0"
                    && row.pane == "0"
                    // A login shell can be reported with its leading dash.
                    && row.command.trim_start_matches('-') == shell
            })
            .map(|row| row.session.clone())
            .collect()
    }
}

/// A shell the user started by hand (no command), which typing `codex` in it
/// has since labelled as a Codex session.
fn plain_shell_with_codex_label(session: &ShellSession) -> bool {
    session.kind == ShellKind::Project
        && session.command.is_none()
        && session.editor_path.is_none()
        && session.harness == Some(HarnessKind::Codex)
}

/// The live plain shells that are labelled as Codex sessions.
fn plain_codex_candidates(sessions: &[ShellSession]) -> HashSet<String> {
    sessions
        .iter()
        .filter(|session| session.alive && plain_shell_with_codex_label(session))
        .map(|session| session.id.clone())
        .collect()
}

/// Take the Codex label back from the sessions in `at_prompt`.
fn forget_codex_labels(sessions: &mut [ShellSession], at_prompt: &HashSet<String>) {
    for session in sessions.iter_mut().filter(|s| at_prompt.contains(&s.id)) {
        session.harness = None;
        session.unrestricted = false;
        session.codex_account_id = None;
        session.codex_account_label = None;
        session.codex_account_email = None;
        session.codex_home = None;
    }
}

/// Sessions among `candidates` whose window 0 pane is at the shell's own
/// prompt, that is, nothing else is in its foreground.
fn panes_at_shell_prompt(
    output: &str,
    candidates: &HashSet<String>,
    shell: &str,
) -> HashSet<String> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\t');
            let (id, window, pane, command) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            // A login shell can be reported with its leading dash.
            (candidates.contains(id)
                && window == "0"
                && pane == "0"
                && command.trim_start_matches('-') == shell)
                .then(|| id.to_owned())
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
        unset_in_command(command, &["RIWORK_PROJECT_ID"])
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

/// `inline_skill` puts the whole skill in the message, which suits a message
/// pasted into a running pane. A launch must not: the message travels in tmux's
/// command line, which tmux limits to about 16 KB, and the skill is already
/// installed at `skill_path` for the agent to read.
fn orchestrator_prompt(
    skill_path: &Path,
    executable: &Path,
    project_id: Option<&str>,
    project_root: Option<&Path>,
    inline_skill: bool,
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
    let (load, skill) = if inline_skill {
        (
            "Load the complete riwork-orchestrator skill below for this RiWork orchestrator session.",
            format!(
                "<riwork-orchestrator-skill>\n{ORCHESTRATOR_SKILL}\n</riwork-orchestrator-skill>\n\n"
            ),
        )
    } else {
        (
            "Load the complete riwork-orchestrator skill for this RiWork orchestrator session: \
             read its installed SKILL.md in full now, and follow it for the whole session.",
            String::new(),
        )
    };
    format!(
        "$riwork-orchestrator\n\n\
         {load} \
         Its installed source is {}. The installed RiWork CLI is {}: use this executable \
         for `riwork` commands if PATH does not resolve it, and retain RIWORK_HOME. \
         {} \
         This startup message only loads the skill. Do not inspect projects, create tasks, \
         modify repositories, delegate work, submit input to other harnesses, or create schedules. \
         After loading, briefly acknowledge readiness and wait for the user's objective.\n\n\
         {skill}{}",
        quote_arg(&skill_path.to_string_lossy()),
        quote_arg(&executable.to_string_lossy()),
        scope,
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
    inline: bool,
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
    if inline {
        arguments.extend(inline_arguments(HarnessKind::Codex));
    }
    arguments.push(orchestrator_prompt(
        skill_path,
        executable,
        project_id,
        project_root,
        false,
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

/// Run `command` without the named variables, for ones RiWork itself exports.
/// This applies after the shell's startup files, so it must not name a
/// variable the user may set there.
fn unset_in_command(command: &str, variables: &[&str]) -> String {
    if variables.is_empty() {
        return command.to_owned();
    }
    let unset = variables
        .iter()
        .map(|variable| format!("-u {}", quote_arg(variable)))
        .collect::<Vec<_>>();
    format!(
        "exec /usr/bin/env {} {}",
        unset.join(" "),
        command.strip_prefix("exec ").unwrap_or(command)
    )
}

/// The profile variables this launch does not set. A tmux server retains the
/// environment it started with, so these are cleared from its global
/// environment before the session exists.
fn stale_profile_variables<'a>(
    profile_locations: &[(&'a str, Option<std::ffi::OsString>)],
) -> Vec<&'a str> {
    profile_locations
        .iter()
        .filter(|(_, value)| value.is_none())
        .map(|(variable, _)| *variable)
        .collect()
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
        for path in [
            home.join(".local/bin"),
            home.join(".cargo/bin"),
            home.join(".grok/bin"),
        ] {
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

/// The RiWork shim directory comes first, then the login shell's directories in
/// the user's order, then this process's own.
fn path_with_harness_shims(
    shim_directory: &Path,
    login_directories: &[PathBuf],
) -> Result<std::ffi::OsString, String> {
    let mut directories = vec![shim_directory.to_path_buf()];
    for directory in login_directories.iter().cloned().chain(executable_dirs()) {
        if !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    env::join_paths(directories).map_err(|error| format!("construct RiWork harness PATH: {error}"))
}

/// A GUI launched from the Dock or Finder has launchd's minimal PATH, so CLIs
/// that nvm, pnpm, bun, pyenv and similar tools install are invisible to it.
/// The directories of the user's login shell are resolved once per process, and
/// only when something needs them.
fn login_shell_dirs() -> &'static [PathBuf] {
    static DIRECTORIES: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRECTORIES.get_or_init(|| {
        // Unit tests must not depend on the developer's shell configuration.
        if cfg!(test) {
            return Vec::new();
        }
        let shell = env::var_os("SHELL")
            .filter(|shell| !shell.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/zsh"));
        login_shell_path(&shell, LOGIN_PATH_TIMEOUT)
            .map(|path| {
                env::split_paths(&path)
                    .filter(|directory| directory.is_absolute())
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Start resolving the login shell's directories without waiting for them, so
/// the first harness launch does not pay for the shell's startup.
pub fn prefetch_login_shell_dirs() {
    std::thread::spawn(|| {
        login_shell_dirs();
    });
}

/// How long the user's shell gets to print its PATH. A profile that prompts or
/// hangs costs this much once and is then ignored.
const LOGIN_PATH_TIMEOUT: Duration = Duration::from_secs(3);

/// Ask `shell` for the PATH a terminal would give it: a login shell that is
/// also interactive, since version managers usually initialise in `.zshrc`, but
/// with no terminal, so nothing can wait for input. Startup noise on stdout is
/// ignored by printing the value between unique markers. Failures give `None`.
/// The output goes to a private file, not a pipe: a daemon the startup files
/// leave running would hold a pipe open and hide what the shell printed.
fn login_shell_path(shell: &Path, timeout: Duration) -> Option<std::ffi::OsString> {
    use std::process::Stdio;
    let marker = format!("__RIWORK_PATH_{}__", Uuid::new_v4().simple());
    let capture = env::temp_dir().join(format!("riwork-login-path-{}", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let output = options.open(&capture).ok()?;
    let read = (|| {
        let mut child = Command::new(shell)
            .args(["-l", "-i", "-c"])
            .arg(format!("printf '%s' \"{marker}$PATH\"\"{marker}\""))
            .stdin(Stdio::null())
            .stdout(output)
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        fs::read(&capture).ok()
    })();
    let _ = fs::remove_file(&capture);
    let stdout = String::from_utf8_lossy(&read?).into_owned();
    // The last pair wins, in case anything echoed the command line itself.
    let end = stdout.rfind(&marker)?;
    let start = stdout[..end].rfind(&marker)? + marker.len();
    let path = &stdout[start..end];
    (!path.is_empty()).then(|| std::ffi::OsString::from(path))
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

/// What a launch says when its agent CLI cannot be found. The remote connector
/// recognizes this sentence to tell the phone the agent is not installed
/// (`remote/src/rpc.rs`, `create_fault`), so it changes there too or not at all.
fn harness_missing(harness: HarnessKind) -> String {
    format!("{} is not installed or is not on PATH", harness.program())
}

/// The CLI a launch should run. This process's own PATH and the usual install
/// directories come first; the login shell is asked only when they find nothing.
fn find_harness_program(harness: HarnessKind, shim_directory: &Path) -> Option<PathBuf> {
    find_harness_program_via(harness, shim_directory, executable_dirs(), || {
        login_shell_dirs().to_vec()
    })
}

/// The official Grok CLI, for reading its own reports. RiWork's launcher shims
/// are skipped by their marker wherever they live, so this never runs one.
pub(crate) fn find_grok_program() -> Option<PathBuf> {
    find_harness_program(
        HarnessKind::Grok,
        Path::new("/nonexistent/riwork-harness-bin"),
    )
}

fn find_harness_program_via(
    harness: HarnessKind,
    shim_directory: &Path,
    directories: Vec<PathBuf>,
    login_directories: impl FnOnce() -> Vec<PathBuf>,
) -> Option<PathBuf> {
    find_harness_program_in(harness, shim_directory, directories)
        .or_else(|| find_harness_program_in(harness, shim_directory, login_directories()))
}

/// The path returned is the one found in the directory, not what it resolves
/// to: multi-call shims (mise, Volta) choose the program by the name they were
/// started under. Resolution serves only to recognise RiWork's own launchers.
fn find_harness_program_in(
    harness: HarnessKind,
    shim_directory: &Path,
    directories: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    let shim_directory = shim_directory
        .canonicalize()
        .unwrap_or_else(|_| shim_directory.to_owned());
    directories.into_iter().find_map(|directory| {
        let candidate = std::path::absolute(directory.join(harness.program())).ok()?;
        let resolved = candidate.canonicalize().ok()?;
        if !executable_file(&resolved) || resolved.parent() == Some(shim_directory.as_path()) {
            return None;
        }
        // Ignore wrappers from another RiWork state directory as well. Read
        // only a small prefix, never an entire official CLI binary.
        use std::io::Read;
        let mut prefix = [0; 512];
        if let Ok(mut file) = File::open(&resolved)
            && let Ok(length) = file.read(&mut prefix)
            && String::from_utf8_lossy(&prefix[..length]).contains("# RiWork Cua harness shim")
        {
            return None;
        }
        Some(candidate)
    })
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

fn harness_utility_invocation(harness: HarnessKind, arguments: &[String]) -> bool {
    if arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| {
            matches!(argument.as_str(), "--help" | "-h" | "--version" | "-V")
                || (harness == HarnessKind::Grok && argument == "-v")
        })
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
        HarnessKind::Grok => matches!(
            first,
            "help"
                | "agent"
                | "clone"
                | "completions"
                | "cursor-worker"
                | "disk-usage"
                | "doctor"
                | "du"
                | "export"
                | "inspect"
                | "leader"
                | "login"
                | "logout"
                | "mcp"
                | "memory"
                | "models"
                | "plugin"
                | "sessions"
                | "setup"
                | "trace"
                | "update"
                | "usage"
                | "v"
                | "version"
                | "worktree"
                | "wrap"
        ),
    }
}

/// The Codex subcommands that write credentials or configuration into
/// `CODEX_HOME`, as the command a user would recognize. Read-only forms such
/// as `login status` and `mcp list` are not included.
fn codex_home_mutation(arguments: &[String]) -> Option<String> {
    if arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| matches!(argument.as_str(), "--help" | "-h" | "--version" | "-V"))
    {
        return None;
    }
    let (command, remaining) = codex_first_command(arguments)?;
    let subcommand = remaining
        .iter()
        .map(String::as_str)
        .find(|argument| !argument.starts_with('-'));
    let mutates = match (command, subcommand) {
        ("login", Some("status")) => false,
        ("login" | "logout", _) => true,
        ("mcp", Some(subcommand)) => !matches!(subcommand, "list" | "get" | "help"),
        ("plugin", Some(subcommand)) => {
            !matches!(subcommand, "list" | "ls" | "show" | "get" | "info" | "help")
        }
        ("features", Some("enable" | "disable")) => true,
        _ => false,
    };
    mutates.then(|| match subcommand {
        Some(subcommand) if command != "login" => format!("codex {command} {subcommand}"),
        _ => format!("codex {command}"),
    })
}

/// Utility commands skip account binding, so they act on whatever `CODEX_HOME`
/// the shell exports. In a shell RiWork bound to a saved account that is
/// Orca's managed home, and `logout` or a second `login` would silently change
/// that account's credentials for every session that uses it.
fn managed_home_refusal(command: &str, home: &Path) -> String {
    let own = crate::codex_accounts::default_codex_home()
        .map(|home| crate::codex_accounts::display_home(&home))
        .unwrap_or_else(|_| "~/.codex".to_owned());
    format!(
        "`{command}` would change {}, a saved account home that Orca manages. \
         Manage that account in Orca. To run the command against your own profile \
         instead, set its home explicitly: `CODEX_HOME={own} {command}`.",
        crate::codex_accounts::display_home(home)
    )
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
        HarnessKind::Grok => {
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
    if harness == HarnessKind::Grok
        && !harness_utility_invocation(harness, arguments)
        && arguments
            .iter()
            .any(|argument| argument == "--agent" || argument.starts_with("--agent="))
    {
        return Err("RiWork's Grok launcher uses --agent for its Cua connection; a second --agent cannot be combined with it".to_owned());
    }
    if harness == HarnessKind::Codex
        && let Some((command, home)) =
            codex_home_mutation(arguments).zip(crate::codex_accounts::managed_codex_home_in_use())
    {
        return Err(managed_home_refusal(&command, &home));
    }
    let home = crate::paths::riwork_home()?;
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
        if harness == HarnessKind::Grok {
            cua.prepare_for_grok()?;
            ensure_grok_agent(&home, &executable)?;
            for (name, value) in grok_mcp_timeout_from_process() {
                command.env(name, value);
            }
        } else {
            cua.driver_path()?;
        }
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
    let mut proxied = cua_proxy_arguments(
        harness,
        arguments,
        &executable,
        &home,
        shell_id.as_deref(),
        account.as_ref().map(|binding| binding.home.as_path()),
    );
    if crate::settings::agent_inline_mode(&home) {
        proxied = with_inline_arguments(harness, arguments, proxied);
        if let Some((name, value)) = inline_environment(harness, arguments) {
            command.env(name, value);
        }
    }
    command.args(proxied).env("RIWORK_HOME", &home);
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

/// Only "there is no server", not every failure to connect: a socket path that
/// is too long, or one the user may not use, means tmux cannot be asked at all.
fn no_tmux_server(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains("no server running")
        || (stderr.contains("error connecting to")
            && stderr.contains("(No such file or directory)"))
}

/// The command a call is named after; chained commands are separated by `;`.
fn tmux_label(args: &[&str]) -> String {
    let last = args.rsplit(|argument| *argument == ";").next();
    format!(
        "tmux {}",
        last.and_then(<[&str]>::first).copied().unwrap_or_default()
    )
}

/// A pane capture and, when tmux could report it, the screen geometry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub output: String,
    pub screen: Option<Screen>,
}

/// The most lines one `read_history` page may hold. The phone asks for up to
/// this many once replies are compressed (`remote/src/link.rs`).
pub const HISTORY_PAGE_MAX: u32 = 5000;
/// The largest line number handed to tmux, which silently misreads larger ones.
const TMUX_LINE_LIMIT: u64 = 1_000_000_000;

/// A page of scrollback: lines above the screen, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPage {
    /// The lines, top to bottom, joined by `\n` with no newline after the
    /// last one: exactly `line_count` lines, blank ones included (so one blank
    /// line and no line are both `""`; `line_count` tells them apart).
    pub output: String,
    pub line_count: u32,
    /// Scrollback lines above the screen when the page was captured.
    pub history_size: u32,
    /// The page reaches the top of the history: no older line exists.
    pub complete: bool,
}

/// How long a `read_output` with `if_changed` sleeps between two captures when
/// nothing tells it sooner (see `watch`).
pub const OUTPUT_POLL: Duration = Duration::from_millis(80);
/// The longest `OutputQuery::wait`; longer ones are shortened to it.
pub const MAX_OUTPUT_WAIT: Duration = Duration::from_secs(10);

/// One `shell output --json` question.
#[derive(Clone, Copy, Debug)]
pub struct OutputQuery<'a> {
    pub lines: usize,
    /// Keep colors and text attributes as SGR sequences, and nothing else
    /// escaped.
    pub styled: bool,
    /// The `hash` of an earlier answer to the same question. While the
    /// content still hashes to it, nothing is returned but `Unchanged`.
    pub if_changed: Option<&'a str>,
    /// How long to wait for a change; only used with `if_changed`.
    pub wait: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputRead {
    Changed {
        capture: Capture,
        hash: String,
    },
    /// Nothing changed. `screen` is the current one, for the fields an
    /// unchanged answer still carries (`history_size`, `alternate`).
    Unchanged {
        hash: String,
        screen: Option<Screen>,
    },
}

/// Capture until `query.if_changed` no longer matches or `query.wait` is
/// over. `pause` is called between two captures with the time left and waits
/// for the screen to have a reason to be captured again, but never longer than
/// that; the deadline is real time, so the time a capture takes counts against
/// the wait and the whole call stays within `wait` plus one capture (each
/// bounded by the tmux timeout).
fn poll_output(
    query: &OutputQuery<'_>,
    mut pause: impl FnMut(Duration),
    mut capture: impl FnMut() -> Result<Capture, String>,
) -> Result<OutputRead, String> {
    let lines = query.lines.clamp(1, HISTORY_LINES);
    let deadline = output_deadline(std::time::Instant::now(), query.wait);
    loop {
        let current = capture()?;
        let hash = output_hash(lines, query.styled, &current);
        if query.if_changed != Some(hash.as_str()) {
            return Ok(OutputRead::Changed {
                capture: current,
                hash,
            });
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(OutputRead::Unchanged {
                hash,
                screen: current.screen,
            });
        }
        pause(deadline - now);
    }
}

/// How a waiting `read_output` passes the time between captures: it watches
/// the pane through a control-mode client (`watch`) and captures when told
/// something happened, or polls every `OUTPUT_POLL` when it cannot watch.
struct Waiter<'a> {
    manager: &'a SessionManager,
    id: &'a str,
    state: WaiterState,
}

enum WaiterState {
    /// No client yet: the first wait starts one.
    Fresh,
    Watching(watch::PaneWatch),
    Polling,
}

impl<'a> Waiter<'a> {
    fn new(manager: &'a SessionManager, id: &'a str) -> Self {
        Self {
            manager,
            id,
            state: WaiterState::Fresh,
        }
    }

    /// Wait for a reason to capture again, for at most `remaining`.
    fn pause(&mut self, remaining: Duration) {
        loop {
            match &mut self.state {
                WaiterState::Fresh => {
                    self.state = match remaining >= watch::MIN_WATCHED_WAIT {
                        true => self.manager.watch(self.id),
                        false => None,
                    }
                    .map_or(WaiterState::Polling, WaiterState::Watching);
                    if matches!(self.state, WaiterState::Watching(_)) {
                        // The client is attached, so nothing from now on can be
                        // missed; what happened before it was is in the next
                        // capture, which must follow at once.
                        return;
                    }
                }
                WaiterState::Watching(watch) => {
                    if watch.wait(remaining.min(watch::SAFETY_POLL)) == watch::Woke::Lost {
                        self.state = WaiterState::Polling;
                        continue;
                    }
                    return;
                }
                WaiterState::Polling => {
                    std::thread::sleep(OUTPUT_POLL.min(remaining));
                    return;
                }
            }
        }
    }
}

impl SessionManager {
    /// A control-mode client watching the pane of `id`, if one can be attached.
    fn watch(&self, id: &str) -> Option<watch::PaneWatch> {
        watch::PaneWatch::start(self.tmux_client(), id)
    }

    /// The same client for the desktop's terminal links, which read a screen again only when it
    /// may have changed. It blocks until attached (at most a second); call it off the UI thread.
    pub(crate) fn watch_shell(&self, id: &str) -> Option<watch::PaneWatch> {
        validate_uuid(id).ok()?;
        self.watch(id)
    }
}

fn output_deadline(start: std::time::Instant, wait: Duration) -> std::time::Instant {
    start + wait.min(MAX_OUTPUT_WAIT)
}

/// A short, stable fingerprint of an answer: 16 lowercase hex digits of a
/// 64-bit FNV-1a with a final mix. It covers everything the answer says and
/// everything that shaped it, so the same question about the same screen
/// gives the same hash in every process and version, and any visible change,
/// or a different question, gives another one:
/// the output text (styled or plain as asked), the cursor, rows, columns,
/// whether the pane is in a mode, the scrollback size and whether the
/// alternate screen is on (or that there is no screen), the number of lines
/// asked for (after clamping) and the styled flag.
fn output_hash(lines: usize, styled: bool, capture: &Capture) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    fn feed(state: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *state = (*state ^ u64::from(*byte)).wrapping_mul(PRIME);
        }
    }
    fn number(state: &mut u64, value: u64) {
        feed(state, &value.to_le_bytes());
    }
    let mut state = OFFSET;
    feed(&mut state, b"riwork/shell-output/v2\0");
    number(&mut state, lines as u64);
    number(&mut state, u64::from(styled));
    number(&mut state, capture.output.len() as u64);
    feed(&mut state, capture.output.as_bytes());
    match &capture.screen {
        None => number(&mut state, 0),
        Some(screen) => {
            number(&mut state, 1);
            number(&mut state, u64::from(screen.cursor.x));
            number(&mut state, u64::from(screen.cursor.y));
            number(&mut state, u64::from(screen.rows));
            number(&mut state, u64::from(screen.cols));
            number(&mut state, u64::from(screen.in_mode));
            number(&mut state, u64::from(screen.history_size));
            number(&mut state, u64::from(screen.alternate));
        }
    }
    // FNV-1a leaves the high bits of a short input weakly mixed.
    state ^= state >> 32;
    state = state.wrapping_mul(0xd6e8_feb8_6659_fd93);
    state ^= state >> 32;
    format!("{state:016x}")
}

/// The visible screen of a captured pane. `x` and `y` are 0-based cursor cells
/// (`#{cursor_x}`, `#{cursor_y}`) within the last `rows` lines of the output.
/// `x` counts terminal cells, so a wide character before the cursor takes two.
/// `history_size` (`#{history_size}`) is the number of scrollback lines above
/// the screen, and `alternate` (`#{alternate_on}`) whether a full-screen
/// program is on the alternate screen, which has no scrollback of its own: the
/// history above it is the normal screen's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Screen {
    pub cursor: Cursor,
    pub rows: u32,
    pub cols: u32,
    pub in_mode: bool,
    pub history_size: u32,
    pub alternate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Cursor {
    pub x: u32,
    pub y: u32,
}

/// What the tmux report of a screen capture asks for; see `align_screen`.
const SCREEN_REPORT: &str = "#{pane_height}|#{pane_width}|#{cursor_x}|#{cursor_y}|#{pane_in_mode}|#{history_size}|#{alternate_on}";

/// The screen rule of `shell output`: the last `rows` lines of the output are
/// the visible screen, exactly `rows` of them, blank ones included.
///
/// `capture-pane -p -S -N` prints `min(N, history_size) + rows` lines, one per
/// screen row, and tmux 3.6 keeps the blank rows at the bottom. A tmux that
/// trimmed them would leave the output short, and the cursor would then point
/// at the wrong line; the missing rows are all at the end, so they are put
/// back. Output with more lines than that cannot be aligned, and the screen
/// is left out rather than misreported.
///
/// `report` is `SCREEN_REPORT` expanded:
/// `HEIGHT|WIDTH|CURSOR_X|CURSOR_Y|IN_MODE|HISTORY_SIZE|ALTERNATE`.
fn align_screen(output: &str, report: &str, requested: usize) -> Option<Capture> {
    let fields: Vec<u32> = report
        .trim_end_matches('\n')
        .split('|')
        .map(|field| field.parse().ok())
        .collect::<Option<_>>()?;
    let [rows, cols, x, y, in_mode, history, alternate] = fields[..] else {
        return None;
    };
    if rows == 0 || cols == 0 || y >= rows || in_mode > 1 || alternate > 1 {
        return None;
    }
    let expected = (rows as usize).saturating_add(requested.min(history as usize));
    let have =
        output.matches('\n').count() + usize::from(!output.is_empty() && !output.ends_with('\n'));
    if have > expected {
        return None;
    }
    let mut output = output.to_owned();
    if have < expected {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&"\n".repeat(expected - have));
    }
    Some(Capture {
        output,
        screen: Some(Screen {
            cursor: Cursor { x, y },
            rows,
            cols,
            in_mode: in_mode == 1,
            history_size: history,
            alternate: alternate == 1,
        }),
    })
}

/// The `HistoryPage` of a `read_history` capture. `page` is what
/// `capture-pane -p -S -(end+lines) -E -(end+1)` printed and `history_size`
/// what tmux said in the same command list.
///
/// tmux clamps `-S` at the top of the history but `-E` to line 0, so a page
/// that ends above the top prints the very first line; for `end >=
/// history_size` the page is empty. Otherwise it holds
/// `min(lines, history_size - end)` lines, one per row like `capture_screen`,
/// and it is `complete` when it includes the first line of the history. A
/// tmux that trimmed blank lines at the end gets them back, as in
/// `align_screen`; more lines than that cannot be a page and are an error.
fn history_page(
    page: &str,
    history_size: u32,
    end: u32,
    lines: u32,
) -> Result<HistoryPage, String> {
    if end >= history_size {
        return Ok(HistoryPage {
            output: String::new(),
            line_count: 0,
            history_size,
            complete: true,
        });
    }
    let line_count = (history_size - end).min(lines);
    let expected = line_count as usize;
    let have = page.matches('\n').count() + usize::from(!page.is_empty() && !page.ends_with('\n'));
    if have > expected {
        return Err(format!(
            "tmux returned {have} history lines for a page of {expected}"
        ));
    }
    let mut output = page.to_owned();
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&"\n".repeat(expected - have));
    // Lines are joined by a newline: none follows the last one.
    output.pop();
    Ok(HistoryPage {
        output,
        line_count,
        history_size,
        complete: u64::from(end) + u64::from(lines) >= u64::from(history_size),
    })
}

/// tmux expands formats in `-c`, so `#T` or `#{...}` in a directory name would
/// name another directory. `##` is a literal `#`.
fn tmux_directory(path: &str) -> String {
    path.replace('#', "##")
}

/// Upper bound for any single tmux client. A healthy server answers in
/// milliseconds; this only trips when the server is wedged.
const TMUX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Run a command to completion, capturing its output like `Command::output`,
/// but kill it and fail if it outlives `timeout`. `input` is written to stdin
/// from a helper thread so a large payload cannot deadlock against the child.
fn run_bounded(
    mut command: Command,
    input: Option<&[u8]>,
    timeout: std::time::Duration,
    label: &str,
) -> Result<Output, String> {
    use std::{io::Read, process::Stdio, sync::mpsc, thread, time::Duration};
    fn drain(mut reader: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = reader.read_to_end(&mut bytes);
            let _ = sender.send(bytes);
        });
        receiver
    }
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("run {program}: {error}"))?;
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        let input = input.to_vec();
        // A child that exits early closes the pipe; that surfaces as its status.
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let stdout = drain(child.stdout.take().ok_or("capture stdout")?);
    let stderr = drain(child.stderr.take().ok_or("capture stderr")?);
    // The exit is waited for on a thread of its own and the deadline is a timed
    // receive, so the answer comes the moment the child ends. (A loop that
    // slept between `try_wait`s noticed a 3 ms tmux call only after 7 ms or
    // more, on every call.) The waiter owns the child until it is reaped, so
    // the pid below cannot have been reused when it is signalled.
    let pid = child.id();
    let (exited, status) = mpsc::channel();
    thread::spawn(move || {
        let _ = exited.send(child.wait());
    });
    let status = match status.recv_timeout(timeout) {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => return Err(format!("wait for {label}: {error}")),
        Err(_) => {
            // SAFETY: the waiter thread has not reaped the child, so `pid`
            // still names it.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            let _ = status.recv();
            return Err(format!(
                "{label} did not finish within {}s and was stopped; the server may be unresponsive",
                timeout.as_secs_f32()
            ));
        }
    };
    // The child is gone; a stream that stays open belongs to a leaked descendant.
    let collect = |receiver: mpsc::Receiver<Vec<u8>>| {
        receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| format!("{label} left its output stream open"))
    };
    Ok(Output {
        status,
        stdout: collect(stdout)?,
        stderr: collect(stderr)?,
    })
}

fn tmux_error(output: &Output) -> String {
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if detail.is_empty() {
        format!("tmux exited with {}", output.status)
    } else {
        format!("tmux: {detail}")
    }
}

pub(crate) fn quote_arg(argument: &str) -> String {
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

/// The CPU, memory and process count under each shell's pane process.
fn metrics_under(roots: BTreeMap<String, u32>) -> Result<BTreeMap<String, SessionMetrics>, String> {
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

/// Why a scheduled target's provider identity is not confirmed.
#[derive(Debug)]
pub(crate) enum IdentityError {
    /// Evidence was read and contradicts the pinned conversation.
    Changed(String),
    /// No proof either way: a timeout, unreadable evidence, or a foreground
    /// process that is not the harness. Callers retry rather than pause.
    Unproven(String),
}

impl IdentityError {
    pub(crate) fn into_message(self) -> String {
        match self {
            Self::Changed(message) | Self::Unproven(message) => message,
        }
    }
}

/// A shell in the foreground means the harness exited; shell prompts often
/// draw the same `›`/`❯` glyph as a harness composer.
fn is_interactive_shell(command: &str) -> bool {
    matches!(
        command.trim_start_matches('-'),
        "sh" | "bash"
            | "zsh"
            | "fish"
            | "dash"
            | "ksh"
            | "tcsh"
            | "csh"
            | "nu"
            | "xonsh"
            | "elvish"
    )
}

const DESCRIPTOR_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const DESCRIPTOR_LISTING_LIMIT: usize = 4 * 1024 * 1024;

/// The single primary rollout held open by the pane's process group, whose
/// leader is `pid`. That is the exec'd harness plus a launcher child such as
/// the npm wrapper's, and never another pane's files or processes. A bounded
/// child query, not a scan.
fn codex_open_rollout(lsof: &Path, pid: &str) -> Result<PathBuf, String> {
    use std::io::Read;
    let mut child = Command::new(lsof)
        .args(["-nP", "-a", "-g", pid, "-Fn"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot inspect Codex rollout identity: {e}"))?;
    // A pipe holds 64 KiB; a process with many descriptors writes more and
    // would block until the timeout unless the listing is drained meanwhile.
    let mut stdout = child.stdout.take().ok_or("Cannot read Codex descriptors")?;
    let reader = std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut truncated = false;
        let mut chunk = [0u8; 16 * 1024];
        while let Ok(read) = stdout.read(&mut chunk) {
            if read == 0 {
                break;
            }
            let room = DESCRIPTOR_LISTING_LIMIT - kept.len();
            truncated |= read > room;
            kept.extend_from_slice(&chunk[..read.min(room)]);
        }
        (kept, truncated)
    });
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
        if started.elapsed() > DESCRIPTOR_CHECK_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Codex descriptor identity check timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let (listing, truncated) = reader
        .join()
        .map_err(|_| "Cannot read Codex descriptors".to_owned())?;
    if !status.success() {
        return Err("Cannot verify Codex descriptor identity".into());
    }
    if truncated {
        return Err("Codex descriptor listing is too large to verify".into());
    }
    let paths: HashSet<_> = String::from_utf8_lossy(&listing)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .filter(|path| path.contains("/rollout-") && path.ends_with(".jsonl"))
        .map(PathBuf::from)
        .collect();
    if paths.len() != 1 {
        return Err("Codex has no unique open primary rollout; scheduling is deferred".into());
    }
    Ok(paths.into_iter().next().expect("one rollout"))
}

/// Test seam: stands in for the pane's foreground-process report and for
/// `lsof`, keyed by state home so unrelated fixtures keep the real probes.
#[cfg(test)]
pub(crate) mod schedule_probe {
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    pub(crate) type PaneReport = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;
    #[derive(Clone)]
    pub(crate) struct Probe {
        pub(crate) pane: PaneReport,
        pub(crate) lsof: PathBuf,
    }
    static PROBES: Mutex<Vec<(PathBuf, Probe)>> = Mutex::new(Vec::new());

    /// Removes the override when dropped, even if the test panics.
    pub(crate) struct Installed(PathBuf);
    impl Drop for Installed {
        fn drop(&mut self) {
            PROBES.lock().unwrap().retain(|(home, _)| home != &self.0);
        }
    }
    pub(crate) fn install(home: &Path, pane: PaneReport, lsof: PathBuf) -> Installed {
        let home = home.canonicalize().unwrap();
        let mut probes = PROBES.lock().unwrap();
        probes.retain(|(existing, _)| existing != &home);
        probes.push((home.clone(), Probe { pane, lsof }));
        Installed(home)
    }
    pub(super) fn lookup(home: &Path) -> Option<Probe> {
        PROBES
            .lock()
            .unwrap()
            .iter()
            .find(|(existing, _)| existing == home)
            .map(|(_, probe)| probe.clone())
    }
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
        Some(HarnessKind::Grok) => return false,
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
mod output_tests;

#[cfg(test)]
mod tmux_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminfo directory with the compiled entries named by `files`.
    fn terminfo_dir(files: &[&str]) -> PathBuf {
        let dir = env::temp_dir().join(format!("riwork-terminfo-{}", Uuid::new_v4()));
        for file in files {
            fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
            fs::write(dir.join(file), b"entry").unwrap();
        }
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_attached_client_announces_ghostty_only_where_its_terminfo_can_be_found() {
        let hex = terminfo_dir(&["78/xterm-ghostty"]);
        let letter = terminfo_dir(&["x/xterm-ghostty"]);
        let empty = terminfo_dir(&["78/xterm-kitty"]);
        let ghostty = |inherited: Option<&Path>, bundled: &[PathBuf]| {
            attach_terminal(Some("xterm-ghostty"), inherited, bundled)
        };
        let announced = |dir: &Path| AttachTerminal {
            term: "xterm-ghostty".into(),
            terminfo: Some(dir.to_path_buf()),
        };
        let fallback = AttachTerminal {
            term: "xterm-256color".into(),
            terminfo: None,
        };

        // The app bundle's terminfo, in macOS's hex layout or the plain one.
        assert_eq!(ghostty(None, &[hex.clone()]), announced(&hex));
        assert_eq!(ghostty(None, &[letter.clone()]), announced(&letter));
        // The first directory that has the entry wins; a bundle beats the environment.
        assert_eq!(
            ghostty(Some(&letter), &[empty.clone(), hex.clone()]),
            announced(&hex)
        );
        // A Ghostty that started the process has already said where it is.
        assert_eq!(ghostty(Some(&letter), &[empty.clone()]), announced(&letter));
        // Nowhere: tmux would refuse the terminal, so it is a plain xterm.
        assert_eq!(ghostty(None, &[]), fallback);
        assert_eq!(ghostty(Some(&empty), &[empty.clone()]), fallback);
        assert_eq!(ghostty(None, &[PathBuf::from("/nonexistent")]), fallback);

        // No terminal, or none that tmux can use, is a plain xterm; any other
        // is the caller's own and stays.
        for requested in [None, Some(""), Some("dumb"), Some("  ")] {
            assert_eq!(attach_terminal(requested, None, &[hex.clone()]), fallback);
        }
        assert_eq!(
            attach_terminal(Some("xterm-256color"), None, &[hex.clone()]),
            fallback
        );
        assert_eq!(
            attach_terminal(Some("screen-256color"), Some(&empty), &[hex.clone()]),
            AttachTerminal {
                term: "screen-256color".into(),
                terminfo: Some(empty.clone())
            }
        );
        for dir in [hex, letter, empty] {
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn a_missing_agent_is_reported_in_the_words_the_remote_connector_recognizes() {
        // `remote/src/rpc.rs` matches these sentences exactly to answer the
        // phone with `harness_unavailable`.
        for (harness, sentence) in [
            (
                HarnessKind::Codex,
                "codex is not installed or is not on PATH",
            ),
            (
                HarnessKind::Claude,
                "claude is not installed or is not on PATH",
            ),
            (HarnessKind::Grok, "grok is not installed or is not on PATH"),
        ] {
            assert_eq!(harness_missing(harness), sentence);
        }
    }

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
            .arg(editor_command(&vim, &file, None).unwrap())
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
    fn the_editor_opens_at_the_line_a_link_names() {
        let vim = Path::new("/usr/bin/vim");
        let file = Path::new("/p/a b.rs");
        let command = |line| editor_command(vim, file, line).unwrap();
        assert!(
            command(Some(120)).contains(" +120 -- "),
            "{}",
            command(Some(120))
        );
        // Line 0 means no line; the file name still follows `--`, whatever it holds.
        for plain in [command(None), command(Some(0))] {
            assert!(!plain.contains('+'), "{plain}");
            assert!(plain.contains(" -- "), "{plain}");
        }
        let hostile = editor_command(vim, Path::new("/p/+9 ; rm"), Some(3)).unwrap();
        assert!(hostile.contains(" +3 -- "), "{hostile}");
    }

    #[test]
    fn one_pane_table_gives_directories_processes_and_prompts() {
        let alpha = "00000000-0000-4000-8000-000000000010";
        let beta = "00000000-0000-4000-8000-000000000011";
        let dead = "00000000-0000-4000-8000-000000000012";
        let table = PaneTable::parse(&format!(
            "{alpha}\t0\t0\t100\t-zsh\t/project with spaces\n\
             {alpha}\t0\t1\t101\tvim\t/other-pane\n\
             {beta}\t0\t0\t200\tcodex\t/project\twith-tab\n\
             {beta}\t1\t0\t201\tzsh\t/other-window\n\
             {dead}\t0\t0\t300\tzsh\t/dead-shell\n\
             unregistered\t0\t0\t400\tzsh\t/unregistered-shell\n\
             nopid\t0\t0\t-\tzsh\t/no-pid\n\
             incomplete\n"
        ));
        assert_eq!(
            table.sessions(),
            HashSet::from([alpha, beta, dead, "unregistered", "nopid"])
        );
        let live = HashSet::from([alpha, beta, "nopid"]);
        // The owned pane only (window 0, pane 0), and a tab survives in a path.
        assert_eq!(
            table.directories(&live),
            BTreeMap::from([
                (alpha.into(), PathBuf::from("/project with spaces")),
                (beta.into(), PathBuf::from("/project\twith-tab")),
                ("nopid".into(), PathBuf::from("/no-pid")),
            ])
        );
        // Every pane counts for its process; the last one listed wins.
        assert_eq!(
            table.roots(&live),
            BTreeMap::from([(alpha.into(), 101), (beta.into(), 201)])
        );
        // Prompts: a login shell's leading dash is ignored, other panes are not asked.
        let candidates = HashSet::from([alpha.to_owned(), beta.to_owned()]);
        assert_eq!(
            table.at_shell_prompt(&candidates, "zsh"),
            HashSet::from([alpha.to_owned()])
        );
        assert!(PaneTable::parse("").sessions().is_empty());
    }

    #[test]
    fn session_activity_reads_the_time_after_the_name() {
        let alpha = "00000000-0000-4000-8000-000000000020";
        let beta = "00000000-0000-4000-8000-000000000021";
        let activity = parse_session_activity(&format!(
            "{alpha}\t1791000000\n{beta}\t0\nbare\nspaced\t 1791000001 \nbad\tsoon\nneg\t-5\n"
        ));
        assert_eq!(activity[alpha], Some(1_791_000_000));
        // Zero, nothing and nonsense are a live session of unknown activity.
        for unknown in [beta, "bare", "bad", "neg"] {
            assert_eq!(activity[unknown], None, "{unknown}");
        }
        assert_eq!(activity["spaced"], Some(1_791_000_001));
        assert_eq!(activity.len(), 6);
        assert!(parse_session_activity("").is_empty());
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
            self.run_in_child_with(name, &[])
        }

        /// As `run_in_child`, with `changes` applied last: a value sets a
        /// variable and `None` removes it.
        fn run_in_child_with(&self, name: &str, changes: &[(&str, Option<&str>)]) -> bool {
            if env::var_os("RIWORK_TEST_ACCOUNT_FIXTURE").is_some() {
                return true;
            }
            // Fake `codex` executables live in `bin`, ahead of any real one.
            let path = env::join_paths(
                std::iter::once(self.0.join("bin"))
                    .chain(env::var_os("PATH").iter().flat_map(env::split_paths)),
            )
            .unwrap();
            let mut child = Command::new(env::current_exe().unwrap());
            child
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
                .env("PATH", path)
                .env("RIWORK_CUA_DRIVER", self.0.join("fake-cua-driver"))
                // A test may itself run inside a RiWork Codex session.
                .env_remove("CODEX_HOME")
                .env_remove("RIWORK_CODEX_ACCOUNT_HOME");
            for (variable, value) in changes {
                match value {
                    Some(value) => child.env(variable, value),
                    None => child.env_remove(variable),
                };
            }
            let output = child.output().unwrap();
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
        let manager = SessionManager {
            home: state.clone(),
            tmux: PathBuf::from("/unused/tmux"),
            socket_name: "unused".into(),
        };
        manager
            .write_registry(&Registry {
                sessions: vec![plain.clone()],
            })
            .unwrap();
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
        // Preference changes never rewrite a saved session's frozen account.
        let saved = manager.registered_session(&plain.id).unwrap();
        assert_eq!(saved.codex_home, Some(account_b.home.clone()));

        fs::remove_dir_all(&account_a.home).unwrap();
        assert_eq!(
            selected_codex_binding(&state, Some(&alpha.id)).unwrap_err(),
            "This saved account's home is missing. Restore or sign in through Orca."
        );
        let saved = manager.registered_session(&plain.id).unwrap();
        assert_eq!(saved.codex_account_id.as_deref(), Some("account-b"));
        assert_eq!(saved.codex_home, Some(account_b.home));
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

    /// A manager whose tmux records every argument it is given, with fake
    /// `codex` and Cua driver executables, for the isolated child process.
    #[cfg(unix)]
    fn recording_launcher(fixture: &AccountFixture, state: &Path) -> (SessionManager, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let executable = |path: PathBuf, body: &str| {
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path
        };
        fs::create_dir_all(fixture.0.join("bin")).unwrap();
        executable(fixture.0.join("bin/codex"), "exit 0");
        executable(fixture.0.join("fake-cua-driver"), "exit 0");
        let capture = fixture.0.join("launch-tmux-argv");
        let tmux = executable(
            fixture.0.join("fake-tmux-launch"),
            &format!(
                "printf '%s\\n' \"$@\" >> {}",
                quote_arg(&capture.to_string_lossy())
            ),
        );
        let manager = SessionManager {
            home: state.to_owned(),
            tmux,
            socket_name: "isolated-launch".into(),
        };
        (manager, capture)
    }

    /// The tmux arguments recorded since the previous call.
    #[cfg(unix)]
    fn take_recorded(capture: &Path) -> Vec<String> {
        let recorded = fs::read_to_string(capture).unwrap_or_default();
        let _ = fs::remove_file(capture);
        recorded.lines().map(str::to_owned).collect()
    }

    #[test]
    #[cfg(unix)]
    fn plain_shell_opens_without_a_home_when_its_account_is_unavailable() {
        use crate::store::{ProjectCodexAccount, Store};
        let fixture = AccountFixture::new();
        if !fixture.run_in_child("plain_shell_opens_without_a_home_when_its_account_is_unavailable")
        {
            return;
        }
        let state = fixture.selected("account-b");
        let (manager, capture) = recording_launcher(&fixture, &state);
        let store = Store::open(&state).unwrap();
        let root = fixture.0.join("stale");
        fs::create_dir_all(&root).unwrap();
        let other_root = fixture.0.join("inheriting");
        fs::create_dir_all(&other_root).unwrap();
        let saved = store.add_project(&root, Some("Saved")).unwrap();
        let inheriting = store.add_project(&other_root, Some("Inheriting")).unwrap();
        store
            .set_project_codex_account(&saved.id, ProjectCodexAccount::Saved("account-a".into()))
            .unwrap();
        let account_a =
            crate::codex_accounts::resolve_launch_binding(&state, Some("account-a")).unwrap();
        let account_b =
            crate::codex_accounts::resolve_launch_binding(&state, Some("account-b")).unwrap();
        let create = |project: &str| manager.create(project.to_owned(), None, root.clone(), None);
        let codex_home = |arguments: &[String]| {
            arguments
                .iter()
                .find_map(|argument| argument.strip_prefix("CODEX_HOME="))
                .map(str::to_owned)
        };

        // Healthy: each project's shell starts on its own effective home.
        create(&saved.id).unwrap();
        assert_eq!(
            codex_home(&take_recorded(&capture)),
            Some(account_a.home.display().to_string())
        );
        create(&inheriting.id).unwrap();
        assert_eq!(
            codex_home(&take_recorded(&capture)),
            Some(account_b.home.display().to_string())
        );

        // Orca is gone: the saved account's home is missing, and the app-level
        // choice that the other project inherits does not exist any more.
        fs::remove_dir_all(&account_a.home).unwrap();
        crate::settings::SettingsStore::open(&state)
            .unwrap()
            .update(|settings| settings.selected_codex_account = Some("deleted".into()))
            .unwrap();
        assert!(selected_codex_binding(&state, Some(&saved.id)).is_err());
        assert!(selected_codex_binding(&state, Some(&inheriting.id)).is_err());
        for project in [&saved.id, &inheriting.id] {
            let shell = create(project).unwrap();
            assert_eq!(shell.harness, None);
            assert_eq!(shell.codex_home, None);
            let arguments = take_recorded(&capture);
            assert!(arguments.iter().any(|argument| argument == "new-session"));
            assert_eq!(codex_home(&arguments), None, "{arguments:?}");
            assert!(
                arguments
                    .iter()
                    .any(|argument| argument == "RIWORK_CODEX_ACCOUNT_HOME=")
            );
        }

        // Codex launches keep failing closed with the reason.
        for (result, reason) in [
            (
                manager.create_harness(
                    saved.id.clone(),
                    None,
                    root.clone(),
                    HarnessKind::Codex,
                    false,
                ),
                "home is missing",
            ),
            (
                manager.create_harness(
                    inheriting.id.clone(),
                    None,
                    root.clone(),
                    HarnessKind::Codex,
                    false,
                ),
                "was not found",
            ),
            (
                manager.orchestrator_create_for_project(saved.id.clone(), root.clone(), None),
                "home is missing",
            ),
        ] {
            let error = result.unwrap_err();
            assert!(error.contains(reason), "{error}");
        }
        assert!(take_recorded(&capture).is_empty(), "nothing was started");
    }

    #[test]
    #[cfg(unix)]
    fn project_launches_resolve_the_project_choice_and_the_global_orchestrator_the_app_choice() {
        use crate::store::{ProjectCodexAccount, Store};
        let fixture = AccountFixture::new();
        if !fixture.run_in_child(
            "project_launches_resolve_the_project_choice_and_the_global_orchestrator_the_app_choice",
        ) {
            return;
        }
        let state = fixture.selected("account-b");
        let (manager, capture) = recording_launcher(&fixture, &state);
        let store = Store::open(&state).unwrap();
        let root = fixture.0.join("launch");
        fs::create_dir_all(&root).unwrap();
        let project = store.add_project(&root, Some("Launch")).unwrap();
        let account_a =
            crate::codex_accounts::resolve_launch_binding(&state, Some("account-a")).unwrap();
        let account_b =
            crate::codex_accounts::resolve_launch_binding(&state, Some("account-b")).unwrap();
        let assert_frozen =
            |shell: &ShellSession, expected: &crate::codex_accounts::CodexAccountBinding| {
                assert_eq!(shell.harness, Some(HarnessKind::Codex));
                assert_eq!(shell.codex_account_id, expected.id);
                assert_eq!(shell.codex_home.as_ref(), Some(&expected.home));
                let saved = manager.registered_session(&shell.id).unwrap();
                assert_eq!(saved.codex_home.as_ref(), Some(&expected.home));
                let arguments = take_recorded(&capture);
                for variable in ["CODEX_HOME", "RIWORK_CODEX_ACCOUNT_HOME"] {
                    assert!(
                        arguments.iter().any(|argument| *argument
                            == format!("{variable}={}", expected.home.display())),
                        "{variable} missing from {arguments:?}"
                    );
                }
            };

        // Inheriting projects follow the app choice (B); an explicit saved
        // account wins over it without touching the app setting.
        let inherited = manager
            .create_harness(
                project.id.clone(),
                None,
                root.clone(),
                HarnessKind::Codex,
                false,
            )
            .unwrap();
        assert_frozen(&inherited, &account_b);
        store
            .set_project_codex_account(&project.id, ProjectCodexAccount::Saved("account-a".into()))
            .unwrap();
        let harness = manager
            .create_harness(
                project.id.clone(),
                None,
                root.clone(),
                HarnessKind::Codex,
                false,
            )
            .unwrap();
        assert_frozen(&harness, &account_a);
        // The first shell keeps B: a preference change never rebinds it.
        assert_eq!(
            manager
                .registered_session(&inherited.id)
                .unwrap()
                .codex_home,
            Some(account_b.home.clone())
        );

        // A project orchestrator uses the project's choice, the global one the app's.
        let orchestrator = manager
            .orchestrator_create_for_project(project.id.clone(), root.clone(), None)
            .unwrap();
        assert_eq!(
            orchestrator.project_id.as_deref(),
            Some(project.id.as_str())
        );
        assert_frozen(&orchestrator, &account_a);
        let global = manager.orchestrator_create(root.clone(), None).unwrap();
        assert_eq!(global.project_id, None);
        assert_frozen(&global, &account_b);

        // System default for the project: no account id, the user's own home.
        store
            .set_project_codex_account(&project.id, ProjectCodexAccount::SystemDefault)
            .unwrap();
        let system = manager
            .create_harness(
                project.id.clone(),
                None,
                root.clone(),
                HarnessKind::Codex,
                false,
            )
            .unwrap();
        assert_eq!(system.codex_account_id, None);
        assert_eq!(
            system.codex_home,
            Some(crate::codex_accounts::default_codex_home().unwrap())
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
            HarnessOptions {
                unrestricted: false,
                inline: false,
            },
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
    fn credential_and_config_commands_are_recognized_but_reads_and_sessions_are_not() {
        let args = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        for (arguments, command) in [
            (&["login"][..], "codex login"),
            (&["login", "--device-auth"], "codex login"),
            (&["login", "--with-api-key"], "codex login"),
            (&["logout"], "codex logout"),
            (&["--profile", "work", "logout"], "codex logout"),
            (&["-c", "a=b", "login"], "codex login"),
            (&["mcp", "add", "docs", "--", "server"], "codex mcp add"),
            (&["mcp", "remove", "docs"], "codex mcp remove"),
            (&["mcp", "login", "docs"], "codex mcp login"),
            (
                &["plugin", "marketplace", "add", "x"],
                "codex plugin marketplace",
            ),
            (&["features", "enable", "x"], "codex features enable"),
        ] {
            assert_eq!(
                codex_home_mutation(&args(arguments)).as_deref(),
                Some(command),
                "{arguments:?}"
            );
        }
        for arguments in [
            &[][..],
            &["login", "status"],
            &["--profile", "work", "login", "status"],
            &["login", "--help"],
            &["logout", "-h"],
            &["--version"],
            &["mcp"],
            &["mcp", "list"],
            &["mcp", "get", "docs"],
            &["plugin"],
            &["plugin", "list"],
            &["features", "list"],
            &["help", "login"],
            &["update"],
            &["exec", "logout"],
            &["resume", "--last"],
            &["fix the login flow"],
            &["--", "logout"],
        ] {
            assert_eq!(codex_home_mutation(&args(arguments)), None, "{arguments:?}");
        }
        let message =
            managed_home_refusal("codex logout", Path::new("/orca/codex-accounts/a/home"));
        assert!(message.contains("`codex logout`"), "{message}");
        assert!(message.contains("Orca"), "{message}");
        assert!(message.contains("CODEX_HOME="), "{message}");
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
                HarnessOptions {
                    unrestricted,
                    inline: false,
                },
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
    fn inline_agents_get_the_screen_flag_before_their_startup_prompt() {
        let executable = Path::new("/Applications/RiWork's App/riwork");
        let home = Path::new("/Users/test/RiWork's State");
        for harness in [HarnessKind::Codex, HarnessKind::Grok, HarnessKind::Claude] {
            let launch = |inline| {
                shell_arguments(
                    &harness_command(
                        harness,
                        HarnessOptions {
                            unrestricted: false,
                            inline,
                        },
                        Path::new("/opt/bin/cli"),
                        executable,
                        home,
                        "uuid",
                        None,
                    )
                    .unwrap(),
                )
            };
            let (full_screen, inline) = (launch(false), launch(true));
            let flag = match harness {
                HarnessKind::Codex => "--no-alt-screen",
                HarnessKind::Grok => "--minimal",
                HarnessKind::Claude => {
                    // Claude Code takes its screen from the environment.
                    assert_eq!(full_screen, inline);
                    continue;
                }
            };
            assert!(!full_screen.iter().any(|a| a == flag));
            // The flag is the only difference. Codex keeps its prompt last.
            let position = inline.iter().position(|a| a == flag).unwrap();
            let mut without = inline.clone();
            without.remove(position);
            assert_eq!(without, full_screen, "{harness:?}");
            assert_eq!(inline.iter().filter(|a| *a == flag).count(), 1);
            if harness == HarnessKind::Codex {
                assert_eq!(position, inline.len() - 2);
                assert!(inline.last().unwrap().contains(CUA_GUIDANCE));
            }
        }
    }

    #[test]
    fn inline_orchestrators_get_the_screen_flag_before_their_prompt() {
        let launch = |inline| {
            shell_arguments(&orchestrator_command(
                Path::new("/opt/bin/codex"),
                Path::new("/Users/test/context"),
                Path::new("/Users/test/state"),
                Path::new("/Users/test/skill/SKILL.md"),
                Path::new("/Applications/RiWork App/riwork"),
                None,
                None,
                "global-orchestrator-pane",
                None,
                inline,
            ))
        };
        let (full_screen, inline) = (launch(false), launch(true));
        assert!(!full_screen.iter().any(|a| a == "--no-alt-screen"));
        assert_eq!(inline.len(), full_screen.len() + 1);
        assert_eq!(inline[inline.len() - 2], "--no-alt-screen");
        assert_eq!(inline.last(), full_screen.last());
    }

    #[test]
    fn wrappers_add_the_screen_flag_only_to_interactive_sessions() {
        let args = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        let proxied = |harness, original: &[&str]| {
            let original = args(original);
            let launched = original.clone();
            with_inline_arguments(harness, &original, launched)
        };
        // Codex: the interactive screen is the bare command, `resume`, `fork`
        // or a prompt; the flag lands after the options and before any `--`.
        for original in [
            &[][..],
            &["resume", "--last"],
            &["resume", "0199-session"],
            &["fork", "--last"],
            &["--model", "o3", "fix the failing test"],
            &["-c", "a=b", "resume"],
            &["--", "--literal prompt"],
        ] {
            let result = proxied(HarnessKind::Codex, original);
            let end = original
                .iter()
                .position(|a| *a == "--")
                .unwrap_or(original.len());
            let mut expected = args(&original[..end]);
            expected.push("--no-alt-screen".to_owned());
            expected.extend(args(&original[end..]));
            assert_eq!(result, expected, "{original:?}");
        }
        // Everything else, including a bare word that could be a subcommand
        // this build does not know, is left as typed.
        for original in [
            &["exec", "list the files"][..],
            &["e", "-c", "a=b", "do it"],
            &["exec", "resume", "--last"],
            &["review"],
            &["mcp", "list"],
            &["login", "status"],
            &["app-server"],
            &["cloud"],
            &["hello"],
            &["--help"],
            &["resume", "--help"],
            &["--version"],
        ] {
            assert_eq!(
                proxied(HarnessKind::Codex, original),
                args(original),
                "{original:?}"
            );
        }
        // The user's own flag is not repeated; clap rejects a doubled switch.
        assert_eq!(
            proxied(HarnessKind::Codex, &["resume", "--no-alt-screen"]),
            args(&["resume", "--no-alt-screen"])
        );

        // Grok: every screen but the single-turn modes and the utilities.
        for original in [
            &[][..],
            &["fix the bug"],
            &["--resume"],
            &["-r", "title"],
            &["--model", "grok-4", "--cwd", "/tmp/project"],
            &["--worktree=feat", "create this feature"],
        ] {
            let mut expected = args(original);
            expected.push("--minimal".to_owned());
            assert_eq!(
                proxied(HarnessKind::Grok, original),
                expected,
                "{original:?}"
            );
        }
        for original in [
            &["-p", "say hi"][..],
            &["--single=say hi"],
            &["--prompt-file", "/tmp/prompt.txt"],
            &["--prompt-json", "[]"],
            &["--output-format", "json", "--single", "x"],
            &["models"],
            &["doctor"],
            &["--version"],
            &["--help"],
            // The caller already chose a screen.
            &["--minimal"],
            &["--fullscreen"],
            &["--no-alt-screen"],
            &["--resume", "--fullscreen"],
        ] {
            assert_eq!(
                proxied(HarnessKind::Grok, original),
                args(original),
                "{original:?}"
            );
        }

        // Claude Code has no such flag; it is steered by the environment.
        assert_eq!(proxied(HarnessKind::Claude, &[]), Vec::<String>::new());
        assert_eq!(
            proxied(HarnessKind::Claude, &["--resume"]),
            args(&["--resume"])
        );
    }

    #[test]
    fn wrappers_flag_the_arguments_they_finally_pass_on() {
        // The flag goes into the list the wrapper builds, so it follows the
        // wrapper's own options and the startup prompt it supplies.
        let executable = Path::new("/Applications/RiWork/riwork");
        let home = Path::new("/Users/test/RiWork State");
        let original = vec!["resume".to_owned(), "--last".to_owned()];
        let proxied = with_inline_arguments(
            HarnessKind::Codex,
            &original,
            cua_proxy_arguments(HarnessKind::Codex, &original, executable, home, None, None),
        );
        assert_eq!(&proxied[..2], &original[..]);
        assert_eq!(proxied.last().unwrap(), "--no-alt-screen");
        assert_eq!(
            proxied.iter().filter(|a| *a == "--no-alt-screen").count(),
            1
        );
        assert_codex_cua_arguments(&proxied, executable, home);

        let bare = with_inline_arguments(
            HarnessKind::Codex,
            &[],
            cua_proxy_arguments(HarnessKind::Codex, &[], executable, home, None, None),
        );
        assert_eq!(bare.last().unwrap(), "--no-alt-screen");
        assert!(
            bare[bare.len() - 2].contains("wait for the user's objective"),
            "the startup prompt stays the positional argument"
        );
    }

    #[test]
    fn only_claude_is_steered_by_the_environment() {
        let args = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            inline_environment(HarnessKind::Claude, &[]),
            Some(("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN", "1"))
        );
        assert_eq!(
            inline_environment(HarnessKind::Claude, &args(&["--resume"])),
            Some(CLAUDE_MAIN_SCREEN)
        );
        for utility in [&["--version"][..], &["mcp", "list"], &["doctor"]] {
            assert_eq!(
                inline_environment(HarnessKind::Claude, &args(utility)),
                None,
                "{utility:?}"
            );
        }
        assert_eq!(inline_environment(HarnessKind::Codex, &[]), None);
        assert_eq!(inline_environment(HarnessKind::Grok, &[]), None);
    }

    #[test]
    fn claude_settings_keep_telemetry_bound_to_the_shell() {
        let program = Path::new("/Users/test/Claude CLI/claude");
        let telemetry = Path::new("/Users/test/RiWork's App/riwork");
        let id = "00000000-0000-4000-8000-000000000002";
        for unrestricted in [false, true] {
            let command = harness_command(
                HarnessKind::Claude,
                HarnessOptions {
                    unrestricted,
                    inline: false,
                },
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
            for event in [
                "UserPromptSubmit",
                "Stop",
                "SessionStart",
                "SubagentStart",
                "SubagentStop",
            ] {
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
                HarnessOptions {
                    unrestricted: false,
                    inline: false,
                },
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
    fn a_custom_cua_driver_reaches_every_harness_mcp_server() {
        let executable = Path::new("/Applications/RiWork's App/riwork");
        let home = Path::new("/Users/test/RiWork State");
        let driver = Path::new("/Users/test/dev \"cua\"\\driver/cua-driver");
        let key = "mcp_servers.cua-driver.env.RIWORK_CUA_DRIVER=";
        let codex = |custom: Option<&Path>| {
            let arguments =
                cua_harness_arguments_with(HarnessKind::Codex, executable, home, custom);
            arguments
                .windows(2)
                .filter(|pair| pair[0] == "-c")
                .find_map(|pair| pair[1].strip_prefix(key).map(str::to_owned))
        };
        assert_eq!(
            serde_json::from_str::<String>(&codex(Some(driver)).unwrap()).unwrap(),
            driver.to_string_lossy()
        );
        assert_eq!(codex(None), None);

        let claude = |custom: Option<&Path>| {
            let arguments =
                cua_harness_arguments_with(HarnessKind::Claude, executable, home, custom);
            let index = arguments
                .iter()
                .position(|arg| arg == "--mcp-config")
                .unwrap();
            let config: serde_json::Value = serde_json::from_str(&arguments[index + 1]).unwrap();
            config["mcpServers"]["cua-driver"]["env"].clone()
        };
        assert_eq!(
            claude(Some(driver))["RIWORK_CUA_DRIVER"],
            driver.to_string_lossy().as_ref()
        );
        assert_eq!(
            claude(Some(driver))["RIWORK_HOME"],
            home.to_string_lossy().as_ref()
        );
        assert!(claude(None).get("RIWORK_CUA_DRIVER").is_none());

        let with_driver = grok_agent_definition(executable, home, Some(driver));
        assert!(with_driver.contains(&format!(
            "      RIWORK_HOME: {}\n      RIWORK_CUA_DRIVER: {}\n---\n",
            toml_string(&home.to_string_lossy()),
            toml_string(&driver.to_string_lossy())
        )));
        assert!(!grok_agent_definition(executable, home, None).contains("RIWORK_CUA_DRIVER"));
        // Launches with different drivers keep separate agent definitions.
        assert_ne!(
            grok_agent_path(home, executable, Some(driver)),
            grok_agent_path(home, executable, None)
        );
        assert_eq!(
            cua_harness_arguments_with(HarnessKind::Grok, executable, home, Some(driver))[1],
            grok_agent_path(home, executable, Some(driver)).to_string_lossy()
        );
    }

    #[test]
    fn managed_wrappers_pass_utilities_through_but_integrate_sessions() {
        for harness in [HarnessKind::Codex, HarnessKind::Claude, HarnessKind::Grok] {
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
        for argument in ["inspect", "models", "usage", "update"] {
            assert!(harness_utility_invocation(
                HarnessKind::Grok,
                &[argument.to_owned()]
            ));
        }
    }

    #[test]
    fn grok_launch_uses_a_session_scoped_cua_agent() {
        let executable = Path::new("/Applications/RiWork's App/riwork");
        let home = Path::new("/Users/test/RiWork State");
        for unrestricted in [false, true] {
            let command = harness_command(
                HarnessKind::Grok,
                HarnessOptions {
                    unrestricted,
                    inline: false,
                },
                Path::new("/Users/test/.grok/bin/grok"),
                executable,
                home,
                "shell-id",
                None,
            )
            .unwrap();
            let arguments = shell_arguments(&command);
            assert_eq!(arguments[0], "/Users/test/.grok/bin/grok");
            assert_eq!(arguments[1], "--agent");
            assert_eq!(
                arguments[2],
                grok_agent_path(
                    home,
                    executable,
                    crate::cua::driver_override_for_harness().as_deref()
                )
                .to_string_lossy()
            );
            assert_eq!(
                arguments.contains(&"--always-approve".to_owned()),
                unrestricted
            );
        }
        let definition = grok_agent_definition(executable, home, None);
        assert!(definition.contains("mcpServers:\n  - name: cua-driver\n"));
        assert!(definition.contains(&format!(
            "    command: {}\n",
            toml_string(&executable.to_string_lossy())
        )));
        assert!(definition.contains(&format!(
            "      RIWORK_HOME: {}\n",
            toml_string(&home.to_string_lossy())
        )));
        assert!(definition.contains(CUA_GUIDANCE));
        assert!(!definition.contains("startup_timeout"), "{definition}");
    }

    #[test]
    fn grok_timeout_environment_uses_120_seconds_only_when_both_defaults_are_unset() {
        assert_eq!(
            grok_mcp_timeout_environment(EnvText::Absent, EnvText::Absent),
            vec![(
                "GROK_MCP_STARTUP_TIMEOUT_SECS",
                GROK_MCP_STARTUP_BUDGET_SECS.to_owned()
            )]
        );
        assert_eq!(
            grok_mcp_timeout_environment(EnvText::Value("45".to_owned()), EnvText::Absent),
            vec![("GROK_MCP_STARTUP_TIMEOUT_SECS", "45".to_owned())]
        );
        assert_eq!(
            grok_mcp_timeout_environment(EnvText::Absent, EnvText::Value("15000".to_owned())),
            vec![("MCP_TIMEOUT", "15000".to_owned())]
        );
        assert_eq!(
            grok_mcp_timeout_environment(
                EnvText::Value("45".to_owned()),
                EnvText::Value("15000".to_owned())
            ),
            vec![
                ("GROK_MCP_STARTUP_TIMEOUT_SECS", "45".to_owned()),
                ("MCP_TIMEOUT", "15000".to_owned())
            ]
        );
        // An unreadable value is already set, so it is left alone.
        assert!(grok_mcp_timeout_environment(EnvText::Unreadable, EnvText::Absent).is_empty());
        assert_eq!(
            grok_mcp_timeout_environment(EnvText::Absent, EnvText::Unreadable),
            Vec::<(&str, String)>::new()
        );
    }

    #[cfg(unix)]
    fn install_grok_preflight_driver(path: &Path, marker: &Path, calls: &Path, mode: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = r#"#!/usr/bin/python3
import json, os, sys, time
marker = "MARKER_PATH"
calls = "CALLS_PATH"
mode = "MODE"
def note(action):
    with open(calls, "a", encoding="utf-8") as handle:
        handle.write(action + "\n")
cmd = sys.argv[1] if len(sys.argv) > 1 else ""
note(cmd or "none")
if cmd == "--version":
    print("cua-driver test")
elif cmd == "status":
    if os.path.exists(marker):
        print("Cua Driver daemon is running")
        raise SystemExit(0)
    raise SystemExit(1)
elif cmd == "serve":
    with open(marker, "w", encoding="utf-8") as handle:
        handle.write(str(os.getpid()))
    while True:
        time.sleep(0.2)
elif cmd == "mcp":
    if mode == "fail":
        print("socket refused", file=sys.stderr)
        raise SystemExit(2)
    for line in sys.stdin:
        message = json.loads(line)
        method = message.get("method")
        if method == "initialize":
            body = {"jsonrpc": "2.0", "id": message.get("id"), "result": {"capabilities": {"tools": {}}}}
            print(json.dumps(body), flush=True)
        elif method == "tools/list":
            body = {"jsonrpc": "2.0", "id": message.get("id"), "result": {"tools": [{"name": "probe_tool"}]}}
            print(json.dumps(body), flush=True)
else:
    raise SystemExit(99)
"#;
        let script = script
            .replace("MARKER_PATH", &marker.display().to_string())
            .replace("CALLS_PATH", &calls.display().to_string())
            .replace("MODE", mode);
        fs::write(path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    struct StopPreflightDaemon(PathBuf);

    #[cfg(unix)]
    impl Drop for StopPreflightDaemon {
        fn drop(&mut self) {
            let Ok(pid) = fs::read_to_string(&self.0) else {
                return;
            };
            let pid = pid.trim();
            if !pid.is_empty() {
                let _ = Command::new("/bin/kill").args(["-9", pid]).status();
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn grok_launch_preflights_the_driver_and_sets_the_timeout_when_unset() {
        const NAME: &str = "grok_launch_preflights_the_driver_and_sets_the_timeout_when_unset";
        let fixture = AccountFixture::new();
        if !fixture.run_in_child_with(
            NAME,
            &[
                ("GROK_MCP_STARTUP_TIMEOUT_SECS", None),
                ("MCP_TIMEOUT", None),
            ],
        ) {
            return;
        }
        let state = fixture.selected("account-a");
        let (manager, capture) = recording_launcher(&fixture, &state);
        executable_script(&fixture.0.join("bin/grok"), "exit 0");
        let marker = fixture.0.join("daemon-pid");
        let calls = fixture.0.join("driver-calls");
        install_grok_preflight_driver(&fixture.0.join("fake-cua-driver"), &marker, &calls, "ready");
        let _stop = StopPreflightDaemon(marker);
        let cwd = fixture.0.join("grok-work");
        fs::create_dir_all(&cwd).unwrap();
        let session = manager
            .create_harness(
                Uuid::new_v4().to_string(),
                None,
                cwd,
                HarnessKind::Grok,
                false,
            )
            .unwrap();
        assert_eq!(session.harness, Some(HarnessKind::Grok));
        let command = session.command.unwrap();
        assert!(command.contains("--agent"), "{command}");
        assert!(!command.contains("startup_timeout"), "{command}");
        let arguments = take_recorded(&capture);
        assert!(contains_sequence(
            &arguments,
            &["-e", "GROK_MCP_STARTUP_TIMEOUT_SECS=120"]
        ));
        assert!(contains_sequence(
            &arguments,
            &[";", "set-environment", "-gu", "MCP_TIMEOUT"]
        ));
        assert!(
            !arguments
                .iter()
                .any(|argument| argument.contains("MCP_TIMEOUT="))
        );
        let calls = fs::read_to_string(&calls).unwrap();
        assert!(calls.contains("serve\n"), "{calls}");
        assert!(calls.contains("mcp\n"), "{calls}");
        let agent = fs::read_dir(state.join("cua"))
            .unwrap()
            .flatten()
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("grok-agent-")
            })
            .unwrap()
            .path();
        let definition = fs::read_to_string(agent).unwrap();
        assert!(definition.contains("name: cua-driver"), "{definition}");
        assert!(!definition.contains("startup_timeout"), "{definition}");
    }

    #[test]
    #[cfg(unix)]
    fn grok_launch_forwards_a_user_timeout_and_refuses_to_start_when_mcp_fails() {
        const NAME: &str =
            "grok_launch_forwards_a_user_timeout_and_refuses_to_start_when_mcp_fails";
        let fixture = AccountFixture::new();
        if !fixture.run_in_child_with(
            NAME,
            &[
                ("GROK_MCP_STARTUP_TIMEOUT_SECS", Some("45")),
                ("MCP_TIMEOUT", Some("15000")),
            ],
        ) {
            return;
        }
        let state = fixture.selected("account-a");
        let (manager, capture) = recording_launcher(&fixture, &state);
        executable_script(&fixture.0.join("bin/grok"), "exit 0");
        let cwd = fixture.0.join("grok-work");
        fs::create_dir_all(&cwd).unwrap();
        let marker = fixture.0.join("daemon-pid");
        install_grok_preflight_driver(
            &fixture.0.join("fake-cua-driver"),
            &marker,
            &fixture.0.join("driver-calls"),
            "ready",
        );
        let _stop = StopPreflightDaemon(marker.clone());
        manager
            .create_harness(
                Uuid::new_v4().to_string(),
                None,
                cwd.clone(),
                HarnessKind::Grok,
                false,
            )
            .unwrap();
        let arguments = take_recorded(&capture);
        assert!(contains_sequence(
            &arguments,
            &["-e", "GROK_MCP_STARTUP_TIMEOUT_SECS=45"]
        ));
        assert!(contains_sequence(&arguments, &["-e", "MCP_TIMEOUT=15000"]));
        assert!(!arguments.iter().any(|argument| argument.contains("=120")));

        install_grok_preflight_driver(
            &fixture.0.join("fake-cua-driver"),
            &marker,
            &fixture.0.join("failed-calls"),
            "fail",
        );
        let error = manager
            .create_harness(
                Uuid::new_v4().to_string(),
                None,
                cwd,
                HarnessKind::Grok,
                false,
            )
            .unwrap_err();
        assert!(error.contains("socket refused"), "{error}");
        assert!(error.contains("30 seconds"), "{error}");
        assert!(error.contains("driver.log"), "{error}");
        let recorded = fs::read_to_string(&capture).unwrap_or_default();
        assert!(
            !recorded.lines().any(|line| line == "new-session"),
            "{recorded}"
        );
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
            for harness in ["codex", "claude", "grok"] {
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
                 command -v grok; grok 'z z'; \
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
                    if result.lines().count() == 10 {
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
            assert_eq!(lines[4], expected_directory.join("grok").to_string_lossy());
            assert_eq!(lines[5], format!("{expected_label}-grok:z z"));
            assert_eq!(lines[6], final_directory.to_string_lossy());
            assert_eq!(
                lines[7],
                final_directory.join(".zsh_history").to_string_lossy()
            );
            assert_eq!(lines[8], "env profile rc login");
            assert_ne!(
                lines[9], "0",
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
    fn only_profile_variables_absent_from_the_launch_are_cleared_from_the_server() {
        let os = |value: &str| Some(std::ffi::OsString::from(value));
        assert_eq!(
            stale_profile_variables(&[("CODEX_HOME", None), ("CLAUDE_CONFIG_DIR", None)]),
            ["CODEX_HOME", "CLAUDE_CONFIG_DIR"]
        );
        assert_eq!(
            stale_profile_variables(&[
                ("CODEX_HOME", os("/Users/test/codex A")),
                ("CLAUDE_CONFIG_DIR", None)
            ]),
            ["CLAUDE_CONFIG_DIR"]
        );
        assert!(
            stale_profile_variables(&[
                ("CODEX_HOME", os("/Users/test/codex A")),
                ("CLAUDE_CONFIG_DIR", os("/Users/test/claude A"))
            ])
            .is_empty()
        );
    }

    #[test]
    fn unsetting_in_the_command_names_only_the_variables_given() {
        let command = "exec /opt/bin/codex";
        assert_eq!(unset_in_command(command, &[]), command);
        assert_eq!(
            shell_arguments(&unset_in_command(command, &["A", "B"])),
            ["/usr/bin/env", "-u", "A", "-u", "B", "/opt/bin/codex"]
        );
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
    fn orchestrator_startup_points_at_the_installed_skill_and_stays_small() {
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
            false,
        );
        // tmux refuses a command line of about 16 KB. The skill used to be inlined,
        // which left little room to grow; the launch must not depend on its size.
        assert!(command.len() < 4 * 1024, "{} bytes", command.len());
        assert!(!command.contains(ORCHESTRATOR_SKILL.lines().nth(5).unwrap()));
        let arguments = shell_arguments(&command);
        assert_eq!(arguments[0], "/opt/bin/codex");
        assert_eq!(arguments[1], "--cd");
        assert_eq!(arguments[2], context.to_string_lossy());
        assert_codex_user_permissions_preserved(&arguments);
        assert_codex_cua_arguments(&arguments, executable, state_home);
        assert_codex_shell_environment(&arguments, state_home, "global-orchestrator-pane");
        let prompt = arguments.last().unwrap();
        assert!(prompt.starts_with("$riwork-orchestrator\n"));
        assert!(!prompt.contains(ORCHESTRATOR_SKILL));
        assert!(!prompt.contains("<riwork-orchestrator-skill>"));
        assert!(prompt.contains("read its installed SKILL.md in full"));
        assert!(prompt.contains(CUA_GUIDANCE));
        assert!(prompt.contains("wait for the user's objective"));
        assert!(prompt.contains(&quote_arg(&skill.to_string_lossy())));
        assert!(prompt.contains(&quote_arg(&executable.to_string_lossy())));
        assert!(prompt.contains("Scope: global."));
        assert!(prompt.contains("You have no project or worktree ownership."));
        // A pane that is already running receives the skill itself, by paste.
        let pasted = orchestrator_prompt(&skill, executable, None, None, true);
        assert!(pasted.contains(ORCHESTRATOR_SKILL));
        assert!(pasted.contains("<riwork-orchestrator-skill>"));
        assert!(pasted.contains(CUA_GUIDANCE));
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
            false,
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
        assert!(prompt.contains(&quote_arg(&skill.to_string_lossy())));
        assert!(!prompt.contains(ORCHESTRATOR_SKILL));
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

    fn scratch(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("riwork-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    #[cfg(unix)]
    fn executable_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn contains_sequence(arguments: &[String], sequence: &[&str]) -> bool {
        arguments.windows(sequence.len()).any(|window| {
            window
                .iter()
                .map(String::as_str)
                .eq(sequence.iter().copied())
        })
    }

    #[test]
    fn tmux_arguments_escape_a_trailing_semicolon_and_directories_escape_hashes() {
        for (argument, sent) in [
            ("plain", "plain"),
            ("x;", "x\\;"),
            (";", "\\;"),
            ("x\\;", "x\\\\;"),
            ("a;b", "a;b"),
            ("find . -exec ls {} \\;", "find . -exec ls {} \\\\;"),
            ("ends with hash#", "ends with hash#"),
        ] {
            assert_eq!(tmux_argument(argument), sent, "{argument}");
        }
        for (path, sent) in [
            ("/dev/C#Tools", "/dev/C##Tools"),
            ("/dev/#{pane_id}", "/dev/##{pane_id}"),
            ("/dev/##", "/dev/####"),
            ("/dev/none", "/dev/none"),
        ] {
            assert_eq!(tmux_directory(path), sent);
        }
        // A directory's escapes compose: `#` first, then the trailing `;`.
        assert_eq!(tmux_argument(&tmux_directory("/dev/#T;")), "/dev/##T\\;");
    }

    #[cfg(unix)]
    #[test]
    fn only_a_missing_server_counts_as_no_tmux_server() {
        use std::os::unix::process::ExitStatusExt;
        let failed = |stderr: &str| Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        for stderr in [
            "no server running on /private/tmp/tmux-501/riwork-0\n",
            "error connecting to /private/tmp/tmux-501/riwork-0 (No such file or directory)\n",
        ] {
            assert!(no_tmux_server(&failed(stderr)), "{stderr}");
        }
        for stderr in [
            "error connecting to /very/long/path/tmux-501/riwork-0 (File name too long)\n",
            "error connecting to /private/tmp/tmux-501/riwork-0 (Permission denied)\n",
            "protocol version mismatch (client 8, server 7)\n",
            "",
        ] {
            assert!(!no_tmux_server(&failed(stderr)), "{stderr}");
        }
    }

    #[test]
    fn tmux_calls_are_named_after_the_last_chained_command() {
        assert_eq!(
            tmux_label(&["list-sessions", "-F", "#{session_name}"]),
            "tmux list-sessions"
        );
        assert_eq!(
            tmux_label(&[
                "start-server",
                ";",
                "set-option",
                "-g",
                "x",
                "1",
                ";",
                "new-session",
                "-d"
            ]),
            "tmux new-session"
        );
        assert_eq!(tmux_label(&[]), "tmux ");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_state_directory_is_owner_only_and_an_existing_one_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        if find_tmux().is_none() {
            return;
        }
        let root = scratch("private-state");
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let fresh = root.join("parent/state");
        SessionManager::at(fresh.clone()).unwrap();
        assert_eq!(mode(&fresh), 0o700);
        let existing = root.join("shared");
        fs::create_dir(&existing).unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).unwrap();
        SessionManager::at(existing.clone()).unwrap();
        assert_eq!(mode(&existing), 0o755);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn harness_discovery_runs_the_path_entry_not_what_it_resolves_to() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = scratch("harness-discovery");
        let shims = root.join("riwork-shims");
        fs::create_dir(&shims).unwrap();
        let directory = |name: &str| {
            let path = root.join(name);
            fs::create_dir(&path).unwrap();
            path
        };
        // mise and Volta install `shims/codex -> mise` and choose the program
        // by the name they are started under.
        let mise_shims = directory("mise-shims");
        let mise = root.join("mise");
        executable_script(&mise, "basename \"$0\"");
        symlink(&mise, mise_shims.join("codex")).unwrap();
        let found =
            find_harness_program_in(HarnessKind::Codex, &shims, [mise_shims.clone()]).unwrap();
        assert_eq!(found, mise_shims.join("codex"));
        assert_ne!(found.canonicalize().unwrap(), found);
        let output = Command::new(&found).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "codex\n");

        // Without the executable bit, a file is skipped in favour of a later one.
        let plain = directory("not-executable");
        fs::write(plain.join("codex"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(plain.join("codex"), fs::Permissions::from_mode(0o644)).unwrap();
        let real = directory("real");
        executable_script(&real.join("codex"), "exit 0");
        assert_eq!(
            find_harness_program_in(HarnessKind::Codex, &shims, [plain.clone(), real.clone()]),
            Some(real.join("codex"))
        );

        // A directory, a link to RiWork's own launcher and another state
        // directory's launcher are not the CLI.
        let folder = directory("folder");
        fs::create_dir(folder.join("codex")).unwrap();
        executable_script(&shims.join("codex"), "# RiWork Cua harness shim");
        let linked = directory("linked");
        symlink(shims.join("codex"), linked.join("codex")).unwrap();
        let foreign = directory("foreign");
        executable_script(&foreign.join("codex"), "# RiWork Cua harness shim\nexit 1");
        assert_eq!(
            find_harness_program_in(
                HarnessKind::Codex,
                &shims,
                [folder, linked, foreign, root.join("missing"), real.clone()]
            ),
            Some(real.join("codex"))
        );
        assert_eq!(
            find_harness_program_in(HarnessKind::Codex, &shims, [root.join("missing")]),
            None
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_login_shell_is_asked_only_when_the_usual_directories_find_nothing() {
        let root = scratch("login-fallback");
        let shims = root.join("riwork-shims");
        let usual = root.join("usual");
        let nvm = root.join("nvm/bin");
        for directory in [&shims, &usual, &nvm] {
            fs::create_dir_all(directory).unwrap();
        }
        executable_script(&nvm.join("claude"), "exit 0");
        let asked = std::cell::Cell::new(0);
        let login = || {
            asked.set(asked.get() + 1);
            vec![nvm.clone()]
        };
        // Found where the app already looks: the shell is never started.
        executable_script(&usual.join("codex"), "exit 0");
        assert_eq!(
            find_harness_program_via(HarnessKind::Codex, &shims, vec![usual.clone()], login),
            Some(usual.join("codex"))
        );
        assert_eq!(asked.get(), 0);
        // A CLI that only the login shell's PATH holds is found there.
        assert_eq!(
            find_harness_program_via(HarnessKind::Claude, &shims, vec![usual.clone()], login),
            Some(nvm.join("claude"))
        );
        assert_eq!(asked.get(), 1);
        assert_eq!(
            find_harness_program_via(HarnessKind::Grok, &shims, vec![usual], login),
            None
        );
        assert_eq!(asked.get(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_path_takes_the_value_between_markers() {
        let root = scratch("login-path");
        let shell = root.join("fake-shell");
        // Chatty startup files, the command line echoed, and output after it.
        executable_script(
            &shell,
            "echo 'welcome to my shell'\n\
             [ \"$1 $2 $3\" = '-l -i -c' ] || { echo \"unexpected flags: $*\" >&2; exit 2; }\n\
             echo \"$4\"\n\
             PATH=\"/opt/nvm/bin:/opt/pnpm:$PATH\"\n\
             eval \"$4\"\n\
             echo 'trailing noise'",
        );
        let path = login_shell_path(&shell, Duration::from_secs(10)).unwrap();
        let path = path.to_string_lossy();
        assert!(path.starts_with("/opt/nvm/bin:/opt/pnpm:"), "{path}");
        assert!(
            !path.contains("noise") && !path.contains("__RIWORK_PATH_"),
            "{path}"
        );

        // A daemon that the startup files leave running holds the shell's
        // output open, and must not hide what the shell printed.
        executable_script(
            &shell,
            "PATH=/opt/late:$PATH\neval \"$4\"\n(sleep 2 &)\necho later noise",
        );
        let path = login_shell_path(&shell, Duration::from_secs(10)).unwrap();
        assert!(path.to_string_lossy().starts_with("/opt/late:"), "{path:?}");

        // Failures are ignored, and a shell that hangs is stopped.
        executable_script(&shell, "exit 3");
        assert_eq!(login_shell_path(&shell, Duration::from_secs(10)), None);
        executable_script(&shell, "exec sleep 30");
        let started = std::time::Instant::now();
        assert_eq!(login_shell_path(&shell, Duration::from_millis(300)), None);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(
            login_shell_path(&root.join("missing"), Duration::from_secs(1)),
            None
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Runs the real zsh the way RiWork does: no terminal, login, interactive.
    #[cfg(unix)]
    #[test]
    fn login_shell_path_reads_a_real_zsh_startup_without_a_terminal() {
        const NAME: &str = "login_shell_path_reads_a_real_zsh_startup_without_a_terminal";
        if !Path::new("/bin/zsh").exists() {
            return;
        }
        let Some(home) = env::var_os("RIWORK_TEST_ZSH_HOME") else {
            let home = scratch("real-zsh");
            fs::write(
                home.join(".zshrc"),
                "echo 'rc noise'\nexport PATH=\"/opt/fake-nvm/bin:$PATH\"\n",
            )
            .unwrap();
            let output = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    &format!("sessions::tests::{NAME}"),
                    "--nocapture",
                ])
                .env("RIWORK_TEST_ZSH_HOME", &home)
                .env("HOME", &home)
                .env_remove("ZDOTDIR")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            fs::remove_dir_all(home).unwrap();
            return;
        };
        let _ = home;
        let path = login_shell_path(Path::new("/bin/zsh"), Duration::from_secs(30)).unwrap();
        let path = path.to_string_lossy();
        assert!(path.starts_with("/opt/fake-nvm/bin:"), "{path}");
        assert!(!path.contains("noise"), "{path}");
    }

    #[test]
    fn harness_path_lists_shims_then_the_login_shell_then_this_process() {
        let shim = Path::new("/state/cua/harness-bin");
        let login = [
            PathBuf::from("/Users/me/.nvm/versions/node/v22/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            shim.to_path_buf(),
        ];
        let path = path_with_harness_shims(shim, &login).unwrap();
        let directories: Vec<PathBuf> = env::split_paths(&path).collect();
        assert_eq!(directories[0], shim);
        assert_eq!(directories[1], login[0]);
        assert_eq!(directories[2], login[1]);
        for once in [shim, Path::new("/opt/homebrew/bin")] {
            assert_eq!(
                directories.iter().filter(|d| d.as_path() == once).count(),
                1
            );
        }
        for directory in executable_dirs() {
            assert!(directories.contains(&directory), "{}", directory.display());
        }
        let without = path_with_harness_shims(shim, &[]).unwrap();
        assert_eq!(env::split_paths(&without).next().unwrap(), shim);
    }

    #[test]
    fn grok_utility_aliases_pass_through() {
        for argument in ["v", "version", "disk-usage", "du", "--version", "-v"] {
            assert!(
                harness_utility_invocation(HarnessKind::Grok, &[argument.to_owned()]),
                "{argument}"
            );
        }
        // `v` is only Grok's alias for `version`.
        assert!(!harness_utility_invocation(
            HarnessKind::Codex,
            &["v".to_owned()]
        ));
        assert!(!harness_utility_invocation(
            HarnessKind::Claude,
            &["v".to_owned()]
        ));
    }

    #[cfg(unix)]
    #[test]
    fn grok_agent_install_cleans_up_a_failed_write_and_prunes_old_definitions() {
        let root = scratch("grok-agent");
        let executable = Path::new("/Applications/RiWork/riwork");
        let directory = root.join("cua");
        let current = grok_agent_path(&root, executable, None);
        let names = |suffix: &str| -> Vec<String> {
            fs::read_dir(&directory)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|entry| entry.file_name().to_string_lossy().into_owned())
                        .filter(|name| name.ends_with(suffix))
                        .collect()
                })
                .unwrap_or_default()
        };
        let install = |write: &dyn Fn(&Path, &str) -> std::io::Result<()>| {
            ensure_grok_agent_with(&root, executable, None, write, GROK_AGENT_MAX_AGE)
        };

        // A write that fails after creating the file leaves nothing behind.
        let error = install(&|path, _| {
            fs::write(path, "partial")?;
            Err(std::io::Error::other("no space left"))
        })
        .unwrap_err();
        assert!(error.contains("no space left"), "{error}");
        assert!(names(".tmp").is_empty(), "{:?}", names(".tmp"));
        assert!(!current.exists());
        install(&|path, content| fs::write(path, content)).unwrap();
        assert!(current.is_file());

        // Definitions nobody launched with for a long time go; others stay.
        let age = |path: &Path, days: u64| {
            let modified = SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60);
            OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        };
        let write = |name: &str, days: u64| {
            let path = directory.join(name);
            fs::write(&path, "x").unwrap();
            age(&path, days);
            path
        };
        let old = write("grok-agent-0000000000000001.md", 90);
        let stale_temporary = write("grok-agent-abandoned.tmp", 90);
        let recent = write("grok-agent-0000000000000002.md", 2);
        let unrelated = write("notes.md", 90);
        let other_kind = write("grok-agent-0000000000000003.json", 90);
        age(&current, 90);
        install(&|path, content| fs::write(path, content)).unwrap();
        assert!(!old.exists() && !stale_temporary.exists());
        assert!(recent.exists() && unrelated.exists() && other_kind.exists());
        // The definition in use is never pruned, and its use refreshes it.
        assert!(current.is_file());
        let age_of_current = SystemTime::now()
            .duration_since(fs::metadata(&current).unwrap().modified().unwrap())
            .unwrap();
        assert!(
            age_of_current < Duration::from_secs(60 * 60),
            "{age_of_current:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A launch with a stale server environment, a custom driver and awkward
    /// names, as tmux would receive it. Nothing is started: tmux is a recorder.
    #[test]
    #[cfg(unix)]
    fn a_launch_clears_stale_server_variables_and_escapes_what_tmux_would_reread() {
        const NAME: &str =
            "a_launch_clears_stale_server_variables_and_escapes_what_tmux_would_reread";
        let fixture = AccountFixture::new();
        if !fixture.run_in_child_with(
            NAME,
            &[
                ("CLAUDE_CONFIG_DIR", None),
                ("GROK_HOME", Some("/Users/test/grok home;")),
            ],
        ) {
            return;
        }
        let state = fixture.selected("account-a");
        let (manager, capture) = recording_launcher(&fixture, &state);
        executable_script(&fixture.0.join("bin/claude"), "exit 0");
        let project = Uuid::new_v4().to_string();
        let awkward = fixture.0.join("C#Tools;");
        fs::create_dir(&awkward).unwrap();
        let driver = fixture
            .0
            .join("fake-cua-driver")
            .to_string_lossy()
            .into_owned();

        // A plain shell and a custom command are cleared like a harness is.
        for command in [None, Some("sleep 60;".to_owned())] {
            let session = manager
                .create(project.clone(), None, awkward.clone(), command.clone())
                .unwrap();
            assert_eq!(session.command, command);
            let arguments = take_recorded(&capture);
            // One invocation clears the variables and then creates the session.
            assert!(contains_sequence(
                &arguments,
                &["start-server", ";", "set-environment"]
            ));
            for stale in ["CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
                assert!(
                    contains_sequence(&arguments, &[";", "set-environment", "-gu", stale]),
                    "{stale}: {arguments:?}"
                );
            }
            // GROK_HOME and RIWORK_CUA_DRIVER are set for this launch.
            for kept in ["GROK_HOME", "RIWORK_CUA_DRIVER"] {
                assert!(
                    !contains_sequence(&arguments, &[";", "set-environment", "-gu", kept]),
                    "{kept}: {arguments:?}"
                );
            }
            assert!(contains_sequence(
                &arguments,
                &[
                    ";",
                    "set-option",
                    "-g",
                    "history-limit",
                    "100000",
                    ";",
                    "new-session"
                ]
            ));
            // `#` is doubled in the directory, and a trailing `;` is escaped in
            // every argument that has one.
            let cwd = format!("{}/C##Tools\\;", fixture.0.display());
            assert!(
                contains_sequence(&arguments, &["-c", &cwd]),
                "{arguments:?}"
            );
            assert!(contains_sequence(
                &arguments,
                &["-e", "GROK_HOME=/Users/test/grok home\\;"]
            ));
            assert!(contains_sequence(
                &arguments,
                &["-e", &format!("RIWORK_CUA_DRIVER={driver}")]
            ));
            if command.is_some() {
                assert!(arguments.iter().any(|argument| argument == "sleep 60\\;"));
            }
        }

        // The user's own startup files must still decide these variables, so no
        // `env -u` runs after them.
        let session = manager
            .create_harness(project, None, awkward, HarnessKind::Claude, false)
            .unwrap();
        let command = session.command.unwrap();
        assert!(!command.contains("/usr/bin/env"), "{command}");
        let arguments = take_recorded(&capture);
        assert!(contains_sequence(
            &arguments,
            &[";", "set-environment", "-gu", "CLAUDE_CONFIG_DIR"]
        ));
    }

    /// Which screen an agent starts on is decided when it launches, from the
    /// saved setting: a session that is already running is never touched.
    #[test]
    #[cfg(unix)]
    fn new_agent_sessions_follow_the_inline_setting_at_launch() {
        use crate::store::Store;
        const NAME: &str = "new_agent_sessions_follow_the_inline_setting_at_launch";
        let fixture = AccountFixture::new();
        if !fixture.run_in_child(NAME) {
            return;
        }
        let state = fixture.selected("account-a");
        let (manager, capture) = recording_launcher(&fixture, &state);
        executable_script(&fixture.0.join("bin/claude"), "exit 0");
        let claude_screen = format!("{}={}", CLAUDE_MAIN_SCREEN.0, CLAUDE_MAIN_SCREEN.1);
        let flagged = |arguments: &[String]| arguments.iter().any(|a| a == "--no-alt-screen");

        for inline in [true, false] {
            // Each project has its own orchestrator, which is launched once.
            let root = fixture.0.join(format!("work-{inline}"));
            fs::create_dir(&root).unwrap();
            let project = Store::open(&state)
                .unwrap()
                .add_project(&root, Some("Inline"))
                .unwrap();
            crate::settings::SettingsStore::open(&state)
                .unwrap()
                .update(|settings| settings.agent_inline_mode = inline)
                .unwrap();
            let codex = manager
                .create_harness(
                    project.id.clone(),
                    None,
                    root.clone(),
                    HarnessKind::Codex,
                    false,
                )
                .unwrap();
            assert_eq!(flagged(&shell_arguments(&codex.command.unwrap())), inline);
            assert!(!contains_sequence(
                &take_recorded(&capture),
                &["-e", &claude_screen]
            ));

            let claude = manager
                .create_harness(
                    project.id.clone(),
                    None,
                    root.clone(),
                    HarnessKind::Claude,
                    false,
                )
                .unwrap();
            assert!(!flagged(&shell_arguments(&claude.command.unwrap())));
            assert_eq!(
                contains_sequence(&take_recorded(&capture), &["-e", &claude_screen]),
                inline
            );

            // The orchestrator is Codex under another name.
            let orchestrator = manager
                .orchestrator_create_for_project(project.id.clone(), root.clone(), None)
                .unwrap();
            assert_eq!(
                flagged(&shell_arguments(&orchestrator.command.unwrap())),
                inline
            );
            take_recorded(&capture);

            // A plain shell and a custom command are never steered.
            for command in [None, Some("sleep 60".to_owned())] {
                manager
                    .create(project.id.clone(), None, root.clone(), command)
                    .unwrap();
                assert!(!contains_sequence(
                    &take_recorded(&capture),
                    &["-e", &claude_screen]
                ));
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_launch_without_a_custom_driver_clears_the_servers_copy() {
        const NAME: &str = "a_launch_without_a_custom_driver_clears_the_servers_copy";
        let fixture = AccountFixture::new();
        if !fixture.run_in_child_with(NAME, &[("RIWORK_CUA_DRIVER", None)]) {
            return;
        }
        let state = fixture.selected("account-a");
        let (manager, capture) = recording_launcher(&fixture, &state);
        manager
            .create(
                Uuid::new_v4().to_string(),
                None,
                fixture.0.clone(),
                Some("sleep 60".into()),
            )
            .unwrap();
        let arguments = take_recorded(&capture);
        assert!(contains_sequence(
            &arguments,
            &[";", "set-environment", "-gu", "RIWORK_CUA_DRIVER"]
        ));
        assert!(
            !arguments
                .iter()
                .any(|argument| argument.starts_with("RIWORK_CUA_DRIVER=")),
            "{arguments:?}"
        );
    }

    /// `RIWORK_HOME=` used to make the session manager (but not the store)
    /// create its state in the working directory.
    #[cfg(unix)]
    #[test]
    fn an_empty_riwork_home_means_the_default_state_directory_everywhere() {
        use std::os::unix::fs::PermissionsExt;
        const NAME: &str = "an_empty_riwork_home_means_the_default_state_directory_everywhere";
        if find_tmux().is_none() {
            return;
        }
        let Some(home) = env::var_os("RIWORK_TEST_EMPTY_HOME") else {
            let home = scratch("empty-riwork-home");
            let output = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    &format!("sessions::tests::{NAME}"),
                    "--nocapture",
                ])
                .env("RIWORK_TEST_EMPTY_HOME", &home)
                .env("HOME", &home)
                .env("RIWORK_HOME", "")
                .current_dir(&home)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            // Nothing landed in the working directory.
            let names: Vec<_> = fs::read_dir(&home)
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(names, [".local"], "{names:?}");
            fs::remove_dir_all(home).unwrap();
            return;
        };
        let expected = PathBuf::from(home).join(".local/share/riwork");
        let manager = SessionManager::open_default().unwrap();
        assert_eq!(manager.state_home(), expected.canonicalize().unwrap());
        assert_eq!(
            fs::metadata(&expected).unwrap().permissions().mode() & 0o777,
            0o700
        );
        crate::cua::CuaManager::open_default().unwrap();
        assert!(crate::layouts::LayoutStore::open_default().is_ok());
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
