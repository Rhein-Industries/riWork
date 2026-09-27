use super::*;
use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

struct Fixture {
    home: PathBuf,
    store: ScheduleStore,
}
impl Fixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("riwork-schedule-test-{}", Uuid::new_v4()));
        let store = ScheduleStore::at(home.clone()).unwrap();
        Self { home, store }
    }
    fn target(&self) -> Target {
        Target {
            scope: Scope::App,
            shell_id: Uuid::new_v4().to_string(),
            created_at: 1,
            command: Some("fixture".into()),
            harness: HarnessKind::Codex,
            codex_home: None,
            pane_identity: "fixture-pane".into(),
            provider_session: Uuid::new_v4().to_string(),
        }
    }
    fn add(&self, timing: Timing) -> Schedule {
        self.store
            .save(
                None,
                "Fixture".into(),
                "Isolated test prompt".into(),
                self.target(),
                timing,
                999,
            )
            .unwrap()
    }
    fn row(&self) -> Schedule {
        self.store.list().unwrap().remove(0)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}
fn submit(
    _: &Target,
    _: &str,
    claim: &mut dyn FnMut(&str) -> Result<bool, String>,
) -> Result<Delivery, String> {
    if claim("turn-1")? {
        Ok(Delivery::Submitted)
    } else {
        Ok(Delivery::Deferred("used turn".into()))
    }
}
#[test]
fn due_boundary_one_time_and_restart_do_not_duplicate() {
    let f = Fixture::new();
    f.add(Timing::Once { at: 1000 });
    f.store
        .tick_with(999, |_, _, _| panic!("early dispatch"))
        .unwrap();
    f.store.tick_with(1000, submit).unwrap();
    assert!(f.row().next_run.is_none());
    assert_eq!(f.row().last_run.unwrap().outcome, Outcome::Submitted);
    ScheduleStore::at(f.home.clone())
        .unwrap()
        .tick_with(1000, |_, _, _| panic!("duplicate"))
        .unwrap();
}
#[test]
fn recurring_cadence_skips_backlog_and_clock_rewind() {
    let f = Fixture::new();
    f.add(Timing::Interval {
        first: 1000,
        seconds: 300,
    });
    f.store.tick_with(1300, submit).unwrap();
    assert_eq!(f.row().next_run, Some(1600));
    f.store
        .tick_with(1000, |_, _, _| panic!("clock moved backward"))
        .unwrap();
    f.store
        .tick_with(10_000, |_, _, _| panic!("catch-up flood"))
        .unwrap();
    assert_eq!(f.row().next_run, Some(10_300));
    assert_eq!(f.row().last_run.unwrap().outcome, Outcome::Missed);
    assert_eq!(
        Timing::Interval {
            first: 1000,
            seconds: 300
        }
        .next_after(u64::MAX),
        None
    );
}
#[test]
fn deferred_is_bounded_and_grace_is_inclusive() {
    let f = Fixture::new();
    f.add(Timing::Once { at: 1000 });
    f.store
        .tick_with(1000, |_, _, _| Ok(Delivery::Deferred("busy".into())))
        .unwrap();
    assert_eq!(f.row().next_run, Some(1000));
    f.store
        .tick_with(1001, |_, _, _| panic!("defer polling bound"))
        .unwrap();
    f.store.tick_with(1300, submit).unwrap();
    assert_eq!(f.row().last_run.unwrap().outcome, Outcome::Submitted);
    let g = Fixture::new();
    g.add(Timing::Once { at: 1000 });
    g.store
        .tick_with(1301, |_, _, _| panic!("too late"))
        .unwrap();
    assert_eq!(g.row().last_run.unwrap().outcome, Outcome::Missed);
    assert!(g.row().next_run.is_none());
}
#[test]
fn pause_resume_edit_delete_and_revision_conflicts() {
    let f = Fixture::new();
    let s = f.add(Timing::Interval {
        first: 1000,
        seconds: 300,
    });
    f.store.pause(&s.id, s.revision, true, 999).unwrap();
    f.store.tick_with(1000, |_, _, _| panic!("paused")).unwrap();
    let s = f.row();
    f.store.pause(&s.id, s.revision, false, 1400).unwrap();
    assert_eq!(f.row().next_run, Some(1600));
    assert!(f.store.delete(&s.id, s.revision).is_err());
    let s = f.row();
    f.store
        .save(
            Some((&s.id, s.revision)),
            "Edited".into(),
            "New isolated prompt".into(),
            s.target,
            Timing::Once { at: 1800 },
            1400,
        )
        .unwrap();
    let s = f.row();
    assert_eq!(s.title, "Edited");
    f.store.delete(&s.id, s.revision).unwrap();
    f.store
        .tick_with(1800, |_, _, _| panic!("deleted"))
        .unwrap();
    assert!(f.store.list().unwrap().is_empty());
}
#[test]
fn failure_and_uncertainty_pause_until_reviewed_future_edit() {
    for uncertain in [false, true] {
        let f = Fixture::new();
        f.add(Timing::Interval {
            first: 1000,
            seconds: 300,
        });
        f.store
            .tick_with(1000, |_, _, claim| {
                if uncertain {
                    assert!(claim("turn")?);
                    Ok(Delivery::Uncertain("transport died".into()))
                } else {
                    Ok(Delivery::Failed("target gone".into()))
                }
            })
            .unwrap();
        let s = f.row();
        assert!(s.paused && s.review_required);
        assert!(f.store.pause(&s.id, s.revision, false, 1001).is_err());
        f.store
            .tick_with(1600, |_, _, _| panic!("fault retried"))
            .unwrap();
        f.store
            .save(
                Some((&s.id, s.revision)),
                s.title,
                s.prompt,
                s.target,
                Timing::Once { at: 1900 },
                1600,
            )
            .unwrap();
        assert!(!f.row().paused && !f.row().review_required);
    }
}
#[test]
fn durable_inflight_claim_is_uncertain_after_owner_crash() {
    let f = Fixture::new();
    f.add(Timing::Interval {
        first: 1000,
        seconds: 300,
    });
    let result = f.store.tick_with(1000, |_, _, claim| {
        assert!(claim("turn")?);
        Err("simulated process death after claim".into())
    });
    assert!(result.is_err());
    let reopened = ScheduleStore::at(f.home.clone()).unwrap();
    reopened
        .tick_with(1001, |_, _, _| panic!("uncertain retry"))
        .unwrap();
    let s = f.row();
    assert!(s.paused && s.review_required);
    assert_eq!(s.last_run.unwrap().outcome, Outcome::Uncertain);
}
#[test]
fn two_schedules_cannot_consume_the_same_idle_turn() {
    let f = Fixture::new();
    let first = f.add(Timing::Once { at: 1000 });
    f.store
        .save(
            None,
            "Second".into(),
            "Second prompt".into(),
            first.target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap();
    f.store.tick_with(1000, submit).unwrap();
    f.store.tick_with(1001, submit).unwrap();
    let rows = f.store.list().unwrap();
    assert_eq!(
        rows.iter()
            .filter(|s| s.last_run.as_ref().unwrap().outcome == Outcome::Submitted)
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|s| s.last_run.as_ref().unwrap().outcome == Outcome::Deferred)
            .count(),
        1
    );
}
#[test]
fn concurrent_threads_send_once() {
    let f = Fixture::new();
    f.add(Timing::Once { at: 1000 });
    let count = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let store = f.store.clone();
            let count = count.clone();
            thread::spawn(move || {
                store
                    .tick_with(1000, |_, _, claim| {
                        assert!(claim("turn")?);
                        count.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(30));
                        Ok(Delivery::Submitted)
                    })
                    .unwrap()
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[test]
fn concurrent_processes_send_once() {
    let f = Fixture::new();
    f.add(Timing::Once { at: 1000 });
    let children: Vec<_> = (0..4)
        .map(|_| {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "schedules::tests::child_tick_worker",
                    "--ignored",
                    "--test-threads=1",
                ])
                .env("RIWORK_HOME", &f.home)
                .env("RIWORK_SCHEDULE_PROCESS_FIXTURE", &f.home)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    assert_eq!(
        fs::read_to_string(f.home.join("attempts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}
#[test]
#[ignore = "launched only by isolated concurrency parent"]
fn child_tick_worker() {
    let home = PathBuf::from(
        std::env::var_os("RIWORK_SCHEDULE_PROCESS_FIXTURE")
            .expect("isolated child fixture required"),
    );
    assert_eq!(std::env::var_os("RIWORK_HOME").unwrap(), home.as_os_str());
    ScheduleStore::at(home.clone())
        .unwrap()
        .tick_with(1000, |_, _, claim| {
            assert!(claim("turn")?);
            let mut file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(home.join("attempts"))
                .unwrap();
            writeln!(file, "sent").unwrap();
            thread::sleep(Duration::from_millis(100));
            Ok(Delivery::Submitted)
        })
        .unwrap();
}
#[test]
fn invalid_store_and_timing_fail_closed() {
    let f = Fixture::new();
    for seconds in [0, 60, 301, 31_536_001] {
        assert!(
            Timing::Interval {
                first: 1000,
                seconds
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        f.store
            .save(
                None,
                "bad".into(),
                "prompt".into(),
                f.target(),
                Timing::Interval {
                    first: 1000,
                    seconds: 0
                },
                999
            )
            .is_err()
    );
    assert!(
        f.store
            .save(
                None,
                "bad".into(),
                "prompt\nsecond".into(),
                f.target(),
                Timing::Once { at: 1000 },
                999
            )
            .is_err()
    );
    fs::write(f.home.join("schedules.json"), "not json").unwrap();
    assert!(
        f.store
            .tick_with(1000, |_, _, _| panic!("corrupt dispatch"))
            .is_err()
    );
}

/// Actual tmux delivery, with a disposable deterministic harness and exact
/// structured lifecycle fixtures. No authenticated Codex/Claude process runs.
struct RealFixture {
    f: Fixture,
    sessions: SessionManager,
    state: State,
    ids: Vec<String>,
}
impl RealFixture {
    fn new() -> Self {
        let f = Fixture::new();
        fs::create_dir_all(f.home.join("project")).unwrap();
        let store = Store::open(f.home.clone()).unwrap();
        let project = store
            .add_project(f.home.join("project"), Some("Isolated scheduling"))
            .unwrap();
        let state = store.snapshot().unwrap();
        let script = f.home.join("harness.py");
        fs::write(&script,r#"import os,sys,tty
from pathlib import Path
tty.setraw(0)
prompt='❯' if '--claude' in sys.argv else '›'
os.write(1,('\x1b[2J\x1b[H'+prompt+' ').encode())
buf=b''
while True:
 b=os.read(0,1)
 if b in (b'\r',b'\n'):
  with open(Path(os.environ['RIWORK_HOME'])/('received-'+os.environ['RIWORK_SHELL_ID']), 'ab') as f: f.write(buf+b'\n')
  buf=b''
  os.write(1,('\x1b[2J\x1b[H'+prompt+' ').encode())
 else: buf+=b
"#).unwrap();
        let sessions = SessionManager::at(f.home.clone()).unwrap();
        let command = format!("/usr/bin/python3 '{}'", script.display());
        let app = sessions
            .orchestrator_create(f.home.join("project"), Some(command.clone()))
            .unwrap();
        let proj = sessions
            .orchestrator_create_for_project(
                project.id.clone(),
                project.root.clone(),
                Some(command.clone()),
            )
            .unwrap();
        let workspace = state.worktrees_for(&project.id)[0];
        let worker = sessions
            .create(
                project.id,
                Some(workspace.id.clone()),
                workspace.path.clone(),
                Some(command),
            )
            .unwrap();
        let ids = vec![app.id, proj.id, worker.id];
        let registry = f.home.join("sessions.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
        for s in value["sessions"].as_array_mut().unwrap() {
            s["harness"] = "codex".into();
            s["codex_home"] = f.home.join("codex").to_string_lossy().as_ref().into();
        }
        fs::write(registry, serde_json::to_vec(&value).unwrap()).unwrap();
        let result = Self {
            f,
            sessions,
            state,
            ids,
        };
        for id in &result.ids {
            result.lifecycle(id, "turn-1", true);
        }
        for id in &result.ids {
            result.wait_for_prompt(id, "›");
        }
        result
    }
    fn wait_for_prompt(&self, id: &str, prompt: &str) {
        for _ in 0..150 {
            if self
                .sessions
                .capture(id, 1)
                .is_ok_and(|screen| screen.lines().any(|line| line.trim() == prompt))
            {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "Disposable harness did not initialize: {}",
            self.sessions.capture(id, 30).unwrap()
        );
    }
    fn lifecycle(&self, id: &str, turn: &str, done: bool) {
        let thread = Uuid::new_v4().to_string();
        let log_home = self.f.home.join("codex");
        let logs = log_home.join("sessions/2026/09/27");
        fs::create_dir_all(&logs).unwrap();
        crate::activity::bind_codex_thread(&self.f.home, id, &thread, &log_home).unwrap();
        let mut records = format!(
            "{}\n{}\n",
            serde_json::json!({"type":"session_meta","payload":{"id":thread,"source":"cli"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":turn}})
        );
        if done {
            records += &format!(
                "{}\n",
                serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":turn}})
            );
        }
        fs::write(
            logs.join(format!("rollout-fixture-{thread}.jsonl")),
            records,
        )
        .unwrap();
    }
    fn scopes(&self) -> Vec<Scope> {
        let project = self.state.projects[0].id.clone();
        let worktree = self.state.worktrees[0].id.clone();
        vec![
            Scope::App,
            Scope::Project {
                project_id: project.clone(),
            },
            Scope::Workspace {
                project_id: project,
                worktree_id: worktree,
            },
        ]
    }
}
impl Drop for RealFixture {
    fn drop(&mut self) {
        for id in &self.ids {
            let _ = self.sessions.close(id);
        }
    }
}
#[test]
fn real_tmux_dispatch_for_all_three_scopes_preserves_identity() {
    let f = RealFixture::new();
    for (i, scope) in f.scopes().into_iter().enumerate() {
        let target = Target::bind(scope, &f.state, &f.sessions, &f.ids[i]).unwrap();
        let identity = target.pane_identity.clone();
        let prompt = format!("isolated scope {i} · literal $() and `text`");
        f.f.store
            .save(
                None,
                format!("Scope {i}"),
                prompt.clone(),
                target,
                Timing::Once {
                    at: 1000 + i as u64,
                },
                999,
            )
            .unwrap();
        f.f.store.tick(1000 + i as u64).unwrap();
        let row =
            f.f.store
                .list()
                .unwrap()
                .into_iter()
                .find(|s| s.title == format!("Scope {i}"))
                .unwrap();
        let run = row.last_run.unwrap();
        assert_eq!(
            run.outcome,
            Outcome::Submitted,
            "scope {i}: {}; screen: {}",
            run.message,
            f.sessions.capture(&f.ids[i], 10).unwrap()
        );
        assert_eq!(
            f.sessions.schedule_pane_identity(&f.ids[i]).unwrap(),
            identity
        );
        for _ in 0..30 {
            if f.f.home.join(format!("received-{}", f.ids[i])).exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            fs::read_to_string(f.f.home.join(format!("received-{}", f.ids[i]))).unwrap(),
            format!("{prompt}\n")
        );
    }
    f.f.store.tick(1100).unwrap();
    for id in &f.ids {
        assert_eq!(
            fs::read_to_string(f.f.home.join(format!("received-{id}")))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}
#[test]
fn real_dispatch_rejects_wrong_scope_busy_provider_change_and_exit() {
    let f = RealFixture::new();
    assert!(Target::bind(Scope::App, &f.state, &f.sessions, &f.ids[2]).is_err());
    let scope = f.scopes().remove(2);
    f.lifecycle(&f.ids[2], "busy", false);
    let target = Target::bind(scope, &f.state, &f.sessions, &f.ids[2]).unwrap();
    f.f.store
        .save(
            None,
            "Busy".into(),
            "must not send".into(),
            target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap();
    f.f.store.tick(1000).unwrap();
    assert_eq!(f.f.row().last_run.unwrap().outcome, Outcome::Deferred);
    assert!(!f.f.home.join(format!("received-{}", f.ids[2])).exists());
    f.lifecycle(&f.ids[2], "new-provider", true);
    f.f.store.tick(1015).unwrap();
    assert_eq!(f.f.row().last_run.unwrap().outcome, Outcome::Failed);
    assert!(f.f.row().paused);
    f.sessions.close(&f.ids[1]).unwrap();
    assert!(Target::bind(f.scopes().remove(1), &f.state, &f.sessions, &f.ids[1]).is_err());
}

#[test]
fn global_delivery_limit_bounds_simultaneous_due_schedules() {
    let f = Fixture::new();
    for _ in 0..7 {
        f.add(Timing::Once { at: 1000 });
    }
    for n in 0..7 {
        f.store.tick_with(1000 + n, submit).unwrap();
    }
    assert_eq!(
        f.store
            .list()
            .unwrap()
            .iter()
            .filter(|s| s
                .last_run
                .as_ref()
                .is_some_and(|r| r.outcome == Outcome::Submitted))
            .count(),
        4
    );
}

/// Opt-in provider acceptance. The caller must provision a fresh disposable
/// RIWORK_HOME and supply all three full shell UUIDs; never discovers sessions.
#[test]
#[ignore = "requires explicitly provisioned disposable provider sessions"]
fn isolated_live_provider_acceptance() {
    let home = PathBuf::from(
        std::env::var_os("RIWORK_SCHEDULE_LIVE_HOME").expect("disposable home required"),
    );
    assert_eq!(std::env::var_os("RIWORK_HOME").unwrap(), home.as_os_str());
    assert!(
        home.join("qa-info.json").is_file(),
        "fixture marker required"
    );
    let state = Store::open(home.clone()).unwrap().snapshot().unwrap();
    let sessions = SessionManager::at(home.clone()).unwrap();
    let schedules = ScheduleStore::at(home.clone()).unwrap();
    assert!(
        schedules.list().unwrap().is_empty(),
        "use a fresh empty scheduling ledger"
    );
    let ids: Vec<_> = std::env::var("RIWORK_SCHEDULE_LIVE_IDS")
        .unwrap()
        .split(',')
        .map(str::to_owned)
        .collect();
    assert_eq!(ids.len(), 3);
    let scopes = [
        Scope::App,
        Scope::Project {
            project_id: state.projects[0].id.clone(),
        },
        Scope::Workspace {
            project_id: state.projects[0].id.clone(),
            worktree_id: state.worktrees[0].id.clone(),
        },
    ];
    for (index, (id, scope)) in ids.iter().zip(scopes).enumerate() {
        canonical_id(id).unwrap();
        let shell = sessions.get(id).unwrap();
        let target = Target::bind(scope, &state, &sessions, id).unwrap();
        let provider = target.provider_session.clone();
        let mut tracker = crate::activity::ActivityTracker::at(home.clone());
        let before = tracker
            .schedule_idle_token(&shell, &provider)
            .expect("complete the startup turn first");
        let identity = target.pane_identity.clone();
        let marker = format!("SCHEDULE_DISPATCH_SCOPE_{index}");
        let prompt = format!(
            "Reply with {marker} only. This is a disposable scheduled dispatch acceptance check. Do not use tools, inspect files or change anything. Then wait."
        );
        schedules
            .save(
                None,
                format!("Live scope {index}"),
                prompt,
                target,
                Timing::Once {
                    at: 1000 + index as u64,
                },
                999,
            )
            .unwrap();
        schedules.tick(1000 + index as u64).unwrap();
        let run = schedules
            .list()
            .unwrap()
            .into_iter()
            .find(|s| s.title == format!("Live scope {index}"))
            .unwrap()
            .last_run
            .unwrap();
        assert_eq!(run.outcome, Outcome::Submitted, "{}", run.message);
        let mut completed = false;
        for _ in 0..150 {
            thread::sleep(Duration::from_millis(200));
            if tracker
                .schedule_idle_token(&sessions.get(id).unwrap(), &provider)
                .is_some_and(|turn| turn != before)
            {
                completed = true;
                break;
            }
        }
        assert!(completed, "provider did not complete the scheduled turn");
        assert!(
            sessions
                .capture(id, 30)
                .unwrap()
                .lines()
                .any(|line| line.trim() == format!("• {marker}")),
            "provider did not return the expected marker"
        );
        assert_eq!(sessions.schedule_pane_identity(id).unwrap(), identity);
    }
    schedules.tick(1100).unwrap();
    assert!(
        schedules
            .list()
            .unwrap()
            .iter()
            .all(|s| s.next_run.is_none())
    );
}

#[test]
fn real_claude_worker_waits_for_completed_hook_before_dispatch() {
    let mut f = RealFixture::new();
    let project = f.state.projects[0].id.clone();
    let workspace = f.state.worktrees[0].clone();
    let session = f
        .sessions
        .create(
            project,
            Some(workspace.id.clone()),
            workspace.path,
            Some(format!(
                "/usr/bin/python3 '{}' --claude",
                f.f.home.join("harness.py").display()
            )),
        )
        .unwrap();
    f.ids.push(session.id.clone());
    let registry = f.f.home.join("sessions.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
    for entry in value["sessions"].as_array_mut().unwrap() {
        if entry["id"] == session.id {
            entry["harness"] = "claude".into();
        }
    }
    fs::write(registry, serde_json::to_vec(&value).unwrap()).unwrap();
    let provider = Uuid::new_v4().to_string();
    let turn = Uuid::new_v4().to_string();
    let hook = |event: &str| {
        serde_json::json!({"session_id":provider,"prompt_id":turn,"hook_event_name":event})
            .to_string()
    };
    crate::agent_hooks::record_claude_hook(&f.f.home, &session.id, &hook("UserPromptSubmit"))
        .unwrap();
    let target = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, &session.id).unwrap();
    f.f.store
        .save(
            None,
            "Claude fixture".into(),
            "literal Claude scheduling check".into(),
            target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap();
    f.wait_for_prompt(&session.id, "❯");
    f.f.store.tick(1000).unwrap();
    assert_eq!(f.f.row().last_run.unwrap().outcome, Outcome::Deferred);
    assert!(!f.f.home.join(format!("received-{}", session.id)).exists());
    crate::agent_hooks::record_claude_hook(&f.f.home, &session.id, &hook("Stop")).unwrap();
    f.f.store.tick(1015).unwrap();
    assert_eq!(f.f.row().last_run.unwrap().outcome, Outcome::Submitted);
    assert_eq!(
        fs::read_to_string(f.f.home.join(format!("received-{}", session.id))).unwrap(),
        "literal Claude scheduling check\n"
    );
}

#[test]
fn delete_waits_for_claimed_delivery_and_cannot_overwrite_its_outcome() {
    let f = Fixture::new();
    let schedule = f.add(Timing::Once { at: 1000 });
    let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let store = f.store.clone();
    let dispatch = thread::spawn(move || {
        store
            .tick_with(1000, |_, _, claim| {
                assert!(claim("turn")?);
                claimed_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
                Ok(Delivery::Submitted)
            })
            .unwrap()
    });
    claimed_rx.recv().unwrap();
    let store = f.store.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let deletion = thread::spawn(move || {
        started_tx.send(()).unwrap();
        done_tx
            .send(store.delete(&schedule.id, schedule.revision))
            .unwrap();
    });
    started_rx.recv().unwrap();
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_millis(50)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    resume_tx.send(()).unwrap();
    dispatch.join().unwrap();
    assert!(done_rx.recv().unwrap().is_err());
    deletion.join().unwrap();
    let row = f.row();
    assert_eq!(row.last_run.unwrap().outcome, Outcome::Submitted);
    f.store.delete(&row.id, row.revision).unwrap();
    f.store
        .tick_with(1001, |_, _, _| panic!("deleted schedule ran"))
        .unwrap();
}
