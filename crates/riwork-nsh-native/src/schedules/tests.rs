//! Private stores and an existing in-process fake chat host only. No dispatch
//! daemon, tmux, external provider, credentials, network or application windows.
use super::*;
use riwork_engine::{
    chat::{
        model::{ChatCommand, ChatState, OrchestratorScope},
        testing::TestHost,
    },
    schedules::Outcome,
    sessions::{HarnessKind, ShellKind},
};

struct Fixture {
    _directory: tempfile::TempDir,
    adapter: EngineSchedules,
    project: String,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("RIWORK_HOME");
        let root = directory.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let project = Store::open(home.clone())
            .unwrap()
            .add_project(root, Some("Fixture"))
            .unwrap();
        Self {
            _directory: directory,
            adapter: EngineSchedules::new(home),
            project: project.id,
        }
    }
    fn scope(&self) -> ScheduleScopeDto {
        ScheduleScopeDto {
            scope: "project".into(),
            project_id: Some(self.project.clone()),
            worktree_id: None,
        }
    }
    fn fresh(&self) -> CreateScheduleDto {
        CreateScheduleDto {
            scope: self.scope(),
            title: "Private automation".into(),
            prompt: "Inert fake prompt".into(),
            at: iso_time(engine::now() + 600),
            every_minutes: None,
            binding: None,
            options: Some(ScheduleChatOptionsDto {
                provider: "claude".into(),
                ..Default::default()
            }),
        }
    }
    fn seed(&self) -> ScheduleDto {
        let target = Target {
            scope: Scope::App,
            shell_id: uuid::Uuid::new_v4().to_string(),
            created_at: 1,
            command: Some("inert-fixture".into()),
            harness: HarnessKind::Codex,
            codex_home: None,
            pane_identity: "private-pane".into(),
            provider_session: "private-provider".into(),
            chat: None,
            new_chat: None,
            shell_kind: Some(ShellKind::Orchestrator),
        };
        schedule_dto(
            &self
                .adapter
                .ledger()
                .unwrap()
                .save(
                    None,
                    "Fixture".into(),
                    "Inert prompt".into(),
                    target,
                    Timing::Once {
                        at: engine::now() + 600,
                    },
                    engine::now(),
                )
                .unwrap(),
        )
    }
    fn bytes(&self) -> Vec<u8> {
        std::fs::read(self.adapter.home.join("schedules.json")).unwrap()
    }
}
fn update(row: &ScheduleDto) -> UpdateScheduleDto {
    UpdateScheduleDto {
        key: row.key.clone(),
        title: row.title.clone(),
        prompt: row.prompt.clone(),
        at: iso_time(engine::now() + 1200),
        every_minutes: Some(60),
        binding: None,
        options: row.options.clone(),
    }
}

#[test]
fn p6b_sched_01_timing_is_offset_explicit_whole_second_and_fixed_utc_cadence() {
    let first = parse_timing("2030-03-31T01:30:00+01:00", Some(60)).unwrap();
    let same = parse_timing("2030-03-31T00:30:00Z", Some(60)).unwrap();
    assert_eq!(first, same);
    assert_eq!(first.next_after(first.first()), Some(first.first() + 3600));
    for at in [
        "2030-03-31T01:30:00",
        "2030-02-31T01:30:00Z",
        "2030-03-31T01:30:00.1Z",
        "1969-12-31T23:59:59Z",
    ] {
        assert!(parse_timing(at, None).is_err(), "{at}");
    }
    assert!(parse_timing("2030-03-31T01:30:00Z", Some(0)).is_err());
    assert!(parse_timing("2030-03-31T01:30:00Z", Some(4)).is_err());
    assert!(parse_timing("2030-03-31T01:30:00Z", Some(525601)).is_err());
}

#[test]
fn p6b_sched_02_real_ledger_crud_preserves_target_and_rejects_stale_or_wrong_keys() {
    let fixture = Fixture::new();
    let initial = fixture.seed();
    let pinned = fixture.adapter.row(&initial.key.id).unwrap().target;
    assert_eq!(fixture.adapter.show(&initial.key.id).unwrap(), initial);
    let paused = fixture.adapter.pause(&initial.key, true).unwrap();
    assert!(paused.paused);
    let before = fixture.bytes();
    assert!(
        fixture
            .adapter
            .update(update(&initial))
            .unwrap_err()
            .starts_with("revision_conflict:")
    );
    let mut wrong = paused.key.clone();
    wrong.target_id = uuid::Uuid::new_v4().to_string();
    assert!(
        fixture
            .adapter
            .delete(&wrong)
            .unwrap_err()
            .starts_with("target_mismatch:")
    );
    assert_eq!(before, fixture.bytes(), "refusals do not mutate the ledger");
    let resumed = fixture.adapter.pause(&paused.key, false).unwrap();
    let edited = fixture.adapter.update(update(&resumed)).unwrap();
    assert_eq!(fixture.adapter.row(&edited.key.id).unwrap().target, pinned);
    assert_eq!(edited.every_minutes, Some(60));
    fixture.adapter.delete(&edited.key).unwrap();
    assert!(fixture.adapter.list(None).unwrap().is_empty());
    assert!(
        fixture
            .adapter
            .show(&edited.key.id)
            .unwrap_err()
            .starts_with("not_found:")
    );
}

