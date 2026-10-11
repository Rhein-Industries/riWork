use super::*;
use crate::chat::{
    client::socket_path,
    host::Options,
    model::{ApprovalMode, Provider},
    testing::*,
};
use crate::schedules::{ChatDelivery, Outcome, Schedule, ScheduleStore, Scope, Timing};
use crate::store::Store;
use std::{
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
};

fn in_process(home: &Path) -> Result<PathBuf, String> {
    Ok(socket_path(home))
}
fn delivery() -> ChatDelivery {
    ChatDelivery {
        ensure: in_process,
        // The capability check gets min(wait, 2 s): shorter, a loaded machine
        // defers the occurrence instead of creating the chat.
        proof: Proof {
            wait: Duration::from_secs(2),
            poll: Duration::from_millis(5),
        },
    }
}
/// The next connection to a stand-in host. Fails instead of hanging when the
/// scheduler connects fewer times than the test expects, and reads time out.
fn accept_within(listener: &UnixListener) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // macOS can inherit O_NONBLOCK from the listener.
                stream.set_nonblocking(false).unwrap();
                // A peer that already left makes macOS refuse the timeout
                // (EINVAL); reads on that stream end at once anyway.
                if let Err(error) = stream.set_read_timeout(Some(Duration::from_secs(10))) {
                    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{error}");
                }
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < end,
                    "the scheduler never connected to the stand-in"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("stand-in accept failed: {error}"),
        }
    }
}
fn setup(provider: Provider, recurring: bool) -> (TestHost, ScheduleStore, Schedule) {
    let mut providers = fake_providers();
    providers.account = crate::chat::launch::account_for;
    let host = TestHost::with_providers(quick_options(), providers);
    let workspace = Store::open(host.home.clone()).unwrap();
    let project = workspace.add_project(&host.work(), None).unwrap();
    let state = workspace.snapshot().unwrap();
    let target = Target::bind_new_chat(
        &host.home,
        &state,
        &project.id,
        provider,
        Some("fixture-model".into()),
        Some("high".into()),
        true,
        ApprovalMode::Supervised,
        None,
    )
    .unwrap();
    let store = ScheduleStore::at(host.home.clone())
        .unwrap()
        .with_chat(delivery());
    let schedule = store
        .save(
            None,
            "Fresh fixture".into(),
            "Exactly once".into(),
            target,
            if recurring {
                Timing::Interval {
                    first: 100,
                    seconds: 300,
                }
            } else {
                Timing::Once { at: 100 }
            },
            99,
        )
        .unwrap();
    (host, store, schedule)
}
fn row(store: &ScheduleStore) -> Schedule {
    store.list().unwrap().remove(0)
}
fn sends(host: &TestHost) -> usize {
    fake_for(&host.work().canonicalize().unwrap())
        .commands()
        .iter()
        .filter(|c| matches!(c, ChatCommand::Send { .. }))
        .count()
}

