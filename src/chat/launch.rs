//! What the chat host hands a driver: the provider program, the Cua MCP
//! wiring, the Codex account and the environment, all resolved the way a
//! terminal launch of the same agent resolves them (`sessions::chat_launch`).

use super::driver::DriverConfig;
use super::model::{ChatInfo, OrchestratorScope, Provider};
use crate::sessions::{self, HarnessKind};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The Codex account a new chat of `provider` runs under: the project's own, or
/// else the app's selection, as `Some(account id)`; `None` is the system
/// default (and every Claude chat). An unusable selection is an error, never a
/// quiet switch to another account.
pub fn account_for(
    home: &Path,
    provider: Provider,
    project_id: Option<&str>,
) -> Result<Option<String>, String> {
    match provider {
        Provider::Codex => Ok(sessions::selected_codex_binding(home, project_id)?.id),
        Provider::Claude => Ok(None),
    }
}

/// How to start the provider process of `info`, resuming `resume` if given.
/// The account is the one saved in `info`, so a chat keeps its Codex home (and
/// with it the threads stored there) however the selection changes later.
pub fn driver_config(
    home: &Path,
    info: &ChatInfo,
    resume: Option<String>,
) -> Result<DriverConfig, String> {
    let home = home
        .canonicalize()
        .map_err(|error| format!("resolve RiWork state: {error}"))?;
    let (harness, codex_home) = match info.provider {
        Provider::Codex => {
            let binding = crate::codex_accounts::resolve_launch_binding(
                &home,
                info.codex_account_id.as_deref(),
            )?;
            (HarnessKind::Codex, Some(binding.home))
        }
        Provider::Claude => (HarnessKind::Claude, None),
    };
    let launch = sessions::chat_launch(harness, &home, codex_home.as_deref())?;
    Ok(assemble(
        info,
        resume,
        launch.program,
        launch.arguments,
        launch.path,
        &home,
        codex_home,
    ))
}

