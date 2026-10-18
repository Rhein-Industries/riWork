//! What the chat host hands a driver: the provider program, the Cua MCP
//! wiring, the Codex account and the environment, all resolved the way a
//! terminal launch of the same agent resolves them (`sessions::chat_launch`).
//! A Muse chat's Cua server is the driver's own, named for its session
//! (`muse::cua_servers`), so nothing here reaches Muse's command line.
//!
//! A Claude chat's `ANTHROPIC_API_KEY` follows its billing (`ChatInfo::claude_billing`,
//! else the Settings default): Subscription removes it; API key sets the key saved in the
//! Keychain (`anthropic_key`), or, with none saved, leaves the environment as it is.

use super::driver::DriverConfig;
use super::model::{ChatInfo, ClaudeBilling, ContributorMode, OrchestratorScope, Provider};
use crate::anthropic_key::KeyStore;
use crate::sessions::{self, HarnessKind};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The Codex account a new chat of `provider` runs under: the one `requested`
/// (an account id, which must be usable), or else the project's own, or else
/// the app's selection (or subscription balancing), as `Some(account id)`; `None` is the system default
/// (and every Claude chat). An unusable selection is an error, never a quiet
/// switch to another account.
pub fn account_for(
    home: &Path,
    provider: Provider,
    project_id: Option<&str>,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    match (provider, requested) {
        (Provider::Codex, Some(id)) => {
            Ok(crate::codex_accounts::resolve_launch_binding(home, Some(id))?.id)
        }
        (Provider::Codex, None) => Ok(sessions::new_codex_binding(home, project_id)?.id),
        (Provider::Claude, Some(_)) => {
            Err("a Claude chat has no Codex account; it uses the system login".into())
        }
        (Provider::Claude, None) => Ok(None),
        (Provider::Muse, Some(_)) => {
            Err("a Muse chat has no Codex account; it uses the system login".into())
        }
        (Provider::Muse, None) => Ok(None),
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
    driver_config_with(home, info, resume, &crate::anthropic_key::Keychain)
}

/// `driver_config`, with the saved API key from `keys`.
pub(crate) fn driver_config_with(
    home: &Path,
    info: &ChatInfo,
    resume: Option<String>,
    keys: &dyn KeyStore,
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
        Provider::Muse => (HarnessKind::Muse, None),
    };
    let launch = sessions::chat_launch(harness, &home, codex_home.as_deref())?;
    let api_key = api_key_for(&home, info, keys)?;
    let mut config = assemble(
        info,
        resume,
        launch.program,
        launch.arguments,
        launch.path,
        &home,
        codex_home,
        api_key,
    );
    trust_folder(&mut config);
    Ok(config)
}

/// What a chat's start does with `ANTHROPIC_API_KEY`. Its `Debug` never shows a key.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum ApiKeyEnv {
    /// The environment as inherited: not a Claude chat, or a Claude chat on API key with
    /// no key saved, which uses a key the shell exports, or else Claude's own sign-in.
    Inherit,
    /// Removed, so the chat runs on the user's Claude sign-in (Subscription).
    Remove,
    /// Set to the key saved in the Keychain (API key).
    Set(String),
}

impl std::fmt::Debug for ApiKeyEnv {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Inherit => "Inherit",
            Self::Remove => "Remove",
            Self::Set(_) => "Set(<key>)",
        })
    }
}

impl ApiKeyEnv {
    /// Makes `config` start its process with `ANTHROPIC_API_KEY` as this says.
    pub(crate) fn apply(self, config: &mut DriverConfig) {
        const NAME: &str = "ANTHROPIC_API_KEY";
        match self {
            // `claude` keeps every sign-in method it has, as published.
            ApiKeyEnv::Inherit => {}
            // Subscription: a stray key must not switch the chat to API billing.
            ApiKeyEnv::Remove => {
                config.env.retain(|(name, _)| name != NAME);
                config.env_remove.push(NAME.into());
            }
            ApiKeyEnv::Set(key) => {
                let marker = crate::anthropic_key::INJECTED_MARKER;
                config
                    .env_remove
                    .retain(|name| name != NAME && name != marker);
                config
                    .env
                    .retain(|(name, _)| name != NAME && name != marker);
                config.env.push((NAME.into(), key.into()));
                config.env.push((marker.into(), "1".into()));
            }
        }
    }
}