#[test]
fn fresh_chat_once_both_providers_pin_configuration_and_leave_orchestrator_alone() {
    for provider in [Provider::Codex, Provider::Claude] {
        let (host, store, _) = setup(provider, false);
        let mut new = host.new_chat(provider);
        new.orchestrator = Some(chat::model::OrchestratorScope::Global);
        let orch = host.client().create(new).unwrap();
        store.tick(99).unwrap();
        assert_eq!(host.client().list().unwrap().len(), 1);
        store.tick(100).unwrap();
        let run = row(&store).last_run.unwrap();
        assert_eq!(run.outcome, Outcome::Submitted, "{run:?}");
        let created = host.info(run.created_chat_id.as_ref().unwrap());
        assert!(created.orchestrator.is_none());
        assert!(created.project_id.is_some());
        assert_eq!(created.title, "Fresh fixture");
        assert_eq!(created.approval_mode, ApprovalMode::Supervised);
        assert_eq!(created.model.as_deref(), Some("fixture-model"));
        assert_eq!(created.effort.as_deref(), Some("high"));
        assert!(created.fast);
        assert_eq!(host.info(&orch.id).id, orch.id);
        assert_eq!(sends(&host), 1);
        ScheduleStore::at(host.home.clone())
            .unwrap()
            .with_chat(delivery())
            .tick(101)
            .unwrap();
        assert_eq!(sends(&host), 1);
    }
}
#[test]
fn fresh_chat_racing_ticks_and_host_restart_create_once_per_occurrence() {
    let (mut host, store, _) = setup(Provider::Claude, true);
    let barrier = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.tick(100).unwrap();
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(host.client().list().unwrap().len(), 1);
    assert_eq!(sends(&host), 1);
    let first = row(&store).last_run.unwrap().created_chat_id.unwrap();
    host.restart(Options::default());
    store.tick(101).unwrap();
    assert_eq!(sends(&host), 1);
    store.tick(400).unwrap();
    assert_eq!(sends(&host), 2);
    assert_eq!(host.client().list().unwrap().len(), 2);
    assert_ne!(
        row(&store).last_run.unwrap().created_chat_id.unwrap(),
        first
    );
    assert!(
        crate::chat::log::read_infos(&host.home)
            .iter()
            .any(|c| c.id == first)
    );
}
#[test]
fn fresh_chat_failed_start_keeps_created_id_and_never_sends_or_retries() {
    let (host, store, _) = setup(Provider::Claude, false);
    *fake_for(&host.work().canonicalize().unwrap())
        .fail_start
        .lock()
        .unwrap() = Some("fixture start failed".into());
    store.tick(100).unwrap();
    let current = row(&store);
    assert_eq!(current.last_run.as_ref().unwrap().outcome, Outcome::Failed);
    assert!(current.paused && current.review_required);
    let id = current.last_run.unwrap().created_chat_id.unwrap();
    assert!(host.info(&id).project_id.is_some());
    store.tick(101).unwrap();
    assert_eq!(sends(&host), 0);
    assert_eq!(host.client().list().unwrap().len(), 1);
}
#[test]
fn fresh_chat_invalid_project_root_or_account_does_not_claim_or_create() {
    let (host, store, schedule) = setup(Provider::Claude, false);
    let workspace = Store::open(host.home.clone()).unwrap();
    let mut target = schedule.target.clone();
    target.new_chat.as_mut().unwrap().root = host.home.join("missing");
    assert!(
        target
            .validate_new_chat(&host.home, &workspace.snapshot().unwrap())
            .is_err()
    );
    assert!(
        Target::bind_new_chat(
            &host.home,
            &workspace.snapshot().unwrap(),
            &Uuid::new_v4().to_string(),
            Provider::Claude,
            None,
            None,
            false,
            ApprovalMode::Supervised,
            None
        )
        .is_err()
    );
    let Scope::Project { project_id } = &schedule.target.scope else {
        panic!()
    };
    assert!(
        Target::bind_new_chat(
            &host.home,
            &workspace.snapshot().unwrap(),
            project_id,
            Provider::Codex,
            None,
            None,
            false,
            ApprovalMode::Supervised,
            Some("missing-account")
        )
        .is_err()
    );
    std::fs::remove_dir(host.work()).unwrap();
    store.tick(100).unwrap();
    assert_eq!(row(&store).last_run.unwrap().outcome, Outcome::Failed);
    assert!(host.client().list().unwrap().is_empty());
}
#[test]
fn fresh_chat_interrupted_claim_preserves_id_and_old_serialized_schedules_load() {
    let (host, store, _) = setup(Provider::Claude, false);
    store.tick(100).unwrap();
    let path = host.home.join("schedules.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["schedules"][0]["last_run"]["outcome"] = serde_json::json!("dispatching");
    let id = json["schedules"][0]["last_run"]["created_chat_id"].clone();
    std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    store.tick(101).unwrap();
    assert_eq!(row(&store).last_run.unwrap().outcome, Outcome::Uncertain);
    assert_eq!(sends(&host), 1);
    assert_eq!(
        serde_json::to_value(row(&store)).unwrap()["last_run"]["created_chat_id"],
        id
    );
    // Fields absent in old ledgers deserialize with the original destination.
    json["schedules"][0]["target"]
        .as_object_mut()
        .unwrap()
        .remove("new_chat");
    json["schedules"][0]["last_run"]
        .as_object_mut()
        .unwrap()
        .remove("created_chat_id");
    std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    let old = row(&store);
    assert!(old.target.new_chat.is_none());
    assert!(old.last_run.unwrap().created_chat_id.is_none());
}

#[derive(Clone, Copy)]
enum Failure {
    CreateRefused,
    CreateBroken,
    SendRefused,
    SendBroken,
    ProofMissing,
    /// One field of the created chat differs from the request; every field is
    /// compared by the same check.
    WrongAccount,
}
#[test]
fn fresh_chat_failed_or_ambiguous_create_send_never_retries_and_claim_precedes_create() {
    use crate::chat::{
        log::ChatLog,
        model::{ChatInfo, ChatState},
        wire::{Request, Response},
    };
    use std::io::{BufRead, BufReader, Write};
    for failure in [
        Failure::CreateRefused,
        Failure::CreateBroken,
        Failure::SendRefused,
        Failure::SendBroken,
        Failure::ProofMissing,
        Failure::WrongAccount,
    ] {
        let (mut host, store, _schedule) = setup(Provider::Claude, false);
        // Stop only this fake host; the stand-in handles exchanges whose outcome
        // the real host normally never breaks.
        let home = host.home.clone();
        host.stop();
        std::fs::create_dir_all(home.join("run")).unwrap();
        let socket = socket_path(&home);
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let served_home = home.clone();
        let thread = std::thread::spawn(move || {
            let stream = accept_within(&listener);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut chat = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    return;
                }
                let req: Request = serde_json::from_str(&line).unwrap();
                let response = match req {
                    Request::Capabilities { id } => Response {
                        id,
                        ok: true,
                        result: Some(serde_json::json!({"identified_create":true})),
                        error: None,
                    },
                    Request::Create {
                        id,
                        chat: new,
                        chat_id: Some(chat_id),
                    } => {
                        let ledger: serde_json::Value = serde_json::from_slice(
                            &std::fs::read(served_home.join("schedules.json")).unwrap(),
                        )
                        .unwrap();
                        assert_eq!(ledger["schedules"][0]["last_run"]["outcome"], "dispatching");
                        assert_eq!(
                            ledger["schedules"][0]["last_run"]["created_chat_id"],
                            chat_id
                        );
                        if matches!(failure, Failure::CreateBroken) {
                            return;
                        }
                        if matches!(failure, Failure::CreateRefused) {
                            Response {
                                id,
                                ok: false,
                                result: None,
                                error: Some("fixture creation refused".into()),
                            }
                        } else {
                            let mut info = ChatInfo {
                                parent_id: None,
                                user_title: None,
                                first_user_message: None,
                                provider_title: None,

                                id: chat_id,
                                provider: new.provider,
                                project_id: new.project_id,
                                worktree_id: None,
                                cwd: new.cwd,
                                title: new.title.unwrap(),
                                created_at_unix: 100,
                                provider_thread_id: None,
                                model: new.model,
                                effort: new.effort,
                                fast: new.fast,
                                approval_mode: new.approval_mode,
                                codex_account_id: new.codex_account_id,
                                state: ChatState::Idle,
                                orchestrator: None,
                                carried_over: None,
                            };
                            if matches!(failure, Failure::WrongAccount) {
                                info.codex_account_id = Some("other-account".into());
                            }
                            ChatLog::create(&served_home.join("chats").join(&info.id), &info)
                                .unwrap();
                            chat = Some(info.clone());
                            Response {
                                id,
                                ok: true,
                                result: Some(serde_json::to_value(info).unwrap()),
                                error: None,
                            }
                        }
                    }
                    Request::List { id } => Response {
                        id,
                        ok: true,
                        result: Some(serde_json::to_value(vec![chat.clone().unwrap()]).unwrap()),
                        error: None,
                    },
                    Request::Command {
                        id,
                        command: ChatCommand::Send { text },
                        ..
                    } => {
                        assert!(
                            matches!(
                                failure,
                                Failure::SendBroken | Failure::SendRefused | Failure::ProofMissing
                            ),
                            "Mismatched destination received Send"
                        );
                        assert_eq!(text, "Exactly once");
                        if matches!(failure, Failure::SendBroken) {
                            return;
                        }
                        Response {
                            id,
                            ok: !matches!(failure, Failure::SendRefused),
                            result: None,
                            error: Some("fixture send refused".into()),
                        }
                    }
                    other => panic!("Unexpected {other:?}"),
                };
                writeln!(writer, "{}", serde_json::to_string(&response).unwrap()).unwrap();
            }
        });
        store.tick(100).unwrap();
        thread.join().unwrap();
        let current = row(&store);
        let expected = if matches!(failure, Failure::CreateRefused | Failure::SendRefused) {
            Outcome::Failed
        } else {
            Outcome::Uncertain
        };
        assert_eq!(current.last_run.as_ref().unwrap().outcome, expected);
        assert!(current.last_run.as_ref().unwrap().created_chat_id.is_some());
        assert!(current.paused && current.review_required);
        store.tick(101).unwrap();
        assert_eq!(row(&store).revision, current.revision);
    }
}