fn assemble(
    info: &ChatInfo,
    resume: Option<String>,
    program: PathBuf,
    extra_args: Vec<String>,
    path: OsString,
    home: &Path,
    codex_home: Option<PathBuf>,
) -> DriverConfig {
    let mut env = vec![
        (OsString::from("PATH"), path),
        (OsString::from("RIWORK_HOME"), home.as_os_str().to_owned()),
    ];
    // The pane variables name a terminal that is not this chat's.
    let mut env_remove = vec![
        OsString::from("RIWORK_SHELL_ID"),
        OsString::from("RIWORK_CODEX_SHELL_ID"),
    ];
    if let Some(codex_home) = codex_home {
        env.push(("CODEX_HOME".into(), codex_home.clone().into_os_string()));
        env.push((
            "RIWORK_CODEX_ACCOUNT_HOME".into(),
            codex_home.into_os_string(),
        ));
    }
    if info.provider == Provider::Claude {
        // A stray key would silently switch the chat to API billing.
        env_remove.push("ANTHROPIC_API_KEY".into());
    }
    // What a terminal orchestrator's session exports, for the skill that reads it.
    match &info.orchestrator {
        Some(OrchestratorScope::Global) => {
            env.push(("RIWORK_ORCHESTRATOR_SCOPE".into(), "global".into()));
            // The host may have started inside a project orchestrator.
            env_remove.push("RIWORK_PROJECT_ID".into());
        }
        Some(OrchestratorScope::Project { project_id }) => {
            env.push(("RIWORK_ORCHESTRATOR_SCOPE".into(), "project".into()));
            env.push(("RIWORK_PROJECT_ID".into(), project_id.into()));
        }
        None => {}
    }
    DriverConfig {
        provider: info.provider,
        program,
        cwd: info.cwd.clone(),
        approval_mode: info.approval_mode,
        model: info.model.clone(),
        effort: info.effort.clone(),
        resume,
        extra_args,
        env,
        env_remove,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ApprovalMode, ChatState};

    fn info(provider: Provider) -> ChatInfo {
        ChatInfo {
            id: "id".into(),
            provider,
            project_id: None,
            worktree_id: None,
            cwd: "/work".into(),
            title: "t".into(),
            created_at_unix: 1,
            provider_thread_id: Some("thread".into()),
            model: Some("m".into()),
            effort: Some("high".into()),
            approval_mode: ApprovalMode::AutoEdit,
            codex_account_id: None,
            state: ChatState::Stopped,
            orchestrator: None,
        }
    }

    fn value<'a>(config: &'a DriverConfig, name: &str) -> Option<&'a OsString> {
        config
            .env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    #[test]
    fn a_codex_chat_runs_under_its_account_home_and_a_claude_chat_never_sees_an_api_key() {
        let codex = assemble(
            &info(Provider::Codex),
            Some("thread".into()),
            "/bin/codex".into(),
            vec!["-c".into(), "x=1".into()],
            "/shims:/bin".into(),
            Path::new("/riwork"),
            Some("/accounts/a".into()),
        );
        assert_eq!(codex.resume.as_deref(), Some("thread"));
        assert_eq!(codex.extra_args, ["-c", "x=1"]);
        assert_eq!(
            (codex.cwd.as_path(), codex.approval_mode),
            (Path::new("/work"), ApprovalMode::AutoEdit)
        );
        assert_eq!(
            (codex.model.as_deref(), codex.effort.as_deref()),
            (Some("m"), Some("high"))
        );
        for name in ["CODEX_HOME", "RIWORK_CODEX_ACCOUNT_HOME"] {
            assert_eq!(value(&codex, name).unwrap(), "/accounts/a", "{name}");
        }
        assert_eq!(value(&codex, "RIWORK_HOME").unwrap(), "/riwork");
        assert_eq!(value(&codex, "PATH").unwrap(), "/shims:/bin");
        assert!(!codex.env_remove.contains(&"ANTHROPIC_API_KEY".into()));

        let claude = assemble(
            &info(Provider::Claude),
            None,
            "/bin/claude".into(),
            Vec::new(),
            "/shims".into(),
            Path::new("/riwork"),
            None,
        );
        assert!(claude.env_remove.contains(&"ANTHROPIC_API_KEY".into()));
        assert!(value(&claude, "CODEX_HOME").is_none());
        assert_eq!(claude.resume, None);
        for config in [&codex, &claude] {
            assert!(config.env_remove.contains(&"RIWORK_SHELL_ID".into()));
            // An ordinary chat is not an orchestrator.
            assert!(value(config, "RIWORK_ORCHESTRATOR_SCOPE").is_none());
        }
    }

    #[test]
    fn an_orchestrator_chat_knows_its_scope_the_way_a_terminal_orchestrator_does() {
        let configure = |scope: OrchestratorScope| {
            let mut chat = info(Provider::Codex);
            chat.orchestrator = Some(scope);
            assemble(
                &chat,
                None,
                "/bin/codex".into(),
                Vec::new(),
                "/shims".into(),
                Path::new("/riwork"),
                None,
            )
        };
        let project = configure(OrchestratorScope::Project {
            project_id: "11111111-1111-4111-8111-111111111111".into(),
        });
        assert_eq!(
            value(&project, "RIWORK_ORCHESTRATOR_SCOPE").unwrap(),
            "project"
        );
        assert_eq!(
            value(&project, "RIWORK_PROJECT_ID").unwrap(),
            "11111111-1111-4111-8111-111111111111"
        );
        let global = configure(OrchestratorScope::Global);
        assert_eq!(
            value(&global, "RIWORK_ORCHESTRATOR_SCOPE").unwrap(),
            "global"
        );
        assert!(value(&global, "RIWORK_PROJECT_ID").is_none());
        assert!(global.env_remove.contains(&"RIWORK_PROJECT_ID".into()));
    }
}