/// `ANTHROPIC_API_KEY` for a Claude chat billed by `billing`, `saved` being the key in the
/// Keychain.
pub(crate) fn api_key_env(billing: ClaudeBilling, saved: Option<String>) -> ApiKeyEnv {
    match (billing, saved) {
        (ClaudeBilling::Subscription, _) => ApiKeyEnv::Remove,
        (ClaudeBilling::ApiKey, Some(key)) => ApiKeyEnv::Set(key),
        (ClaudeBilling::ApiKey, None) => ApiKeyEnv::Inherit,
    }
}

/// `ANTHROPIC_API_KEY` for the start of `info`: its billing, else the Settings default of
/// `home`. The Keychain is asked only for a Claude chat on API key, and first only whether
/// a key is saved, which needs no access to the key: with none saved, or no answer, the
/// start is as it always was (`Inherit`), so a user who never saved a key meets no new
/// failure. A key that is saved but cannot be read fails the start: the chat asked for the
/// key, and starting on another sign-in could bill another account.
pub(crate) fn api_key_for(
    home: &Path,
    info: &ChatInfo,
    keys: &dyn KeyStore,
) -> Result<ApiKeyEnv, String> {
    if info.provider != Provider::Claude {
        return Ok(ApiKeyEnv::Inherit);
    }
    let billing = info.claude_billing_or(crate::settings::claude_chat_billing(home));
    let saved = match billing {
        ClaudeBilling::Subscription => None,
        ClaudeBilling::ApiKey => match keys.is_saved() {
            Ok(true) => keys.read().map_err(|error| {
                format!(
                    "{}. Unlock the Keychain and send again, or switch this chat to Subscription.",
                    error.trim_end_matches('.')
                )
            })?,
            Ok(false) => None,
            Err(error) => {
                eprintln!(
                    "riwork chat: {}: cannot tell whether an Anthropic API key is saved ({error}); \
                     the chat starts with the environment as it is",
                    info.id
                );
                None
            }
        },
    };
    Ok(api_key_env(billing, saved))
}

/// Trust the chat's folder for its provider process, as a terminal's is
/// (`folder_trust`): Codex with a per-run `-c` among the leading arguments,
/// Muse with `serve --trust-workspace`. Claude asks nothing without a
/// terminal, so its configuration is left alone.
fn trust_folder(config: &mut DriverConfig) {
    let Ok(folder) = config.cwd.canonicalize() else {
        return;
    };
    match config.provider {
        Provider::Codex => config
            .extra_args
            .extend(crate::folder_trust::codex_arguments(&folder)),
        Provider::Muse => config.trust_workspace = crate::folder_trust::muse_may_trust(&folder),
        Provider::Claude => {}
    }
}

/// A read-only Muse model query has no chat identity, permissions to select or
/// session context. Keep the normal provider program and PATH, without Cua.
pub(super) fn muse_discovery_config(home: &Path) -> Result<DriverConfig, String> {
    let home = home.canonicalize().map_err(|error| error.to_string())?;
    let launch = sessions::chat_launch(HarnessKind::Muse, &home, None)?;
    Ok(discovery_config(
        home,
        launch.program,
        launch.arguments,
        launch.path,
    ))
}