#[test]
fn preassigned_chat_identity_refuses_duplicates_even_after_host_restart() {
    let mut host = TestHost::new();
    let id = Uuid::new_v4().to_string();
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..3)
        .map(|_| {
            let barrier = barrier.clone();
            let socket = host.socket();
            let new = host.new_chat(Provider::Claude);
            let id = id.clone();
            std::thread::spawn(move || {
                barrier.wait();
                crate::chat::client::Client::connect(&socket)
                    .unwrap()
                    .create_identified(&id, new)
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(CallError::Refused(_))))
            .count(),
        2
    );
    host.restart(Options::default());
    assert!(matches!(
        host.client()
            .create_identified(&id, host.new_chat(Provider::Claude)),
        Err(CallError::Refused(_))
    ));
    assert_eq!(host.client().list().unwrap().len(), 1);
    assert!(host.fake().commands().is_empty());
}

#[test]
fn fresh_chat_saved_account_stays_pinned_and_missing_home_fails_before_creation() {
    const CHILD: &str = "RIWORK_FRESH_ACCOUNT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = short_home();
        let profile = root.join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let output=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","schedule_chat::fresh_tests::fresh_chat_saved_account_stays_pinned_and_missing_home_fails_before_creation","--nocapture"])
            .env(CHILD,&profile).env("ORCA_USER_DATA_PATH",&profile).env("RIWORK_HOME",root.join("state")).env("RIWORK_RUNTIME_DIR",root.join("runtime"))
            .output().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let profile = PathBuf::from(std::env::var_os(CHILD).unwrap());
    for account in ["account-a", "account-b"] {
        std::fs::create_dir_all(profile.join("codex-accounts").join(account).join("home")).unwrap();
    }
    let mut providers = fake_providers();
    providers.account = crate::chat::launch::account_for;
    let host = TestHost::with_providers(quick_options(), providers);
    let cache = host.home.join("codex-accounts.json");
    std::fs::write(&cache,serde_json::to_vec(&serde_json::json!({"version":1,"user_data":profile,"accounts":[{"id":"account-a","managedHomeRuntime":"host"},{"id":"account-b","managedHomeRuntime":"host"}],"source_active_id":"account-b"})).unwrap()).unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o600)).unwrap();
    let workspace = Store::open(host.home.clone()).unwrap();
    let project = workspace.add_project(host.work(), None).unwrap();
    workspace
        .set_project_codex_account(
            &project.id,
            crate::store::ProjectCodexAccount::Saved("account-a".into()),
        )
        .unwrap();
    let target = Target::bind_new_chat(
        &host.home,
        &workspace.snapshot().unwrap(),
        &project.id,
        Provider::Codex,
        None,
        None,
        false,
        ApprovalMode::Supervised,
        None,
    )
    .unwrap();
    assert_eq!(
        target
            .new_chat
            .as_ref()
            .unwrap()
            .codex_account_id
            .as_deref(),
        Some("account-a")
    );
    let store = ScheduleStore::at(host.home.clone())
        .unwrap()
        .with_chat(delivery());
    store
        .save(
            None,
            "Account fixture".into(),
            "Exactly once".into(),
            target,
            Timing::Interval {
                first: 100,
                seconds: 300,
            },
            99,
        )
        .unwrap();
    store.tick(100).unwrap();
    workspace
        .set_project_codex_account(
            &project.id,
            crate::store::ProjectCodexAccount::Saved("account-b".into()),
        )
        .unwrap();
    store.tick(400).unwrap();
    assert_eq!(sends(&host), 2);
    assert!(
        host.client()
            .list()
            .unwrap()
            .iter()
            .all(|chat| chat.codex_account_id.as_deref() == Some("account-a"))
    );
    std::fs::remove_dir(profile.join("codex-accounts/account-a/home")).unwrap();
    store.tick(700).unwrap();
    assert_eq!(
        row(&store).last_run.as_ref().unwrap().outcome,
        Outcome::Failed
    );
    assert!(row(&store).last_run.unwrap().created_chat_id.is_none());
    assert_eq!(host.client().list().unwrap().len(), 2);
    assert_eq!(sends(&host), 2);
}

