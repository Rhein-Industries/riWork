//! A deliberately separate worker protocol: older reload workers must reject
//! an account switch instead of silently resuming under the old account.
use super::*;
use crate::{
    activity::ActivityTracker, codex_accounts::CodexAccountBinding, sessions::ShellSession,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct AccountSwitch {
    source: ShellSession,
    pane_identity: String,
    target_home: PathBuf,
    target_id: Option<String>,
    expected_email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) supervised_policy: Option<SupervisedPolicy>,
}

/// This is evidence of the one supported supervised policy, not a configurable
/// permission template. Every policy field is checked before constructing it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct SupervisedPolicy {
    turn_id: String,
    cwd: PathBuf,
}

impl SupervisedPolicy {
    pub(super) fn arguments(&self) -> Vec<String> {
        [
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "on-request",
            "-c",
            "sandbox_mode=\"workspace-write\"",
            "-c",
            "approval_policy=\"on-request\"",
            "-c",
            "approvals_reviewer=\"user\"",
            "-c",
            "default_permissions=\":workspace\"",
            "-c",
            "sandbox_workspace_write.writable_roots=[]",
            "-c",
            "sandbox_workspace_write.network_access=false",
            "-c",
            "sandbox_workspace_write.exclude_tmpdir_env_var=false",
            "-c",
            "sandbox_workspace_write.exclude_slash_tmp=false",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

/// The extension deliberately accepts only the observed standard permission
/// shape. An added permission field must be reviewed, not silently discarded.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // Ignored non-permission fields still form the strict schema.
struct TurnContext {
    turn_id: String,
    root_turn_id: String,
    cwd: PathBuf,
    approval_policy: serde_json::Value,
    sandbox_policy: serde_json::Value,
    permission_profile: serde_json::Value,
    file_system_sandbox_policy: serde_json::Value,
    active_permission_profile: serde_json::Value,
    approvals_reviewer: serde_json::Value,
    cyber_access_program: serde_json::Value,
    // Known non-permission metadata does not need to be retained in memory.
    #[serde(default)]
    collaboration_mode: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    comp_hash: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    disabled_plugin_ids: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    effort: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    model: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    multi_agent_version: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    realtime_active: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    summary: Option<serde::de::IgnoredAny>,
}

fn standard_filesystem_entries(cwd: &Path) -> serde_json::Value {
    let mut entries = vec![
        serde_json::json!({"path":{"type":"special","value":{"kind":"root"}},"access":"read"}),
        serde_json::json!({"path":{"type":"path","path":cwd},"access":"write"}),
        serde_json::json!({"path":{"type":"special","value":{"kind":"slash_tmp"}},"access":"write"}),
        serde_json::json!({"path":{"type":"special","value":{"kind":"tmpdir"}},"access":"write"}),
    ];
    for name in [".git", ".agents", ".codex", ".aws"] {
        entries.push(serde_json::json!({"path":{"type":"path","path":cwd.join(name)},"access":"read","missing_path_behavior":"skip"}));
    }
    serde_json::Value::Array(entries)
}

fn context_policy(context: TurnContext, cwd: &Path) -> Result<SupervisedPolicy, String> {
    let mut sandbox = context.sandbox_policy;
    if sandbox.get("writable_roots") == Some(&serde_json::json!([])) {
        sandbox.as_object_mut().unwrap().remove("writable_roots");
    }
    let entries = standard_filesystem_entries(cwd);
    if context.cwd != cwd
        || context.root_turn_id != context.turn_id
        || context.approval_policy != "on-request"
        || sandbox
            != serde_json::json!({"type":"workspace-write","network_access":false,"exclude_tmpdir_env_var":false,"exclude_slash_tmp":false})
        || context.permission_profile
            != serde_json::json!({"type":"managed","file_system":{"type":"restricted","entries":entries},"network":"restricted"})
        || context.file_system_sandbox_policy
            != serde_json::json!({"kind":"restricted","entries":entries})
        || context.active_permission_profile != serde_json::json!({"id":":workspace"})
        || context.approvals_reviewer != "user"
        || context.cyber_access_program != "standard"
    {
        return Err("The completed turn does not use the supported standard workspace-write/on-request policy; nothing was switched".into());
    }
    validate_uuid(&context.turn_id)?;
    Ok(SupervisedPolicy {
        turn_id: context.turn_id,
        cwd: cwd.to_path_buf(),
    })
}

fn supervised_policy(path: &Path, thread: &str, cwd: &Path) -> Result<SupervisedPolicy, String> {
    let state = snapshot(path)?;
    validate_snapshot(&state, thread)?;
    let mut cursor = 0;
    let mut active = None;
    let mut completed = None;
    let mut policy = None;
    scan_record_bytes(path, &mut cursor, |line| {
        let record: LogRecord = serde_json::from_slice(line).map_err(|e| e.to_string())?;
        if record.kind == "turn_context" {
            #[derive(Deserialize)]
            struct ContextRecord {
                payload: TurnContext,
            }
            policy = Some(serde_json::from_slice::<ContextRecord>(line)
                .map_err(|_| "The latest turn context has an unknown or malformed permission state; nothing was switched".to_owned())
                .and_then(|record| context_policy(record.payload, cwd)));
        } else if let Some(event) = lifecycle(record) {
            match event {
                Lifecycle::Started(id) => active = Some(id),
                Lifecycle::Completed(id) if active.as_deref() == Some(&id) => {
                    active = None;
                    completed = Some(id);
                }
                _ => {}
            }
        }
        Ok(())
    })?;
    let policy =
        policy.ok_or("The completed turn has no permission context; nothing was switched")??;
    if active.is_some()
        || state.active_turn.is_some()
        || completed.as_deref() != Some(&policy.turn_id)
        || cursor != state.cursor
        || cursor != fs::metadata(path).map_err(|e| e.to_string())?.len()
    {
        return Err("A stable completed turn is required to preserve supervised permissions; nothing was switched".into());
    }
    Ok(policy)
}

/// Queue one exact terminal conversation for an account switch at an empty,
/// completed prompt. No auth/config files or account selections are changed.
pub fn queue_account_switch(
    executable: &Path,
    shell_id: &str,
    account: &str,
    expected_email: &str,
) -> Result<SessionReloadReport, String> {
    validate_uuid(shell_id)?;
    if expected_email.is_empty()
        || expected_email.len() > 200
        || !expected_email.contains('@')
        || expected_email
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("--expect-email must be the exact target account email".into());
    }
    let manager = SessionManager::open_default()?;
    let source = manager.get(shell_id)?;
    if !source.alive || source.harness != Some(HarnessKind::Codex) {
        return Err("Account switching requires a live Codex terminal".into());
    }
    let profile = metadata_profile(source.command.as_deref())?;
    if profile.is_some() {
        return Err("Account switching does not support custom Codex profiles; the original terminal was retained".into());
    }
    let source_home = source
        .codex_home
        .as_ref()
        .ok_or("This terminal's original Codex account is unknown")?
        .canonicalize()
        .map_err(|e| format!("Cannot resolve original Codex home: {e}"))?;
    let mut binding = crate::codex_accounts::resolve_account(manager.state_home(), account)?;
    binding.home = binding
        .home
        .canonicalize()
        .map_err(|e| format!("Cannot resolve target Codex home: {e}"))?;
    if source_home == binding.home {
        return Err("This terminal already uses the selected Codex home".into());
    }
    verify_target(&binding.home, expected_email)?;
    crate::usage::codex_supports_direct_resume(&binding.home)?;
    let pane_identity = manager.schedule_pane_identity(shell_id)?;
    let thread_id = manager
        .account_switch_provider_proof(&source, false)
        .map_err(|e| e.into_message())?;
    validate_uuid(&thread_id)?;
    let log_path = find_rollout(&source_home.join("sessions"), &thread_id)?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !log_path.starts_with(source_home.join("sessions")) {
        return Err("The conversation log is outside its original account home".into());
    }
    let state = snapshot(&log_path)?;
    validate_snapshot(&state, &thread_id)?;
    let supervised_policy = if source.unrestricted {
        None
    } else {
        let cwd = manager
            .current_directory(shell_id)?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        Some(supervised_policy(&log_path, &thread_id, &cwd)?)
    };
    let executable = executable.canonicalize().map_err(|e| e.to_string())?;
    let request = ReloadRequest {
        home: manager.state_home().to_path_buf(),
        shell_id: shell_id.into(),
        thread_id,
        executable,
        log_path,
        cursor: state.cursor,
        active_turn: state.active_turn,
        original_command: source.command.clone(),
        unrestricted: source.unrestricted,
        codex_home: Some(source_home.to_string_lossy().into_owned()),
        claude_config_dir: None,
        profile: None,
        account_switch: Some(AccountSwitch {
            source,
            pane_identity,
            target_home: binding.home,
            target_id: binding.id,
            expected_email: expected_email.into(),
            supervised_policy,
        }),
    };
    queue_request(request)
}

fn verify_target(home: &Path, email: &str) -> Result<String, String> {
    let usage = crate::usage::read_codex_usage_at(home)?;
    let label = usage
        .account_label
        .ok_or("Codex did not report the target account identity")?;
    // The engine produces email, optional workspace, then plan. Compare the
    // entire first field, never a substring or a saved catalog label.
    if !target_email_matches(&label, email) {
        return Err(
            "The target Codex home is signed in to a different account; nothing was switched"
                .into(),
        );
    }
    Ok(label)
}

fn target_email_matches(label: &str, email: &str) -> bool {
    label
        .split(" · ")
        .next()
        .is_some_and(|actual| actual.eq_ignore_ascii_case(email))
}

fn validate_snapshot(state: &LogSnapshot, thread: &str) -> Result<(), String> {
    if state.subagent
        || state.noninteractive
        || !state.saw_lifecycle
        || state.session_id.as_deref() != Some(thread)
    {
        return Err(
            "Cannot verify the exact primary Codex conversation; nothing was switched".into(),
        );
    }
    Ok(())
}

pub fn run_account_switch_worker(path: &Path) -> Result<(), String> {
    let request: ReloadRequest =
        serde_json::from_reader(File::open(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    validate_request(path, &request)?;
    if request.account_switch.is_none() {
        return Err("The account-switch worker requires an account-switch request".into());
    }
    let receipt = receipt_path(path);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(
            request
                .home
                .join(RELOAD_DIRECTORY)
                .join(format!("{}.lock", request.shell_id)),
        )
        .map_err(|e| e.to_string())?;
    if fs2::FileExt::try_lock_exclusive(&lock).is_err() {
        let message = "Another reload or account switch is already pending for this pane";
        write_receipt(&receipt, &request, "aborted", message)?;
        return Err(message.into());
    }
    write_receipt(
        &receipt,
        &request,
        "waiting",
        "Waiting for the exact conversation to finish at an empty prompt",
    )?;
    let result = finish_switch(&request);
    match &result {
        Ok(()) => write_receipt(
            &receipt,
            &request,
            "completed",
            "Resumed the exact conversation under the verified target account",
        )?,
        Err(error) => write_receipt(
            &receipt,
            &request,
            if error.starts_with("Account switch was dispatched") {
                "uncertain"
            } else {
                "failed"
            },
            error,
        )?,
    }
    result
}

fn unchanged(source: &ShellSession, current: &ShellSession) -> bool {
    current.alive
        && current.harness == Some(HarnessKind::Codex)
        && current.created_at_unix == source.created_at_unix
        && current.command == source.command
        && current.codex_home == source.codex_home
        && current.codex_account_id == source.codex_account_id
        && current.unrestricted == source.unrestricted
        && current.cwd == source.cwd
        && current.project_id == source.project_id
        && current.worktree_id == source.worktree_id
}

fn finish_switch(request: &ReloadRequest) -> Result<(), String> {
    let switch = request
        .account_switch
        .as_ref()
        .ok_or("Missing account switch")?;
    if request.unrestricted != switch.source.unrestricted
        || request.unrestricted == switch.supervised_policy.is_some()
        || request.profile.is_some()
    {
        return Err(
            "The account switch has no matching permission proof; nothing was switched".into(),
        );
    }
    let manager = SessionManager::at(request.home.clone())?;
    let mut tracker = ActivityTracker::at(request.home.clone());
    let mut cursor = request.cursor;
    let mut complete = request.active_turn.is_none();
    loop {
        let current = manager.get(&request.shell_id)?;
        if !unchanged(&switch.source, &current)
            || manager.schedule_pane_identity(&request.shell_id)? != switch.pane_identity
            || tracker.schedule_identity(&current).as_deref() != Some(&request.thread_id)
        {
            return Err("The queued terminal identity changed; nothing was switched".into());
        }
        for event in read_lifecycle(&request.log_path, &mut cursor)? {
            match event {
                Lifecycle::Started(_) => {
                    return Err("A newer Codex turn started; nothing was switched".into());
                }
                Lifecycle::Completed(id) if request.active_turn.as_deref() == Some(&id) => {
                    complete = true
                }
                Lifecycle::Completed(_) => {}
            }
        }
        if complete
            && tracker
                .schedule_idle_token(&current, &request.thread_id)
                .is_some()
            && manager.schedule_prompt_screen(&current).is_ok()
        {
            let label = verify_target(&switch.target_home, &switch.expected_email)?;
            crate::usage::codex_supports_direct_resume(&switch.target_home)?;
            let state = snapshot(&request.log_path)?;
            validate_snapshot(&state, &request.thread_id)?;
            if state.active_turn.is_some() || state.cursor != cursor {
                return Err(
                    "The Codex conversation changed during preparation; nothing was switched"
                        .into(),
                );
            }
            let source_home =
                Path::new(request.codex_home.as_deref().ok_or("Missing source home")?);
            let staged = StagedRollout::new(
                &request.log_path,
                source_home,
                &switch.target_home,
                &request.thread_id,
                state.cursor,
            )?;
            if let Some(expected) = &switch.supervised_policy {
                let actual =
                    supervised_policy(&staged.temporary, &request.thread_id, &expected.cwd)?;
                if &actual != expected {
                    return Err(
                        "The supervised turn or permission policy changed; nothing was switched"
                            .into(),
                    );
                }
            }
            let binding = CodexAccountBinding {
                home: switch.target_home.clone(),
                id: switch.target_id.clone(),
                label: Some(label),
                email: Some(switch.expected_email.clone()),
            };
            let folder = manager
                .current_directory(&request.shell_id)?
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if switch
                .supervised_policy
                .as_ref()
                .is_some_and(|policy| policy.cwd != folder)
            {
                return Err(
                    "The supervised working directory changed; nothing was switched".into(),
                );
            }
            let mut resume = request.clone();
            resume.codex_home = Some(binding.home.to_string_lossy().into_owned());
            let command = resume_command(&resume, Some(&folder));
            if manager.switch_codex_account_at_idle(
                &switch.source,
                &switch.pane_identity,
                &request.thread_id,
                &binding,
                &command,
                || staged.publish(),
            )? {
                crate::activity::bind_codex_thread(&request.home, &request.shell_id, &request.thread_id,
                    &binding.home).map_err(|e| format!("Account switch was dispatched, but activity binding could not be saved; do not retry: {e}"))?;
                return verify_resumed(&manager, request, &binding);
            }
        }
        // No deadline kills work. The worker stays queued through approvals and
        // drafts; a changed pane/thread or a newer turn cancels this exact request.
        thread::sleep(Duration::from_secs(2));
    }
}

fn verify_resumed(
    manager: &SessionManager,
    request: &ReloadRequest,
    binding: &CodexAccountBinding,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(shell) = manager.get(&request.shell_id) {
            if shell.alive
                && shell.codex_home.as_ref() == Some(&binding.home)
                && shell.codex_account_id == binding.id
                && manager
                    .account_switch_provider_proof(&shell, false)
                    .ok()
                    .as_deref()
                    == Some(&request.thread_id)
            {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err("Account switch was dispatched, but resumed conversation identity could not be verified; do not retry automatically".into());
        }
        thread::sleep(Duration::from_millis(200));
    }
}

struct StagedRollout {
    source: PathBuf,
    source_metadata: fs::Metadata,
    temporary: PathBuf,
    destination: PathBuf,
}

fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
}

impl StagedRollout {
    fn new(
        source: &Path,
        source_home: &Path,
        target_home: &Path,
        thread: &str,
        length: u64,
    ) -> Result<Self, String> {
        let relative = source
            .strip_prefix(source_home.join("sessions"))
            .map_err(|_| "Invalid source rollout path")?;
        let sessions = target_home.join("sessions");
        if sessions.exists() {
            match find_rollout(&sessions, thread) {
                Ok(existing) if existing != sessions.join(relative) => return Err("The target already contains this conversation at another path; nothing was overwritten".into()),
                Ok(_) => {},
                Err(e) if e.starts_with("Cannot find a persisted log") => {},
                Err(e) => return Err(e),
            }
        }
        let destination = sessions.join(relative);
        let mut directory = target_home.to_path_buf();
        for part in destination
            .parent()
            .ok_or("Missing rollout directory")?
            .strip_prefix(target_home)
            .map_err(|_| "Invalid target path")?
            .components()
        {
            if !matches!(part, std::path::Component::Normal(_)) {
                return Err("Invalid rollout directory".into());
            }
            directory.push(part);
            match fs::symlink_metadata(&directory) {
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err("The target rollout directory is not a regular directory".into());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    use std::os::unix::fs::DirBuilderExt;
                    fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&directory)
                        .map_err(|e| e.to_string())?;
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        let source_metadata = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
        if !source_metadata.is_file()
            || source_metadata.file_type().is_symlink()
            || source_metadata.len() != length
        {
            return Err("The Codex rollout is incomplete or changed; nothing was switched".into());
        }
        let temporary = directory.join(format!(".account-switch-{}.tmp", Uuid::new_v4()));
        let staged = Self {
            source: source.into(),
            source_metadata,
            temporary,
            destination,
        };
        let mut target = private_file(&staged.temporary)?;
        let mut input = File::open(source).map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 64 * 1024];
        let mut copied = 0;
        loop {
            let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            copied += count as u64;
            if copied > length {
                return Err(
                    "Codex wrote a newer turn during transfer; nothing was switched".into(),
                );
            }
            target
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
        }
        target.sync_all().map_err(|e| e.to_string())?;
        if copied != length
            || !same_file(
                &staged.source_metadata,
                &input.metadata().map_err(|e| e.to_string())?,
            )
        {
            return Err("The Codex rollout changed during transfer; nothing was switched".into());
        }
        let state = snapshot(&staged.temporary)?;
        validate_snapshot(&state, thread)?;
        if state.active_turn.is_some() || state.cursor != length {
            return Err(
                "The transferred conversation is not complete; nothing was switched".into(),
            );
        }
        Ok(staged)
    }

    fn publish(&self) -> Result<(), String> {
        if !same_file(
            &self.source_metadata,
            &fs::metadata(&self.source).map_err(|e| e.to_string())?,
        ) {
            return Err("The source conversation changed; nothing was switched".into());
        }
        // Link creation is atomic and never replaces an existing conversation.
        match fs::hard_link(&self.temporary, &self.destination) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(
                "The conversation already exists in the target home; nothing was overwritten"
                    .into(),
            ),
            Err(e) => Err(format!("Cannot publish the transferred conversation: {e}")),
        }
    }
}

impl Drop for StagedRollout {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temporary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        source: PathBuf,
        target: PathBuf,
        log: PathBuf,
        id: String,
    }
    impl Fixture {
        fn new() -> Self {
            let root = env::temp_dir().join(format!("somtus-account-switch-{}", Uuid::new_v4()));
            let source = root.join("source");
            let target = root.join("target");
            let id = Uuid::new_v4().to_string();
            let log = source.join(format!(
                "sessions/2026/11/07/rollout-2026-11-07T12-00-00-{id}.jsonl"
            ));
            fs::create_dir_all(log.parent().unwrap()).unwrap();
            fs::create_dir(&target).unwrap();
            fs::write(&log, format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"source\":\"cli\"}}}}\n{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"turn\"}}}}\n{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"turn\"}}}}\n")).unwrap();
            Self {
                root,
                source,
                target,
                log,
                id,
            }
        }
        fn stage(&self) -> Result<StagedRollout, String> {
            StagedRollout::new(
                &self.log,
                &self.source,
                &self.target,
                &self.id,
                fs::metadata(&self.log).unwrap().len(),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn standard_context(fixture: &Fixture, turn: &str) -> serde_json::Value {
        let entries = standard_filesystem_entries(&fixture.root);
        serde_json::json!({
            "turn_id":turn,"root_turn_id":turn,"cwd":fixture.root,
            "approval_policy":"on-request",
            "sandbox_policy":{"type":"workspace-write","network_access":false,"exclude_tmpdir_env_var":false,"exclude_slash_tmp":false},
            "permission_profile":{"type":"managed","file_system":{"type":"restricted","entries":entries},"network":"restricted"},
            "file_system_sandbox_policy":{"kind":"restricted","entries":entries},
            "active_permission_profile":{"id":":workspace"},
            "approvals_reviewer":"user","cyber_access_program":"standard",
            "model":"test-model","effort":"high"
        })
    }

    fn write_policy_log(fixture: &Fixture, context: &serde_json::Value) {
        let turn = context["turn_id"].as_str().unwrap();
        let records = [
            serde_json::json!({"type":"session_meta","payload":{"id":fixture.id,"source":"cli"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":turn}}),
            serde_json::json!({"type":"turn_context","payload":context}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":turn}}),
        ];
        fs::write(
            &fixture.log,
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )
        .unwrap();
    }

    #[test]
    fn supervised_policy_requires_the_exact_completed_thread_and_standard_permissions() {
        let fixture = Fixture::new();
        let turn = Uuid::new_v4().to_string();
        let mut context = standard_context(&fixture, &turn);
        write_policy_log(&fixture, &context);
        let expected = SupervisedPolicy {
            turn_id: turn.clone(),
            cwd: fixture.root.clone(),
        };
        assert_eq!(
            supervised_policy(&fixture.log, &fixture.id, &fixture.root).unwrap(),
            expected
        );
        context["sandbox_policy"]["writable_roots"] = serde_json::json!([]);
        write_policy_log(&fixture, &context);
        assert_eq!(
            supervised_policy(&fixture.log, &fixture.id, &fixture.root).unwrap(),
            expected
        );
        assert!(
            supervised_policy(&fixture.log, &Uuid::new_v4().to_string(), &fixture.root).is_err()
        );
        assert!(supervised_policy(&fixture.log, &fixture.id, &fixture.target).is_err());
    }

    #[test]
    fn supervised_policy_rejects_unknown_malformed_and_custom_permission_states() {
        let fixture = Fixture::new();
        let turn = Uuid::new_v4().to_string();
        for (pointer, value) in [
            ("/approval_policy", serde_json::json!("never")),
            ("/approval_policy", serde_json::json!(null)),
            (
                "/sandbox_policy/type",
                serde_json::json!("danger-full-access"),
            ),
            ("/sandbox_policy/network_access", serde_json::json!(true)),
            (
                "/sandbox_policy/exclude_tmpdir_env_var",
                serde_json::json!(true),
            ),
            ("/sandbox_policy/exclude_slash_tmp", serde_json::json!(true)),
            ("/permission_profile/network", serde_json::json!("enabled")),
            (
                "/permission_profile/file_system/entries/4/access",
                serde_json::json!("write"),
            ),
            (
                "/file_system_sandbox_policy/entries/1/access",
                serde_json::json!("read"),
            ),
            ("/active_permission_profile/id", serde_json::json!("custom")),
            (
                "/approvals_reviewer",
                serde_json::json!("guardian_subagent"),
            ),
            ("/cyber_access_program", serde_json::json!("unknown")),
            (
                "/root_turn_id",
                serde_json::json!(Uuid::new_v4().to_string()),
            ),
        ] {
            let mut context = standard_context(&fixture, &turn);
            *context.pointer_mut(pointer).unwrap() = value;
            write_policy_log(&fixture, &context);
            assert!(
                supervised_policy(&fixture.log, &fixture.id, &fixture.root).is_err(),
                "{pointer}"
            );
        }
        for (key, value) in [
            ("writable_roots", serde_json::json!(["/extra"])),
            ("new_permission", serde_json::json!(false)),
        ] {
            let mut context = standard_context(&fixture, &turn);
            context["sandbox_policy"][key] = value;
            write_policy_log(&fixture, &context);
            assert!(supervised_policy(&fixture.log, &fixture.id, &fixture.root).is_err());
        }
        let mut context = standard_context(&fixture, &turn);
        context["new_permission_state"] = serde_json::json!({"enabled":false});
        write_policy_log(&fixture, &context);
        assert!(
            supervised_policy(&fixture.log, &fixture.id, &fixture.root)
                .unwrap_err()
                .contains("unknown or malformed")
        );
        context
            .as_object_mut()
            .unwrap()
            .remove("new_permission_state");
        context
            .as_object_mut()
            .unwrap()
            .remove("permission_profile");
        write_policy_log(&fixture, &context);
        assert!(supervised_policy(&fixture.log, &fixture.id, &fixture.root).is_err());
    }

    #[test]
    fn supervised_policy_does_not_reuse_an_earlier_context_after_a_new_turn_or_partial_record() {
        let fixture = Fixture::new();
        let turn = Uuid::new_v4().to_string();
        write_policy_log(&fixture, &standard_context(&fixture, &turn));
        let expected = supervised_policy(&fixture.log, &fixture.id, &fixture.root).unwrap();
        let initial = fs::read(&fixture.log).unwrap();
        for suffix in [
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"new\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"new\"}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"new\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":",
        ] {
            fs::write(&fixture.log, &initial).unwrap();
            OpenOptions::new()
                .append(true)
                .open(&fixture.log)
                .unwrap()
                .write_all(suffix.as_bytes())
                .unwrap();
            assert!(supervised_policy(&fixture.log, &fixture.id, &fixture.root).is_err());
        }
        let next_turn = Uuid::new_v4().to_string();
        write_policy_log(&fixture, &standard_context(&fixture, &next_turn));
        assert_ne!(
            supervised_policy(&fixture.log, &fixture.id, &fixture.root).unwrap(),
            expected
        );
        let mut changed = standard_context(&fixture, &turn);
        changed["sandbox_policy"]["network_access"] = serde_json::json!(true);
        write_policy_log(&fixture, &changed);
        assert!(supervised_policy(&fixture.log, &fixture.id, &fixture.root).is_err());
    }

    #[test]
    fn supervised_resume_explicitly_overrides_target_defaults_and_uses_a_new_worker_protocol() {
        let fixture = Fixture::new();
        let turn = Uuid::new_v4().to_string();
        write_policy_log(&fixture, &standard_context(&fixture, &turn));
        let policy = supervised_policy(&fixture.log, &fixture.id, &fixture.root).unwrap();
        let source: ShellSession = serde_json::from_value(serde_json::json!({
            "id":Uuid::new_v4().to_string(),"kind":"project","cwd":fixture.root,
            "command":"codex","harness":"codex","created_at_unix":0,"unrestricted":false,
            "codex_home":fixture.source
        }))
        .unwrap();
        let request = ReloadRequest {
            home: fixture.root.clone(),
            shell_id: source.id.clone(),
            thread_id: fixture.id.clone(),
            executable: PathBuf::from("/stable/helper with spaces/somtus"),
            log_path: fixture.log.clone(),
            cursor: fs::metadata(&fixture.log).unwrap().len(),
            active_turn: None,
            original_command: source.command.clone(),
            unrestricted: false,
            codex_home: Some(fixture.target.to_string_lossy().into_owned()),
            claude_config_dir: None,
            profile: None,
            account_switch: Some(AccountSwitch {
                source,
                pane_identity: "%5|identity".into(),
                target_home: fixture.target.clone(),
                target_id: None,
                expected_email: "target@example.test".into(),
                supervised_policy: Some(policy),
            }),
        };
        let argv = shell_words(&resume_command(&request, Some(&fixture.root))).unwrap();
        for pair in [
            ["--sandbox", "workspace-write"],
            ["--ask-for-approval", "on-request"],
            ["-c", "sandbox_mode=\"workspace-write\""],
            ["-c", "approval_policy=\"on-request\""],
            ["-c", "approvals_reviewer=\"user\""],
            ["-c", "default_permissions=\":workspace\""],
            ["-c", "sandbox_workspace_write.writable_roots=[]"],
            ["-c", "sandbox_workspace_write.network_access=false"],
            ["-c", "sandbox_workspace_write.exclude_tmpdir_env_var=false"],
            ["-c", "sandbox_workspace_write.exclude_slash_tmp=false"],
            ["resume", &fixture.id],
        ] {
            assert!(argv.windows(2).any(|args| args == pair), "{pair:?}");
        }
        assert!(
            !argv
                .iter()
                .any(|arg| arg.contains("bypass") || arg == "--yolo")
        );
        assert!(argv.contains(&"--no-daemon".to_owned()));
        assert!(argv.contains(&format!("CODEX_HOME={}", fixture.target.display())));
        assert_eq!(worker_command(&request), "switch-supervised-account-worker");
        assert!(!request.account_switch.unwrap().source.unrestricted);
    }

    #[test]
    fn transferred_log_preserves_exact_bytes_and_never_copies_auth_or_database() {
        let fixture = Fixture::new();
        fs::write(fixture.source.join("auth.json"), "do-not-copy").unwrap();
        fs::write(fixture.source.join("state_5.sqlite"), "do-not-copy").unwrap();
        let staged = fixture.stage().unwrap();
        let destination = staged.destination.clone();
        let temporary = staged.temporary.clone();
        staged.publish().unwrap();
        drop(staged);
        assert_eq!(
            fs::read(destination).unwrap(),
            fs::read(&fixture.log).unwrap()
        );
        assert!(!temporary.exists());
        assert!(!fixture.target.join("auth.json").exists());
        assert!(!fixture.target.join("state_5.sqlite").exists());
    }

    #[test]
    fn changed_source_and_every_existing_destination_are_refused() {
        let fixture = Fixture::new();
        let staged = fixture.stage().unwrap();
        let mut log = OpenOptions::new().append(true).open(&fixture.log).unwrap();
        log.write_all(b"{}\n").unwrap();
        assert!(staged.publish().unwrap_err().contains("changed"));
        assert!(!staged.destination.exists());
        let fixture = Fixture::new();
        let staged = fixture.stage().unwrap();
        fs::copy(&fixture.log, &staged.destination).unwrap();
        assert!(staged.publish().unwrap_err().contains("already exists"));
        assert_eq!(
            fs::read(&staged.destination).unwrap(),
            fs::read(&fixture.log).unwrap()
        );
    }

    #[test]
    fn duplicate_thread_at_another_path_is_refused() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.target.join("sessions/other")).unwrap();
        fs::copy(
            &fixture.log,
            fixture
                .target
                .join(format!("sessions/other/rollout-other-{}.jsonl", fixture.id)),
        )
        .unwrap();
        assert!(fixture.stage().err().unwrap().contains("another path"));
    }

    #[test]
    fn partial_or_active_rollout_is_not_transferred() {
        for suffix in [
            "{",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"new\"}}\n",
        ] {
            let fixture = Fixture::new();
            OpenOptions::new()
                .append(true)
                .open(&fixture.log)
                .unwrap()
                .write_all(suffix.as_bytes())
                .unwrap();
            assert!(fixture.stage().is_err());
        }
    }

    #[test]
    fn target_directory_symlinks_are_refused() {
        let fixture = Fixture::new();
        std::os::unix::fs::symlink(
            fixture.source.join("sessions"),
            fixture.target.join("sessions"),
        )
        .unwrap();
        assert!(fixture.stage().is_err());
    }

    #[test]
    fn expected_email_is_an_exact_reported_identity_not_a_substring() {
        assert!(target_email_matches(
            "target@example.test · Workspace · pro",
            "target@example.test"
        ));
        for label in [
            "othertarget@example.test · pro",
            "other@example.test · target@example.test",
            "ChatGPT · pro",
            "target@example.test.evil · pro",
        ] {
            assert!(!target_email_matches(label, "target@example.test"));
        }
    }

    #[test]
    fn active_or_incomplete_delegated_work_blocks_switching() {
        let fixture = Fixture::new();
        let mut log = fs::read_to_string(&fixture.log)
            .unwrap()
            .replace("\"source\":\"cli\"", "\"source\":{\"subagent\":{}}");
        fs::write(&fixture.log, &log).unwrap();
        assert!(super::super::delegated_rollout_complete(&fixture.log).unwrap());
        log.push_str("{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"new\"}}\n");
        fs::write(&fixture.log, &log).unwrap();
        assert!(!super::super::delegated_rollout_complete(&fixture.log).unwrap());
    }
}