fn discovery_config(
    home: PathBuf,
    program: PathBuf,
    extra_args: Vec<String>,
    path: OsString,
) -> DriverConfig {
    DriverConfig {
        provider: Provider::Muse,
        program,
        cwd: home.clone(),
        approval_mode: super::model::ApprovalMode::Supervised,
        model: None,
        effort: None,
        fast: false,
        // No chat chose it: off, as every Muse RiWork starts unless asked on.
        contributor_mode: ContributorMode::Off,
        trust_workspace: false,
        resume: None,
        resume_has_history: false,
        reporter: Default::default(),
        outstanding_notices: Default::default(),
        extra_args,
        env: vec![
            ("PATH".into(), path),
            ("RIWORK_HOME".into(), home.into_os_string()),
            ("MUSE_NO_AUTO_UPDATE".into(), "1".into()),
            ("MUSE_LOGIN".into(), "0".into()),
        ],
        env_remove: [
            "RIWORK_CHAT_ID",
            "RIWORK_SHELL_ID",
            "RIWORK_CODEX_SHELL_ID",
            "RIWORK_ORCHESTRATOR_SCOPE",
            "RIWORK_PROJECT_ID",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        instructions: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn assemble(
    info: &ChatInfo,
    resume: Option<String>,
    program: PathBuf,
    extra_args: Vec<String>,
    path: OsString,
    home: &Path,
    codex_home: Option<PathBuf>,
    api_key: ApiKeyEnv,
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
    // The chat's own agent finds itself by this id: `riwork handoff` with no
    // `--from` hands off the chat that runs it. Codex builds the environment of
    // the commands it runs from a policy of its own, which a user's config can
    // narrow, so the id is named there too, as a terminal's pane id is.
    env.push(("RIWORK_CHAT_ID".into(), info.id.clone().into()));
    let mut extra_args = extra_args;
    if info.provider == Provider::Codex {
        extra_args.extend([
            "-c".to_owned(),
            format!(
                "shell_environment_policy.set.RIWORK_CHAT_ID={}",
                serde_json::Value::from(info.id.clone())
            ),
        ]);
    }
    if let Some(codex_home) = codex_home {
        env.push(("CODEX_HOME".into(), codex_home.clone().into_os_string()));
        env.push((
            "RIWORK_CODEX_ACCOUNT_HOME".into(),
            codex_home.into_os_string(),
        ));
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
    let mut config = DriverConfig {
        provider: info.provider,
        program,
        cwd: info.cwd.clone(),
        approval_mode: info.approval_mode,
        model: info.model.clone(),
        effort: info.effort.clone(),
        fast: info.fast,
        contributor_mode: info.contributor_mode,
        trust_workspace: false,
        resume,
        resume_has_history: false,
        reporter: Default::default(),
        outstanding_notices: Default::default(),
        extra_args,
        instructions: instructions(info),
        env,
        env_remove,
    };
    api_key.apply(&mut config);
    config
}

/// What the agent is told of the conversation the chat had before it switched provider.
/// A document that is gone is said once in the host's log and leaves the agent without it.
pub(crate) fn instructions(info: &ChatInfo) -> Option<String> {
    let carried = info.carried_over.as_ref()?;
    let instructions = crate::handoff::carried_over_instructions(&carried.from, &carried.document);
    if instructions.is_none() {
        eprintln!(
            "riwork chat: {}: cannot read {}; the agent starts without the earlier conversation",
            info.id,
            carried.document.display()
        );
    }
    instructions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ApprovalMode, ChatState};

    fn info(provider: Provider) -> ChatInfo {
        ChatInfo {
            parent_id: None,
            user_title: None,
            first_user_message: None,
            provider_title: None,
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
            fast: true,
            contributor_mode: ContributorMode::Off,
            contributor_opt_out: false,
            claude_billing: None,
            approval_mode: ApprovalMode::AutoEdit,
            codex_account_id: None,
            state: ChatState::Stopped,
            orchestrator: None,
            carried_over: None,
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
    fn a_chat_runs_under_the_account_asked_for_and_a_claude_chat_has_none_to_ask_for() {
        use crate::codex_accounts::SYSTEM_DEFAULT_ID;
        let home = std::env::temp_dir().join(format!("riwork-launch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        // The system default is no account of RiWork's.
        assert_eq!(
            account_for(&home, Provider::Codex, None, Some(SYSTEM_DEFAULT_ID)),
            Ok(None)
        );
        // An account nobody saved is refused, not swapped for the system's.
        assert!(account_for(&home, Provider::Codex, None, Some("acct-1")).is_err());
        assert!(account_for(&home, Provider::Codex, None, Some("../escape")).is_err());
        // Without a request it is the project's or the app's, here the system's.
        assert_eq!(account_for(&home, Provider::Codex, None, None), Ok(None));
        assert_eq!(account_for(&home, Provider::Claude, None, None), Ok(None));
        let error = account_for(&home, Provider::Claude, None, Some("Work")).unwrap_err();
        assert!(error.contains("no Codex account"), "{error}");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_codex_chat_runs_under_its_account_home_and_a_claude_chat_keeps_its_api_key() {
        let codex = assemble(
            &info(Provider::Codex),
            Some("thread".into()),
            "/bin/codex".into(),
            vec!["-c".into(), "x=1".into()],
            "/shims:/bin".into(),
            Path::new("/riwork"),
            Some("/accounts/a".into()),
            ApiKeyEnv::Inherit,
        );
        assert_eq!(codex.resume.as_deref(), Some("thread"));
        assert_eq!(
            codex.extra_args,
            [
                "-c",
                "x=1",
                "-c",
                "shell_environment_policy.set.RIWORK_CHAT_ID=\"id\""
            ]
        );
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
        assert_eq!(value(&codex, "RIWORK_CHAT_ID").unwrap(), "id");
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
            ApiKeyEnv::Inherit,
        );
        // Claude runs as published unless its billing removes or sets the key.
        assert!(!claude.env_remove.contains(&"ANTHROPIC_API_KEY".into()));
        assert!(value(&claude, "ANTHROPIC_API_KEY").is_none());
        let ignoring = assemble(
            &info(Provider::Claude),
            None,
            "/bin/claude".into(),
            Vec::new(),
            "/shims".into(),
            Path::new("/riwork"),
            None,
            ApiKeyEnv::Remove,
        );
        assert!(ignoring.env_remove.contains(&"ANTHROPIC_API_KEY".into()));
        assert_eq!(value(&claude, "RIWORK_CHAT_ID").unwrap(), "id");
        assert!(
            claude.extra_args.is_empty(),
            "Claude takes its environment as it is"
        );
        assert!(value(&claude, "CODEX_HOME").is_none());
        assert_eq!(claude.resume, None);
        for config in [&codex, &claude] {
            assert!(config.env_remove.contains(&"RIWORK_SHELL_ID".into()));
            // An ordinary chat is not an orchestrator.
            assert!(value(config, "RIWORK_ORCHESTRATOR_SCOPE").is_none());
        }
    }

    #[test]
    fn a_chat_is_trusted_in_its_folder_as_a_terminal_is_and_claude_is_left_alone() {
        let root = std::env::temp_dir().join(format!("riwork-chat-trust-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        std::fs::create_dir_all(root.join("repo/inner")).unwrap();
        let repository = root.join("repo").canonicalize().unwrap();
        let configure = |provider: Provider, cwd: &Path| {
            let mut chat = info(provider);
            chat.cwd = cwd.to_path_buf();
            let mut config = assemble(
                &chat,
                None,
                "/bin/agent".into(),
                Vec::new(),
                "/shims".into(),
                Path::new("/riwork"),
                None,
                ApiKeyEnv::Inherit,
            );
            trust_folder(&mut config);
            config
        };
        let codex = configure(Provider::Codex, &repository);
        let trust = crate::folder_trust::codex_arguments(&repository);
        assert_eq!(trust.len(), 2);
        assert!(codex.extra_args.ends_with(&trust), "{:?}", codex.extra_args);
        assert!(!codex.trust_workspace);
        let muse = configure(Provider::Muse, &repository);
        assert!(muse.trust_workspace && muse.extra_args.is_empty());
        // Muse would take the whole repository for its workspace there.
        assert!(!configure(Provider::Muse, &repository.join("inner")).trust_workspace);
        let claude = configure(Provider::Claude, &repository);
        assert!(claude.extra_args.is_empty() && !claude.trust_workspace);
        // A folder that is gone is trusted with nothing; the start reports it.
        let gone = configure(Provider::Codex, &root.join("gone"));
        assert_eq!(gone.extra_args.len(), 2, "{:?}", gone.extra_args);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_muse_chat_hands_its_contributor_mode_to_every_start_and_an_older_opt_out_is_off() {
        let configure = |mode: ContributorMode, opt_out: bool, resume: Option<&str>| {
            let mut chat = info(Provider::Muse);
            chat.contributor_mode = mode;
            chat.contributor_opt_out = opt_out;
            assemble(
                &chat,
                resume.map(Into::into),
                "/bin/muse".into(),
                Vec::new(),
                "/shims".into(),
                Path::new("/riwork"),
                None,
                ApiKeyEnv::Inherit,
            )
        };
        for resume in [None, Some("sess")] {
            for opt_out in [false, true] {
                assert_eq!(
                    configure(ContributorMode::On, opt_out, resume).contributor_mode,
                    ContributorMode::On
                );
                // Whatever an older host wrote beside it, a chat not turned on starts off.
                assert_eq!(
                    configure(ContributorMode::Off, opt_out, resume).contributor_mode,
                    ContributorMode::Off
                );
            }
        }
        // Muse takes it on its own command line, after `serve`; nothing leads it.
        assert!(
            configure(ContributorMode::On, false, None)
                .extra_args
                .is_empty()
        );
    }

    #[test]
    fn model_discovery_starts_muse_with_contributor_mode_off() {
        let config = discovery_config(
            "/riwork".into(),
            "/bin/muse".into(),
            Vec::new(),
            "/shims".into(),
        );
        assert_eq!(config.contributor_mode, ContributorMode::Off);
        assert!(config.extra_args.is_empty());
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
                ApiKeyEnv::Inherit,
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

    /// What the start of `chat` does with `ANTHROPIC_API_KEY`, by the Settings default of
    /// `home` and the key in `keys`.
    fn key_of(home: &Path, chat: &ChatInfo, keys: &dyn KeyStore) -> Result<ApiKeyEnv, String> {
        api_key_for(home, chat, keys)
    }

    #[test]
    fn a_claude_chat_bills_its_choice_or_the_settings_default_and_only_api_key_reads_the_key() {
        use crate::anthropic_key::MemoryStore;
        use ClaudeBilling::{ApiKey, Subscription};
        const SAVED: &str = "sk-ant-dummy-saved";
        let home = std::env::temp_dir().join(format!("riwork-billing-{}", uuid::Uuid::new_v4()));
        let store = crate::settings::SettingsStore::open(&home).unwrap();
        let set_default = |billing| {
            store
                .update(|settings| settings.claude_chat_billing = billing)
                .unwrap();
        };
        for default in [None, Some(ApiKey), Some(Subscription)] {
            match default {
                // A home with no settings file: the default, API key.
                None => {
                    let _ = std::fs::remove_file(home.join("settings.json"));
                }
                Some(billing) => set_default(billing),
            }
            for own in [None, Some(ApiKey), Some(Subscription)] {
                let mut chat = info(Provider::Claude);
                chat.claude_billing = own;
                let billing = own.or(default).unwrap_or(ApiKey);
                for saved in [None, Some(SAVED)] {
                    let keys = saved.map_or_else(MemoryStore::default, MemoryStore::with_key);
                    let expected = match (billing, saved) {
                        (Subscription, _) => ApiKeyEnv::Remove,
                        (ApiKey, Some(key)) => ApiKeyEnv::Set(key.into()),
                        (ApiKey, None) => ApiKeyEnv::Inherit,
                    };
                    let case = format!(
                        "default {default:?}, own {own:?}, saved {}",
                        saved.is_some()
                    );
                    assert_eq!(key_of(&home, &chat, &keys), Ok(expected), "{case}");
                    // A saved key the Keychain will not give fails the start on API key; with
                    // none saved there is nothing to give. Subscription never asks.
                    keys.fail_reads(Some(
                        "The Keychain could not read the Anthropic API key: locked.",
                    ));
                    let failed = key_of(&home, &chat, &keys);
                    match (billing, saved) {
                        (Subscription, _) => assert_eq!(failed, Ok(ApiKeyEnv::Remove), "{case}"),
                        (ApiKey, None) => assert_eq!(failed, Ok(ApiKeyEnv::Inherit), "{case}"),
                        (ApiKey, Some(_)) => {
                            let error = failed.unwrap_err();
                            assert!(error.contains("locked"), "{case}: {error}");
                            assert!(
                                error.contains("switch this chat to Subscription"),
                                "{error}"
                            );
                            assert!(!error.contains(".."), "{error}");
                        }
                    }
                    // A Keychain that cannot even say whether a key is saved: the start is as
                    // it always was, so a user who never saved a key meets no new failure.
                    keys.fail_reads(None);
                    keys.fail(Some(
                        "The Keychain could not look up the Anthropic API key.",
                    ));
                    let unknown = key_of(&home, &chat, &keys);
                    match billing {
                        Subscription => assert_eq!(unknown, Ok(ApiKeyEnv::Remove), "{case}"),
                        ApiKey => assert_eq!(unknown, Ok(ApiKeyEnv::Inherit), "{case}"),
                    }
                }
            }
        }
        // Another provider's chat is left as it is, whatever it was given, and never asks.
        let failing = MemoryStore::with_key(SAVED);
        failing.fail(Some("must not be asked"));
        for provider in [Provider::Codex, Provider::Muse] {
            for own in [None, Some(ApiKey), Some(Subscription)] {
                let mut chat = info(provider);
                chat.claude_billing = own;
                assert_eq!(key_of(&home, &chat, &failing), Ok(ApiKeyEnv::Inherit));
            }
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn the_key_is_set_or_removed_in_the_environment_alone_and_debug_never_shows_it() {
        const SAVED: &str = "sk-ant-dummy-saved";
        let start = |api_key: ApiKeyEnv| {
            assemble(
                &info(Provider::Claude),
                None,
                "/bin/claude".into(),
                Vec::new(),
                "/shims".into(),
                Path::new("/riwork"),
                None,
                api_key,
            )
        };
        let set = start(ApiKeyEnv::Set(SAVED.into()));
        assert_eq!(value(&set, "ANTHROPIC_API_KEY").unwrap(), SAVED);
        // Marked as RiWork's, so a RiWork process the agent starts drops it again.
        assert_eq!(
            value(&set, crate::anthropic_key::INJECTED_MARKER).unwrap(),
            "1"
        );
        assert!(!set.env_remove.contains(&"ANTHROPIC_API_KEY".into()));
        assert!(
            !set.extra_args
                .iter()
                .any(|argument| argument.contains(SAVED))
        );
        for shown in [
            format!("{set:?}"),
            format!("{:?}", ApiKeyEnv::Set(SAVED.into())),
        ] {
            assert!(!shown.contains(SAVED), "{shown}");
        }
        assert!(format!("{set:?}").contains("ANTHROPIC_API_KEY"));
        let removed = start(ApiKeyEnv::Remove);
        assert!(value(&removed, "ANTHROPIC_API_KEY").is_none());
        assert!(removed.env_remove.contains(&"ANTHROPIC_API_KEY".into()));
        let inherited = start(ApiKeyEnv::Inherit);
        assert!(value(&inherited, "ANTHROPIC_API_KEY").is_none());
        assert!(!inherited.env_remove.contains(&"ANTHROPIC_API_KEY".into()));
        // Applied over an environment that has a key, Set replaces it and Remove drops it.
        let mut over = removed.clone();
        over.env_remove.clear();
        over.env
            .push(("ANTHROPIC_API_KEY".into(), "sk-ant-dummy-env".into()));
        let mut replaced = over.clone();
        ApiKeyEnv::Set(SAVED.into()).apply(&mut replaced);
        let keys: Vec<_> = replaced
            .env
            .iter()
            .filter(|(name, _)| name == "ANTHROPIC_API_KEY")
            .collect();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].1, SAVED);
        ApiKeyEnv::Remove.apply(&mut over);
        assert!(value(&over, "ANTHROPIC_API_KEY").is_none());
        assert!(over.env_remove.contains(&"ANTHROPIC_API_KEY".into()));
    }
}