#[test]
fn fresh_chat_capability_preflight_defers_old_or_unproven_hosts_without_claim_or_starvation() {
    use std::io::{BufRead, BufReader, Write};
    // The legacy stand-in would accept Create and ignore chat_id. It must never
    // receive that operation (nor Command) through normal scheduled dispatch.
    // One case per way the check can fail: an old host's refusal, an explicit
    // no, an answer that cannot be read, and no answer in time.
    for behavior in ["unknown_op", "false", "malformed", "timeout"] {
        let (mut host, store, schedule) = setup(Provider::Claude, false);
        host.stop();
        let home = host.home.clone();
        let socket = socket_path(&home);
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(socket).unwrap();
        let served = std::thread::spawn(move || {
            for _ in 0..3 {
                let mut stream = accept_within(&listener);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(
                    request["op"], "capabilities",
                    "legacy host received a mutation"
                );
                let mut response = serde_json::json!({"id":request["id"],"ok":true,"result":{"identified_create":true}});
                match behavior {
                    "unknown_op" => {
                        response =
                            serde_json::json!({"id":request["id"],"ok":false,"error":behavior})
                    }
                    "false" => response["result"]["identified_create"] = false.into(),
                    "malformed" => response["result"]["identified_create"] = "true".into(),
                    _ => {}
                }
                // A stand-in that never answers waits for the client to give up.
                if behavior != "timeout" {
                    writeln!(stream, "{response}").unwrap();
                }
                line.clear();
                // EOF after deferral proves the connection was not used to Create.
                assert_eq!(reader.read_line(&mut line).unwrap(), 0);
            }
        });
        let workspace = Store::open(home.clone()).unwrap();
        let state = workspace.snapshot().unwrap();
        let host_ref = ChatHost {
            home: &home,
            ensure: &in_process,
        };
        // Long enough that a loaded machine still answers in time, short
        // enough that the stand-in that never answers costs little.
        let proof = Proof {
            wait: Duration::from_millis(400),
            poll: Duration::from_millis(2),
        };
        let mut legacy = schedule.target.clone();
        legacy.new_chat = None;
        legacy.shell_id = Uuid::new_v4().to_string();
        let other = store
            .save(
                None,
                "Legacy due".into(),
                "Fixture".into(),
                legacy,
                Timing::Once { at: 101 },
                99,
            )
            .unwrap();
        for at in [100, 115, 130] {
            let reloaded = ScheduleStore::at(home.clone()).unwrap();
            reloaded
                .tick_with(at, |target, prompt, claim| {
                    if target.new_chat.is_some() {
                        deliver_new(&host_ref, proof, target, &state, prompt, claim)
                    } else {
                        assert!(claim("legacy-fixture-turn")?);
                        Ok(Delivery::Submitted)
                    }
                })
                .unwrap();
            let rows = store.list().unwrap();
            let fresh = rows.iter().find(|s| s.id == schedule.id).unwrap();
            assert_eq!(
                fresh.last_run.as_ref().unwrap().outcome,
                Outcome::Deferred,
                "{behavior}"
            );
            assert!(fresh.last_run.as_ref().unwrap().created_chat_id.is_none());
            assert!(!fresh.paused && fresh.next_run == Some(100));
            let ledger: serde_json::Value =
                serde_json::from_slice(&std::fs::read(home.join("schedules.json")).unwrap())
                    .unwrap();
            assert_eq!(ledger["sends_in_window"], if at == 100 { 0 } else { 1 });
            assert!(
                !ledger["consumed"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r[0] == schedule.target.shell_id)
            );
            // The oldest deferred check backs off so the legacy schedule proceeds.
            if at == 100 {
                reloaded
                    .tick_with(101, |target, _, claim| {
                        assert!(target.new_chat.is_none());
                        assert!(claim("legacy-fixture-turn")?);
                        Ok(Delivery::Submitted)
                    })
                    .unwrap();
                assert_eq!(
                    store
                        .list()
                        .unwrap()
                        .iter()
                        .find(|s| s.id == other.id)
                        .unwrap()
                        .last_run
                        .as_ref()
                        .unwrap()
                        .outcome,
                    Outcome::Submitted
                );
            }
        }
        served.join().unwrap();
        store
            .tick_with(401, |_, _, _| panic!("expired capability check replayed"))
            .unwrap();
        assert_eq!(
            store
                .list()
                .unwrap()
                .iter()
                .find(|s| s.id == schedule.id)
                .unwrap()
                .last_run
                .as_ref()
                .unwrap()
                .outcome,
            Outcome::Missed
        );
        assert!(crate::chat::log::read_infos(&home).is_empty());
    }
}