#[test]
fn p6b_sched_03_new_chat_options_edit_keeps_account_root_provider_and_destination_pinned() {
    let fixture = Fixture::new();
    let created = fixture.adapter.create(fixture.fresh(), true).unwrap();
    let pinned = fixture.adapter.row(&created.key.id).unwrap().target;
    let mut request = update(&created);
    let options = request.options.as_mut().unwrap();
    options.model = Some("fixture-model".into());
    options.effort = Some("high".into());
    options.permission = "plan".into();
    let edited = fixture.adapter.update(request).unwrap();
    let target = fixture.adapter.row(&edited.key.id).unwrap().target;
    assert_eq!(target.shell_id, pinned.shell_id);
    assert_eq!(target.codex_home, pinned.codex_home);
    assert_eq!(
        target.new_chat.as_ref().unwrap().root,
        pinned.new_chat.as_ref().unwrap().root
    );
    assert_eq!(
        target.new_chat.as_ref().unwrap().codex_account_id,
        pinned.new_chat.as_ref().unwrap().codex_account_id
    );
    assert_eq!(
        target.new_chat.as_ref().unwrap().approval_mode,
        ApprovalMode::Plan
    );
    assert!(!target.new_chat.as_ref().unwrap().fast, "Fast defaults off");
    let mut forbidden = update(&edited);
    forbidden.options.as_mut().unwrap().provider = "muse".into();
    let before = fixture.bytes();
    assert!(
        fixture
            .adapter
            .update(forbidden)
            .unwrap_err()
            .starts_with("target_mismatch:")
    );
    assert_eq!(before, fixture.bytes());
}

#[test]
fn p6b_sched_04_validation_does_not_invent_a_target_or_overwrite_an_uncertain_run() {
    let fixture = Fixture::new();
    let mut missing = fixture.fresh();
    missing.options = None;
    assert!(
        fixture
            .adapter
            .create(missing, false)
            .unwrap_err()
            .contains("Choose an existing target")
    );
    let mut wrong_scope = fixture.fresh();
    wrong_scope.scope = ScheduleScopeDto::from(&Scope::App);
    assert!(
        fixture
            .adapter
            .create(wrong_scope, true)
            .unwrap_err()
            .contains("project scope")
    );
    let mut bad_prompt = fixture.fresh();
    bad_prompt.prompt = "two\nlines".into();
    assert!(
        fixture
            .adapter
            .create(bad_prompt, true)
            .unwrap_err()
            .starts_with("invalid_argument:")
    );
    let row = fixture.seed();
    let path = fixture.adapter.home.join("schedules.json");
    let mut ledger: serde_json::Value = serde_json::from_slice(&fixture.bytes()).unwrap();
    ledger["schedules"][0]["paused"] = true.into();
    ledger["schedules"][0]["review_required"] = true.into();
    std::fs::write(&path, serde_json::to_vec(&ledger).unwrap()).unwrap();
    assert!(
        fixture
            .adapter
            .pause(&row.key, false)
            .unwrap_err()
            .starts_with("review_required:")
    );
    let reviewed = fixture.adapter.update(update(&row)).unwrap();
    assert!(!reviewed.review_required && !reviewed.paused);
}

#[test]
fn p6b_sched_05_failed_reads_and_idle_clock_are_not_empty_changed_snapshots() {
    let fixture = Fixture::new();
    fixture.seed();
    let mut read = fixture.adapter.read(None).unwrap();
    let previous = read.clone();
    read.now += 60;
    assert_eq!(
        read, previous,
        "idle clock changes cannot wake every window"
    );
    std::fs::write(fixture.adapter.home.join("schedules.json"), "not json").unwrap();
    assert!(
        fixture.adapter.read(None).is_err(),
        "a broken ledger is not an empty successful list"
    );
    assert!(
        fixture_home(&fixture.adapter.home),
        "unit-test initialization never dispatches"
    );
}

