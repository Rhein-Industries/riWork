//! Preference and project form checks use private fixtures and saved metadata
//! only. No account discovery, credentials, agent process, network, or window.
use riwork_core::{
    provider::{ClaudeBilling, Provider},
    settings::{OrchestratorRuns, Settings},
    store::Store,
};
use riwork_nsh_native::{
    ProcessService,
    backend::{CoreStoreBackend, SessionsBackend, StoreBackend},
    dto::{ProjectCodexAccountDto, SessionsSnapshot, ShellSessionDto},
    project_settings::project_form,
    settings::{
        CoreSettingsBackend, PreferenceService, SettingsBackend, SettingsDto, SettingsPatch,
        StatusChange,
    },
    usage::{AccountsDto, CodexAccountDto},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn patch(value: Value) -> SettingsPatch {
    serde_json::from_value(value).unwrap()
}
fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn scalar_form_edits_write_only_selected_keys_and_preserve_unknown_preferences() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path();
    let path = home.join("settings.json");
    // Core materializes known defaults absent from an older document. Start
    // with those explicit values so this assertion isolates the form edit and
    // verifies that every unrelated known/future value stays byte-for-value.
    let mut original = Value::Object(Settings::default().values().unwrap());
    original["theme"] = json!("future_theme");
    original["unknown"] = json!({"keep":true});
    original["orchestrator_chat_provider"] = json!("future_provider");
    original["orchestrator_chat_model"] = json!("future_model");
    let cases = [
        (
            "rememberWindowSize",
            json!(false),
            "remember_window_size",
            json!(false),
        ),
        ("panelTabIcons", json!(true), "panel_tab_icons", json!(true)),
        (
            "tabCloseBehavior",
            json!("detach"),
            "tab_close_behavior",
            json!("detach"),
        ),
        (
            "agentInlineMode",
            json!(false),
            "agent_inline_mode",
            json!(false),
        ),
        (
            "agentTerminalsUnrestricted",
            json!(false),
            "agent_terminals_unrestricted",
            json!(false),
        ),
        ("dictationMic", json!(true), "dictation_mic", json!(true)),
        (
            "claudeChatIgnoresApiKey",
            json!(true),
            "claude_chat_ignores_api_key",
            json!(true),
        ),
        (
            "codexSubscriptionBalancing",
            json!(true),
            "codex_subscription_balancing",
            json!(true),
        ),
        (
            "dashboardHaikuSummaries",
            json!(true),
            "dashboard_haiku_summaries",
            json!(true),
        ),
        (
            "orchestratorMode",
            json!("terminal"),
            "orchestrator_mode",
            json!("terminal"),
        ),
    ];
    let backend = CoreSettingsBackend::new(home.into()).unwrap();
    for (bridge, value, stored, expected) in cases {
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        backend.set(patch(json!({bridge:value}))).unwrap();
        let mut expected_document = original.clone();
        expected_document[stored] = expected;
        assert_eq!(json_file(&path), expected_document, "{bridge}");
    }
}

#[test]
fn validation_is_atomic_model_provider_changes_use_core_semantics_and_null_resets() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path();
    let path = home.join("settings.json");
    let backend = CoreSettingsBackend::new(home.into()).unwrap();
    fs::write(&path,br#"{"orchestrator_mode":"chat","orchestrator_chat_provider":"claude","orchestrator_chat_model":"saved-model","unknown":"keep"}"#).unwrap();
    for invalid in [
        json!({"tabCloseBehavior":"kill"}),
        json!({"orchestratorMode":"other"}),
        json!({"orchestratorChatProvider":"other"}),
        json!({"orchestratorChatModel":"  "}),
        json!({"orchestratorChatModel":"bad\nmodel"}),
        json!({"orchestratorChatModel":"é".repeat(101)}),
        json!({"delegationPatch":{"parallelShells":0}}),
        json!({"delegationPatch":{"parallelShells":11}}),
        json!({"delegationPatch":{"design":{"provider":"codex","model":" "}}}),
        json!({"delegationPatch":{"design":{"provider":"codex","model":"model\n"}}}),
    ] {
        let before = fs::read(&path).unwrap();
        assert!(
            backend
                .set(patch(invalid))
                .unwrap_err()
                .starts_with("invalid:")
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    assert!(serde_json::from_value::<SettingsPatch>(json!({"agentInlineMode":"yes"})).is_err());
    assert!(
        serde_json::from_value::<SettingsPatch>(json!({"delegationPatch":{"newSetting":true}}))
            .is_err()
    );
    let provider = backend
        .set(patch(json!({"orchestratorChatProvider":"codex"})))
        .unwrap();
    assert_eq!(provider.orchestrator_chat_provider, Provider::Codex);
    assert_eq!(provider.orchestrator_chat_model, None);
    let model = backend
        .set(patch(json!({"orchestratorChatModel":"  custom-model  "})))
        .unwrap();
    assert_eq!(
        model.orchestrator_chat_model.as_deref(),
        Some("custom-model")
    );
    let mode = backend
        .set(patch(json!({"orchestratorMode":"automatic"})))
        .unwrap();
    assert_eq!(
        mode.orchestrator_chat_model.as_deref(),
        Some("custom-model")
    );
    let reset = backend
        .set(patch(json!({"orchestratorChatModel":null})))
        .unwrap();
    assert_eq!(reset.orchestrator_chat_model, None);
    assert_eq!(json_file(&path)["unknown"], "keep");
}

#[test]
fn delegation_field_edits_keep_future_siblings_and_only_null_removes_an_override() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("settings.json");
    fs::write(&path,br#"{"delegation_policy":{"spread_work":false,"parallel_shells":7,"design":{"provider":"future","model":"future"},"future_field":{"preserved":true}},"other":"keep"}"#).unwrap();
    let backend = CoreSettingsBackend::new(fixture.path().into()).unwrap();
    backend
        .set(patch(json!({"delegationPatch":{"parallelShells":3}})))
        .unwrap();
    let document = json_file(&path);
    assert_eq!(document["delegation_policy"]["spread_work"], false);
    assert_eq!(document["delegation_policy"]["parallel_shells"], 3);
    assert_eq!(
        document["delegation_policy"]["design"]["provider"],
        "future"
    );
    assert_eq!(
        document["delegation_policy"]["future_field"]["preserved"],
        true
    );
    let current = backend
        .set(patch(json!({"delegationPatch":{"spreadWork":null}})))
        .unwrap();
    assert!(current.delegation_policy.spread_work.is_none());
    assert!(
        !json_file(&path)["delegation_policy"]
            .as_object()
            .unwrap()
            .contains_key("spread_work")
    );
    let projected = SettingsDto::from_settings(&current, None);
    assert!(projected.delegation.spread_work);
    assert_eq!(projected.delegation.parallel_shells, 3);
    assert!(
        projected
            .delegation
            .unreadable_note
            .unwrap()
            .contains("future_field")
    );
}

#[test]
fn strict_opt_in_damage_refuses_form_saves_and_status_mutations_keep_other_fields() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("settings.json");
    let backend = CoreSettingsBackend::new(fixture.path().into()).unwrap();
    for damaged in [
        json!({"schema_version":2}),
        json!({"selected_codex_account":false}),
        json!({"codex_subscription_balancing":"yes"}),
        json!({"dashboard_haiku_summaries":"yes"}),
    ] {
        fs::write(&path, serde_json::to_vec(&damaged).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(
            backend
                .set(patch(json!({"rememberWindowSize":false})))
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    fs::write(
        &path,
        br#"{"unknown":{"keep":true},"agent_inline_mode":false}"#,
    )
    .unwrap();
    backend
        .change_status(StatusChange::UsageMode(
            riwork_core::status_bar::UsageMode::AllAccounts,
        ))
        .unwrap();
    assert_eq!(json_file(&path)["unknown"]["keep"], true);
    assert_eq!(json_file(&path)["agent_inline_mode"], false);
    let reset = backend.change_status(StatusChange::Reset).unwrap();
    assert_eq!(reset.status_bar, Default::default());
    assert_eq!(json_file(&path)["unknown"]["keep"], true);
}

#[test]
fn form_defaults_and_saved_model_route_choices_come_from_core_without_changing_files() {
    let fixture = tempfile::tempdir().unwrap();
    let prefs = PreferenceService::new(fixture.path().into()).unwrap();
    let defaults = prefs.get();
    assert!(defaults.remember_window_size);
    assert!(defaults.agent_inline_mode);
    assert!(defaults.agent_terminals_unrestricted);
    assert!(!defaults.dictation_mic);
    assert!(!defaults.codex_subscription_balancing);
    assert!(!defaults.dashboard_haiku_summaries);
    assert_eq!(defaults.tab_close_behavior, "ask");
    assert!(!defaults.claude_chat_ignores_api_key);
    assert_eq!(
        Settings::default().claude_chat_billing,
        ClaudeBilling::ApiKey
    );
    assert!(!fixture.path().join("settings.json").exists());
    let mut settings = Settings::default();
    settings.orchestrator_chat_provider = Provider::Muse;
    settings.orchestrator_chat_model = Some("custom-muse".into());
    settings.delegation_policy.design = Some(riwork_core::delegation::Route {
        provider: Provider::Claude,
        model: "custom-design".into(),
    });
    let dto = SettingsDto::from_settings(&settings, None);
    assert_eq!(dto.model_choices.last().unwrap().id, "saved");
    assert_eq!(
        dto.model_choices.last().unwrap().model.as_deref(),
        Some("custom-muse")
    );
    assert_eq!(
        dto.delegation.design_choices.last().unwrap().route.model,
        "custom-design"
    );
}

#[derive(Clone)]
struct FrozenSessions {
    value: SessionsSnapshot,
}
impl SessionsBackend for FrozenSessions {
    fn list(&self) -> anyhow::Result<SessionsSnapshot> {
        Ok(self.value.clone())
    }
}
struct ProjectFixture {
    _temp: Option<tempfile::TempDir>,
    home: PathBuf,
    root: PathBuf,
    project: String,
    store: Store,
    backend: CoreStoreBackend,
}
impl ProjectFixture {
    fn new() -> Self {
        let (temp, base) =
            if let Some(base) = std::env::var_os("RIWORK_NSH_PROJECT_FORM_FIXTURE_BASE") {
                (None, PathBuf::from(base).canonicalize().unwrap())
            } else {
                let temp = tempfile::tempdir().unwrap();
                let base = temp.path().canonicalize().unwrap();
                (Some(temp), base)
            };
        let home = base.join("home");
        let root = base.join("repo");
        fs::create_dir(&root).unwrap();
        let store = Store::open(&home).unwrap();
        let project = store.add_project(&root, Some("Fixture")).unwrap().id;
        let profile = home.join("synthetic-orca");
        fs::create_dir_all(profile.join("codex-accounts/saved-a/home")).unwrap();
        fs::write(home.join("codex-accounts.json"),serde_json::to_vec(&json!({"version":1,"user_data":profile,"accounts":[{"id":"saved-a","email":"fixture@example.invalid","workspaceLabel":"Fixture","managedHomeRuntime":"host"}],"source_active_id":"saved-a"})).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                home.join("codex-accounts.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        Self {
            _temp: temp,
            backend: CoreStoreBackend::new(home.clone()),
            home,
            root,
            project,
            store,
        }
    }
    fn accounts(&self) -> AccountsDto {
        AccountsDto {
            schema: 1,
            accounts: vec![CodexAccountDto {
                id: "saved-a".into(),
                label: "Fixture account".into(),
                email: Some("fixture@example.invalid".into()),
                home: "private fixture".into(),
                home_path: self
                    .home
                    .join("synthetic-orca/codex-accounts/saved-a/home")
                    .to_string_lossy()
                    .into(),
                system: false,
                available: true,
                unavailable_reason: None,
                saved: true,
                orca_active: true,
                detail: "Saved in Orca".into(),
            }],
            selected_id: Some("saved-a".into()),
            selected_unavailable: false,
            balancing: false,
            listed: true,
            from_cache: true,
            error: None,
            refreshing: false,
        }
    }
}

#[test]
fn project_form_resolves_inheritance_missing_pins_saved_choices_and_folder_order() {
    let f = ProjectFixture::new();
    let a = f.store.create_project_folder("z-last").unwrap();
    let b = f.store.create_project_folder("Alpha").unwrap();
    f.store
        .create_project_folder_in("nested", Some(&b.id))
        .unwrap();
    let settings:Settings=serde_json::from_value(json!({"orchestrator_mode":"chat","orchestrator_chat_provider":"muse","orchestrator_chat_model":"custom-muse","delegation_policy":{"parallel_shells":6,"workhorse":{"provider":"claude","model":"custom-workhorse"}}})).unwrap();
    let state = f.backend.snapshot().unwrap();
    let form = project_form(&state, &f.project, &settings, None, &f.accounts()).unwrap();
    assert_eq!(form.orchestrator.selected_mode.as_deref(), Some("inherit"));
    assert_eq!(form.orchestrator.provider, "muse");
    assert_eq!(
        form.orchestrator.effective.model.as_deref(),
        Some("custom-muse")
    );
    assert_eq!(form.delegation.selected_parallel_shells, None);
    assert_eq!(form.delegation.effective.parallel_shells, 6);
    assert_eq!(
        form.folders
            .iter()
            .map(|folder| folder.path.as_str())
            .collect::<Vec<_>>(),
        ["Alpha", "Alpha/nested", "z-last"]
    );
    assert_eq!(a.name, "z-last");
    let mut unavailable = state.clone();
    unavailable.projects[0].codex_account = ProjectCodexAccountDto {
        source: "saved".into(),
        account_id: Some("missing-pin".into()),
    };
    unavailable.projects[0].orchestrator = json!({"mode":"future","extra":true});
    let form = project_form(&unavailable, &f.project, &settings, None, &f.accounts()).unwrap();
    assert!(form.selected_account_unavailable);
    assert!(
        form.accounts
            .iter()
            .any(|account| account.id == "missing-pin"
                && !account.available
                && account.label.contains("Saved account unavailable"))
    );
    assert_eq!(form.orchestrator.selected_mode, None);
    assert!(form.orchestrator.unreadable_note.is_some());
}

#[tokio::test]
async fn project_writes_validate_accounts_merge_metadata_and_leave_running_bindings_frozen() {
    const CHILD: &str = "RIWORK_NSH_PROJECT_FORM_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Account-cache profile identity and HOME are configured only for this
        // child. No global environment mutation can redirect parallel tests.
        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().canonicalize().unwrap();
        fs::create_dir(base.join("user")).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "project_writes_validate_accounts_merge_metadata_and_leave_running_bindings_frozen",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("RIWORK_NSH_PROJECT_FORM_FIXTURE_BASE", &base)
            .env("HOME", base.join("user"))
            .env("RIWORK_HOME", base.join("home"))
            .env("CODEX_HOME", base.join("user/.codex"))
            .env("ORCA_USER_DATA_PATH", base.join("home/synthetic-orca"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "private child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let f = ProjectFixture::new();
    let saved:riwork_engine::sessions::ShellSession=serde_json::from_value(json!({"id":"00000000-0000-4000-8000-000000000061","kind":"project","project_id":f.project,"cwd":f.root,"codex_account_id":"frozen-running-account","codex_home":f.home.join("frozen-home"),"created_at_unix":1})).unwrap();
    let saved: ShellSessionDto = saved.into();
    let frozen = SessionsSnapshot {
        sessions: vec![saved.clone()],
    };
    let service = ProcessService::with_backends(
        f.home.clone(),
        Arc::new(CoreStoreBackend::new(f.home.clone())),
        Arc::new(FrozenSessions {
            value: frozen.clone(),
        }),
    )
    .unwrap();
    let changes = service.subscribe_store().await.unwrap();
    changes.recv().await.unwrap();
    let folder = f.store.create_project_folder("Work").unwrap();
    let project = service
        .update_project_metadata(
            f.project.clone(),
            "  Renamed  ".into(),
            Some(folder.id.clone()),
        )
        .await
        .unwrap();
    assert_eq!(project.name, "Renamed");
    assert_eq!(project.folder_id, Some(folder.id));
    for source in ["inherit", "system_default"] {
        let result = service
            .set_project_codex_account(
                f.project.clone(),
                ProjectCodexAccountDto {
                    source: source.into(),
                    account_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(result.codex_account.source, source);
    }
    let result = service
        .set_project_codex_account(
            f.project.clone(),
            ProjectCodexAccountDto {
                source: "saved".into(),
                account_id: Some("saved-a".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.codex_account.account_id.as_deref(), Some("saved-a"));
    let before = fs::read(f.home.join("state.json")).unwrap();
    let unavailable = service
        .set_project_codex_account(
            f.project.clone(),
            ProjectCodexAccountDto {
                source: "saved".into(),
                account_id: Some("missing".into()),
            },
        )
        .await
        .unwrap_err();
    assert!(unavailable.starts_with("refused:"));
    assert_eq!(fs::read(f.home.join("state.json")).unwrap(), before);
    let malformed = service
        .set_project_orchestrator(
            f.project.clone(),
            json!({"mode":"chat","provider":"future"}),
        )
        .await
        .unwrap_err();
    assert!(malformed.starts_with("invalid:"));
    service
        .set_project_orchestrator(
            f.project.clone(),
            json!({"mode":"chat","provider":"codex","model":"  fixture-model  "}),
        )
        .await
        .unwrap();
    service
        .set_project_delegation_policy(
            f.project.clone(),
            serde_json::from_value(json!({"parallelShells":3})).unwrap(),
        )
        .await
        .unwrap();
    service
        .set_project_delegation_policy(
            f.project.clone(),
            serde_json::from_value(json!({"spreadWork":false})).unwrap(),
        )
        .await
        .unwrap();
    let state = f.store.snapshot().unwrap();
    let selected = state.project(&f.project).unwrap();
    assert_eq!(selected.delegation_policy.parallel_shells, Some(3));
    assert_eq!(selected.delegation_policy.spread_work, Some(false));
    assert!(
        matches!(selected.orchestrator.choice(),Some(OrchestratorRuns::Chat{model:Some(model),..}) if model=="fixture-model")
    );
    assert_eq!(
        service.sessions_list().await.unwrap(),
        frozen,
        "new-session preferences must not rebind running shells"
    );
    assert_eq!(
        saved.codex_account_id.as_deref(),
        Some("frozen-running-account")
    );
    assert!(
        service
            .update_project_metadata(f.project.clone(), " ".into(), None)
            .await
            .unwrap_err()
            .starts_with("invalid:")
    );
}