#[test]
fn p6b_sched_06_public_engine_tick_delivers_once_across_reopened_store_with_fake_host() {
    // TestHost injects fake providers and an in-memory key store. The socket
    // already exists, so system_ensure neither starts a CLI nor a real host.
    let host = TestHost::new();
    assert!(host.socket().exists());
    let mut request = host.new_chat(Provider::Claude);
    request.orchestrator = Some(OrchestratorScope::Global);
    let chat = host.client().create(request).unwrap();
    let chat = host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    let state = Store::open(host.home.clone()).unwrap().snapshot().unwrap();
    let target = Target::bind_chat(Scope::App, &state, &chat).unwrap();
    let ledger = ScheduleStore::at(host.home.clone()).unwrap();
    let due = engine::now() + 1;
    let row = ledger
        .save(
            None,
            "Fixture once".into(),
            "exactly once inert prompt".into(),
            target,
            Timing::Once { at: due },
            due - 1,
        )
        .unwrap();
    let mut trackers = HashMap::new();
    ledger.tick_tracked(due - 1, &mut trackers).unwrap();
    ledger.tick_tracked(due, &mut trackers).unwrap();
    let after = EngineSchedules::new(host.home.clone())
        .show(&row.id)
        .unwrap();
    assert_eq!(
        after.last_run.as_ref().unwrap().outcome,
        "submitted",
        "{:?}",
        after.last_run
    );
    for now in [due, due + 1, due + 60] {
        ScheduleStore::at(host.home.clone())
            .unwrap()
            .tick_tracked(now, &mut trackers)
            .unwrap();
    }
    let delivered = host.fake().commands().into_iter().filter(|command| matches!(command, ChatCommand::Send { text } if text == "exactly once inert prompt")).count();
    assert_eq!(delivered, 1);
    let saved = ScheduleStore::at(host.home.clone())
        .unwrap()
        .list()
        .unwrap()
        .remove(0);
    assert_eq!(saved.last_run.unwrap().outcome, Outcome::Submitted);
    assert_eq!(saved.revision, after.key.revision);
    assert_eq!(saved.next_run, None);
}

#[test]
fn p6b_sched_07_empty_startup_tick_does_not_create_schedule_ledger() {
    let fixture = Fixture::new();
    let path = fixture.adapter.home.join("schedules.json");
    assert!(!path.exists());
    assert!(fixture.adapter.list(None).unwrap().is_empty());
    fixture
        .adapter
        .ledger()
        .unwrap()
        .tick_tracked(engine::now(), &mut HashMap::new())
        .unwrap();
    assert!(
        !path.exists(),
        "opening a desktop process must not write a fresh schedules ledger"
    );
    assert!(
        fixture.adapter.home.join("schedules.lock").exists(),
        "only the existing engine's lock-file exception is permitted"
    );
}

#[test]
fn p6b_sched_08_target_catalog_requires_live_supported_shell_and_exact_scope() {
    let fixture = Fixture::new();
    let state = Store::open(fixture.adapter.home.clone())
        .unwrap()
        .snapshot()
        .unwrap();
    let root = state
        .projects
        .iter()
        .find(|p| p.id == fixture.project)
        .unwrap()
        .root
        .clone();
    let mut shell: ShellSession = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(), "project_id": fixture.project.clone(),
        "kind": "project", "cwd": root, "created_at_unix": 1, "harness": "codex"
    }))
    .unwrap();
    shell.alive = true;
    let scopes = [
        Scope::App,
        Scope::Project {
            project_id: fixture.project.clone(),
        },
    ];
    let choices = target_choices(&state, &[shell.clone()], &scopes);
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].binding.scope, fixture.scope());
    assert_eq!(choices[0].binding.kind, "shell");
    shell.alive = false;
    assert!(target_choices(&state, &[shell.clone()], &scopes).is_empty());
    shell.alive = true;
    shell.harness = Some(HarnessKind::Muse);
    assert!(target_choices(&state, &[shell.clone()], &scopes).is_empty());
    shell.harness = Some(HarnessKind::Claude);
    shell.project_id = Some(uuid::Uuid::new_v4().to_string());
    assert!(target_choices(&state, &[shell], &scopes).is_empty());
}

#[test]
fn p6b_sched_09_bridge_errors_keep_frozen_codes_and_reopen_detail() {
    let fixture = Fixture::new();
    let initial = fixture.seed();
    fixture.adapter.pause(&initial.key, true).unwrap();
    let conflict = bridge_error(fixture.adapter.update(update(&initial)).unwrap_err());
    assert!(conflict.starts_with("conflict: revision_conflict:"));
    let invalid = bridge_error(parse_timing("2030-03-31T01:30:00", None).unwrap_err());
    assert!(invalid.starts_with("invalid: invalid_argument:"));
    let missing = bridge_error(
        fixture
            .adapter
            .show(&uuid::Uuid::new_v4().to_string())
            .unwrap_err(),
    );
    assert!(missing.starts_with("not_found:"));
}
