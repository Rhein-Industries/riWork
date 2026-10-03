use super::*;
use std::{
    collections::HashMap,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

fn fixture_pid(pids: &Mutex<HashMap<String, u32>>, id: &str) -> u32 {
    let mut pids = pids.lock().unwrap();
    let next = 40_000 + pids.len() as u32;
    *pids.entry(id.to_owned()).or_insert(next)
}

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
fn shared_service_requires_future_edit_before_failed_or_uncertain_rearm() {
    use crate::schedule_service::{
        RepeatChange, ScheduleKey, ScheduleService, ScopeInput, UpdateRequest,
    };

    for uncertain in [false, true] {
        let f = Fixture::new();
        f.add(Timing::Interval {
            first: 1000,
            seconds: 300,
        });
        f.store
            .tick_with(1000, |_, _, claim| {
                if uncertain {
                    assert!(claim("fixture-turn")?);
                    Ok(Delivery::Uncertain("fixture input may have arrived".into()))
                } else {
                    Ok(Delivery::Failed("fixture target failed".into()))
                }
            })
            .unwrap();
        let failed = f.row();
        let key = ScheduleKey {
            id: failed.id.clone(),
            revision: failed.revision,
            scope: ScopeInput {
                scope: "app".into(),
                project_id: None,
                worktree_id: None,
            },
            shell_id: failed.target.shell_id.clone(),
        };
        let service = ScheduleService::at(f.home.clone()).unwrap();
        let error = service.pause(&key, false).unwrap_err();
        assert_eq!(error.code, "review_required");
        let at = (chrono::Utc::now() + chrono::Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let rearmed = service
            .update(UpdateRequest {
                key,
                title: None,
                prompt: None,
                at,
                repeat: RepeatChange::Keep,
            })
            .unwrap();
        assert_eq!(rearmed.target, failed.target);
        assert!(!rearmed.paused && !rearmed.review_required);
        assert_eq!(
            rearmed.timing,
            Timing::Interval {
                first: rearmed.timing.first(),
                seconds: 300
            }
        );
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
/// Stops the fixture's whole tmux server when dropped, even when setup panics
/// before the fixture exists. Closing sessions one by one left servers running.
struct ServerGuard(SessionManager);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.0.kill_server();
    }
}
struct RealFixture {
    _server: ServerGuard,
    f: Fixture,
    sessions: SessionManager,
    state: State,
    ids: Vec<String>,
    foreground: Arc<Mutex<String>>,
    pids: Arc<Mutex<HashMap<String, u32>>>,
    _probe: crate::sessions::schedule_probe::Installed,
}
/// Fixture `lsof`: prints the canned descriptor listing for the queried PID.
const FAKE_LSOF: &str = "#!/bin/sh\ncat \"$(dirname \"$0\")/lsof-$4\"\n";
fn fake_lsof(home: &Path, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = home.join("fake-lsof");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
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
        fs::write(&script,r#"import os,sys,tty,select
from pathlib import Path
tty.setraw(0)
prompt='❯' if '--claude' in sys.argv else '›'
os.write(1,('\x1b[2J\x1b[H'+prompt+' ').encode())
buf=b''
home=Path(os.environ['RIWORK_HOME'])
screen_path=home/('screen-'+os.environ['RIWORK_SHELL_ID'])
last_screen=None
while True:
 if screen_path.exists():
  screen=screen_path.read_bytes()
  if screen!=last_screen:
   os.write(1,screen)
   last_screen=screen
   (home/('painted-'+os.environ['RIWORK_SHELL_ID'])).write_bytes(screen)
 if not select.select([0],[],[],0.02)[0]: continue
 b=os.read(0,1)
 if b in (b'\r',b'\n'):
  with open(Path(os.environ['RIWORK_HOME'])/('received-'+os.environ['RIWORK_SHELL_ID']), 'ab') as f: f.write(buf+b'\n')
  buf=b''
  os.write(1,('\x1b[2J\x1b[H'+prompt+' ').encode())
 else: buf+=b
"#).unwrap();
        let sessions = SessionManager::at(f.home.clone()).unwrap();
        let server = ServerGuard(SessionManager::at(f.home.clone()).unwrap());
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
        // The python fixtures are not processes named `codex`. Report the
        // foreground command a directly launched harness would have and let the
        // fixture `lsof` show its rollout, so the real proof code runs.
        let foreground = Arc::new(Mutex::new(String::from("codex")));
        let pids = Arc::new(Mutex::new(HashMap::new()));
        let probe = crate::sessions::schedule_probe::install(
            &f.home,
            {
                let (foreground, pids) = (foreground.clone(), pids.clone());
                Arc::new(move |id: &str| {
                    Ok(format!(
                        "{}|{}",
                        fixture_pid(&pids, id),
                        foreground.lock().unwrap()
                    ))
                })
            },
            fake_lsof(&f.home, FAKE_LSOF),
        );
        let result = Self {
            _server: server,
            f,
            sessions,
            state,
            ids,
            foreground,
            pids,
            _probe: probe,
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
        let rollout = logs.join(format!("rollout-fixture-{thread}.jsonl"));
        self.set_open_files(id, &[&rollout]);
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
        fs::write(rollout, records).unwrap();
    }
    /// What the fixture `lsof` reports the pane's process holds open.
    fn set_open_files(&self, id: &str, paths: &[&Path]) {
        let pid = fixture_pid(&self.pids, id);
        let mut listing = format!("p{pid}\nfcwd\nn/\nf3\nn/dev/null\n");
        for path in paths {
            listing += &format!("f9\nn{}\n", path.display());
        }
        fs::write(self.f.home.join(format!("lsof-{pid}")), listing).unwrap();
    }
    fn set_foreground(&self, command: &str) {
        *self.foreground.lock().unwrap() = command.into();
    }
    fn paint(&self, id: &str, screen: &str, row: usize, column: usize) {
        let painted = screen.replace('\n', "\r\n");
        let bytes = format!("\x1b[2J\x1b[H{painted}\x1b[{};{}H", row + 1, column + 1);
        fs::write(self.f.home.join(format!("screen-{id}")), &bytes).unwrap();
        for _ in 0..150 {
            if fs::read(self.f.home.join(format!("painted-{id}")))
                .is_ok_and(|b| b == bytes.as_bytes())
                && self
                    .sessions
                    .capture(id, 100)
                    .is_ok_and(|s| s.contains(screen.lines().next().unwrap()))
            {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("Disposable terminal did not paint its fixture screen");
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
fn a_real_fixture_leaves_no_tmux_server_behind() {
    let f = RealFixture::new();
    let probe = SessionManager::at(f.f.home.clone()).unwrap();
    assert!(probe.server_running());
    // The way servers leaked: closing the sessions one by one fails (here the
    // registry is gone; a refused close or a panic halfway through setup does the
    // same), and nothing else stops the server.
    fs::remove_file(f.f.home.join("sessions.json")).unwrap();
    drop(f);
    let left = probe.server_running();
    probe.kill_server();
    assert!(!left, "the fixture's tmux server is still running");
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

#[test]
fn real_dispatch_accepts_completed_history_and_refuses_current_interaction() {
    let history = "• Thinking about login: approve the login fix.\n  Sign in to continue was the old error.\n  Approval required and esc to interrupt were quoted UI text.\n\n";
    let cases = [
        (format!("{history}› \n? for shortcuts"), 4, 2, false, true),
        (
            format!(
                "{history}\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n? for shortcuts"
            ),
            4,
            2,
            false,
            true,
        ),
        (
            "• Working (1s • esc to interrupt)\n\n› \n? for shortcuts".into(),
            2,
            2,
            true,
            false,
        ),
        (
            "Busy UI transition\n› \nEsc to interrupt".into(),
            1,
            2,
            false,
            false,
        ),
        (
            "Do you trust this directory?\n❯ 1. Yes, proceed\n  2. No".into(),
            1,
            2,
            false,
            false,
        ),
        (
            "Sign in to continue\nEmail: \nPress Enter".into(),
            1,
            7,
            false,
            false,
        ),
        (
            "Approval required\n❯ 1. Allow once\n  2. Reject".into(),
            1,
            2,
            false,
            false,
        ),
        (
            "Existing draft\n› existing draft\n? for shortcuts".into(),
            1,
            16,
            false,
            false,
        ),
        (
            "Typed placeholder draft\n› Ask Codex to do anything\n? for shortcuts".into(),
            1,
            2,
            false,
            false,
        ),
    ];
    for (screen, row, column, busy, accepts) in cases {
        let f = RealFixture::new();
        let id = &f.ids[2];
        if busy {
            f.lifecycle(id, "busy-turn", false);
        }
        let target = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, id).unwrap();
        let identity = target.pane_identity.clone();
        f.paint(id, &screen, row, column);
        f.f.store
            .save(
                None,
                "Current surface".into(),
                "literal fixture acceptance".into(),
                target,
                Timing::Once { at: 1000 },
                999,
            )
            .unwrap();
        let mut trackers = Default::default();
        f.f.store.tick_tracked(1000, &mut trackers).unwrap();
        let run = f.f.row().last_run.unwrap();
        assert_eq!(
            run.outcome,
            if accepts {
                Outcome::Submitted
            } else {
                Outcome::Deferred
            },
            "{screen}: {}; pane: {}",
            run.message,
            f.sessions.capture(id, 100).unwrap()
        );
        let received = f.f.home.join(format!("received-{id}"));
        if accepts {
            for _ in 0..150 {
                if received.exists() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(
                fs::read_to_string(&received).unwrap(),
                "literal fixture acceptance\n"
            );
            f.f.store.tick_tracked(1015, &mut trackers).unwrap();
            assert_eq!(
                fs::read_to_string(&received).unwrap(),
                "literal fixture acceptance\n"
            );
        } else {
            assert!(!received.exists(), "blocked terminal received test input");
        }
        assert_eq!(f.sessions.schedule_pane_identity(id).unwrap(), identity);
    }
}

#[test]
fn tracked_scheduler_catches_up_large_rollout_without_startup_alerts_or_duplicate() {
    let f = RealFixture::new();
    let id = &f.ids[2];
    let target = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, id).unwrap();
    let identity = target.pane_identity.clone();
    let provider = target.provider_session.clone();
    let path =
        f.f.home
            .join("codex/sessions/2026/09/27")
            .join(format!("rollout-fixture-{provider}.jsonl"));
    let mut log = OpenOptions::new().append(true).open(&path).unwrap();
    write!(
        log,
        "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"content\":\"{}\"}}}}\n",
        "x".repeat(crate::activity::MAX_POLL_BYTES * 2 + 4096)
    )
    .unwrap();
    for event in ["task_started", "task_complete"] {
        writeln!(log, "{}", serde_json::json!({"type":"event_msg","payload":{"type":event,"turn_id":"latest-history-turn"}})).unwrap();
    }
    drop(log);
    assert!(fs::metadata(&path).unwrap().len() > crate::activity::MAX_POLL_BYTES as u64);
    f.f.store
        .save(
            None,
            "Large history".into(),
            "large rollout fixture dispatch".into(),
            target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap();
    let mut trackers = std::collections::HashMap::new();
    let received = f.f.home.join(format!("received-{id}"));
    // A rollout past the poll limit is opened from its head and tail, so its
    // idle state is known on the first tick instead of after 8 MiB catch-up
    // polls; the historical completion is a baseline, not an alert.
    f.f.store.tick_tracked(1000, &mut trackers).unwrap();
    assert_eq!(f.f.row().last_run.unwrap().outcome, Outcome::Submitted);
    assert!(trackers.get_mut(id).unwrap().take_completions().is_empty());
    assert!(!f.f.home.join("agent-notifications.json").exists());
    assert!(
        crate::notifications::claim_pending(&f.f.home)
            .unwrap()
            .is_empty()
    );
    for _ in 0..150 {
        if received.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read_to_string(&received).unwrap(),
        "large rollout fixture dispatch\n"
    );
    for at in [1045, 1100, 1500] {
        f.f.store.tick_tracked(at, &mut trackers).unwrap();
    }
    assert_eq!(
        fs::read_to_string(received).unwrap(),
        "large rollout fixture dispatch\n"
    );
    assert_eq!(f.sessions.schedule_pane_identity(id).unwrap(), identity);
    assert_eq!(f.f.row().target.provider_session, provider);
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

impl RealFixture {
    /// A disposable worker registered as Claude, with the fixture's `❯` composer.
    fn add_claude_worker(&mut self) -> ShellSession {
        let project = self.state.projects[0].id.clone();
        let workspace = self.state.worktrees[0].clone();
        let session = self
            .sessions
            .create(
                project,
                Some(workspace.id.clone()),
                workspace.path,
                Some(format!(
                    "/usr/bin/python3 '{}' --claude",
                    self.f.home.join("harness.py").display()
                )),
            )
            .unwrap();
        self.ids.push(session.id.clone());
        let registry = self.f.home.join("sessions.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
        for entry in value["sessions"].as_array_mut().unwrap() {
            if entry["id"] == session.id {
                entry["harness"] = "claude".into();
            }
        }
        fs::write(registry, serde_json::to_vec(&value).unwrap()).unwrap();
        session
    }
    fn claude_hook(&self, id: &str, payload: serde_json::Value) {
        crate::agent_hooks::record_claude_hook(&self.f.home, id, &payload.to_string()).unwrap();
    }
}
fn claude_event(event: &str, session: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({"session_id":session,"prompt_id":prompt,"hook_event_name":event})
}
#[test]
fn real_claude_worker_waits_for_completed_hook_before_dispatch() {
    let mut f = RealFixture::new();
    let session = f.add_claude_worker();
    let provider = Uuid::new_v4().to_string();
    let turn = Uuid::new_v4().to_string();
    let hook = |event: &str| claude_event(event, &provider, &turn);
    f.claude_hook(&session.id, hook("UserPromptSubmit"));
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
    f.claude_hook(&session.id, hook("Stop"));
    f.f.store.tick(1015).unwrap();
    assert_eq!(f.f.row().last_run.unwrap().outcome, Outcome::Submitted);
    assert_eq!(
        fs::read_to_string(f.f.home.join(format!("received-{}", session.id))).unwrap(),
        "literal Claude scheduling check\n"
    );
}

#[test]
fn claude_shell_foreground_and_stale_completion_defer_until_a_real_stop() {
    let mut f = RealFixture::new();
    let session = f.add_claude_worker();
    let (provider, turn) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    f.claude_hook(
        &session.id,
        claude_event("UserPromptSubmit", &provider, &turn),
    );
    f.claude_hook(&session.id, claude_event("Stop", &provider, &turn));
    let target = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, &session.id).unwrap();
    f.f.store
        .save(
            None,
            "Claude gate".into(),
            "literal Claude gate check".into(),
            target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap();
    f.wait_for_prompt(&session.id, "❯");
    let received = f.f.home.join(format!("received-{}", session.id));
    let deferred = |at: u64, why: &str| {
        f.f.store.tick(at).unwrap();
        let row = f.f.row();
        let run = row.last_run.unwrap();
        assert_eq!(run.outcome, Outcome::Deferred, "{why}: {}", run.message);
        assert!(!row.paused && !row.review_required, "{why}");
        assert!(!received.exists(), "{why}: input was delivered");
    };
    // Claude exited: the same pane, pid and hook cursor, but a shell draws `❯`.
    f.set_foreground("-zsh");
    deferred(1000, "shell in the foreground");
    // A subagent finished in a turn whose UserPromptSubmit was missed (its stop
    // names a prompt the cursor never saw begin), so the main agent is mid-turn
    // and the earlier completion is withdrawn. This needs no SubagentStart, so it
    // also holds for a Claude launched before that hook was registered.
    f.set_foreground("2.1.284");
    f.claude_hook(
        &session.id,
        serde_json::json!({"session_id":provider,"prompt_id":"missed-turn",
            "hook_event_name":"SubagentStop","agent_id":"subagent-1",
            "agent_transcript_path":"/subagent/transcript"}),
    );
    deferred(1015, "subagent activity");
    // The next turn that the hooks do see ends normally.
    f.claude_hook(
        &session.id,
        claude_event("UserPromptSubmit", &provider, &turn),
    );
    deferred(1020, "a turn is open");
    f.claude_hook(&session.id, claude_event("Stop", &provider, &turn));
    f.f.store.tick(1030).unwrap();
    assert_eq!(f.f.row().last_run.unwrap().outcome, Outcome::Submitted);
    assert_eq!(
        fs::read_to_string(&received).unwrap(),
        "literal Claude gate check\n"
    );
}

#[test]
fn a_stray_subagent_stop_after_a_reply_does_not_stop_a_schedule() {
    let mut f = RealFixture::new();
    let session = f.add_claude_worker();
    let (provider, turn) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    // A reply, and the SubagentStop Claude 2.1.288 sends 3 to 5 seconds after some of
    // them (a tool-using one included): no SubagentStart came before it, its
    // `agent_type` is empty and it carries the finished turn's own prompt. It says
    // nothing about a running turn.
    f.claude_hook(
        &session.id,
        claude_event("UserPromptSubmit", &provider, &turn),
    );
    f.claude_hook(&session.id, claude_event("Stop", &provider, &turn));
    f.claude_hook(
        &session.id,
        serde_json::json!({"session_id":provider,"prompt_id":turn,"hook_event_name":"SubagentStop",
            "agent_id":"prompt-suggestion","agent_type":"",
            "agent_transcript_path":"/subagent/transcript"}),
    );
    let target = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, &session.id).unwrap();
    f.f.store
        .save(
            None,
            "Claude stray stop".into(),
            "sent after a stray subagent stop".into(),
            target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap();
    f.wait_for_prompt(&session.id, "❯");
    let received = f.f.home.join(format!("received-{}", session.id));
    f.f.store.tick(1000).unwrap();
    let run = f.f.row().last_run.unwrap();
    assert_eq!(run.outcome, Outcome::Submitted, "{}", run.message);
    assert_eq!(
        fs::read_to_string(&received).unwrap(),
        "sent after a stray subagent stop\n"
    );
}

#[test]
fn a_real_subagent_start_and_stop_pair_in_an_unseen_turn_withdraws_the_completion() {
    let mut f = RealFixture::new();
    let session = f.add_claude_worker();
    let (provider, turn) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    f.claude_hook(
        &session.id,
        claude_event("UserPromptSubmit", &provider, &turn),
    );
    f.claude_hook(&session.id, claude_event("Stop", &provider, &turn));
    let target = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, &session.id).unwrap();
    f.f.store
        .save(
            None,
            "Claude real pair".into(),
            "never sent while a subagent turn is open".into(),
            target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap();
    f.wait_for_prompt(&session.id, "❯");
    let received = f.f.home.join(format!("received-{}", session.id));
    let subagent = |event: &str| {
        serde_json::json!({"session_id":provider,"prompt_id":"missed-turn",
            "hook_event_name":event,"agent_id":"agent-1","agent_type":"general-purpose"})
    };
    // The start of a subagent in a turn the hooks never announced reopens the
    // turn at once, and its stop leaves it open: only a Stop completes a turn.
    f.claude_hook(&session.id, subagent("SubagentStart"));
    f.f.store.tick(1000).unwrap();
    let run = f.f.row().last_run.unwrap();
    assert_eq!(run.outcome, Outcome::Deferred, "{}", run.message);
    assert!(
        !received.exists(),
        "input was delivered into a running turn"
    );
    f.claude_hook(&session.id, subagent("SubagentStop"));
    f.f.store.tick(1015).unwrap();
    let run = f.f.row().last_run.unwrap();
    assert_eq!(run.outcome, Outcome::Deferred, "{}", run.message);
    assert!(
        !received.exists(),
        "input was delivered into a running turn"
    );
    // The turn after it, announced normally, completes and admits the prompt.
    f.claude_hook(
        &session.id,
        claude_event("UserPromptSubmit", &provider, &turn),
    );
    f.claude_hook(&session.id, claude_event("Stop", &provider, &turn));
    f.f.store.tick(1030).unwrap();
    assert_eq!(f.f.row().last_run.unwrap().outcome, Outcome::Submitted);
}

#[test]
fn claude_clear_and_resume_rebind_so_the_old_completion_cannot_admit_a_prompt() {
    let mut f = RealFixture::new();
    let session = f.add_claude_worker();
    let (provider, turn) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    f.claude_hook(
        &session.id,
        claude_event("UserPromptSubmit", &provider, &turn),
    );
    f.claude_hook(&session.id, claude_event("Stop", &provider, &turn));
    let target = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, &session.id).unwrap();
    assert_eq!(target.provider_session, provider);
    f.f.store
        .save(
            None,
            "Claude cleared".into(),
            "must not reach the new conversation".into(),
            target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap();
    f.wait_for_prompt(&session.id, "❯");
    // `/clear` starts a new conversation; its SessionStart names the new id.
    let replacement = Uuid::new_v4().to_string();
    f.claude_hook(
        &session.id,
        serde_json::json!({"session_id":replacement,"hook_event_name":"SessionStart","source":"clear"}),
    );
    f.f.store.tick(1000).unwrap();
    let row = f.f.row();
    let run = row.last_run.unwrap();
    assert_eq!(run.outcome, Outcome::Failed, "{}", run.message);
    assert!(row.paused && row.review_required);
    assert!(!f.f.home.join(format!("received-{}", session.id)).exists());
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

// Provider identity proof. Codex identity needs the pane's own process to hold
// its rollout open; a stale binding alone proves nothing.
fn rollout_of(f: &RealFixture, provider: &str) -> PathBuf {
    f.f.home
        .join("codex/sessions/2026/09/27")
        .join(format!("rollout-fixture-{provider}.jsonl"))
}
fn received_prompt(f: &RealFixture, id: &str) -> PathBuf {
    f.f.home.join(format!("received-{id}"))
}

#[test]
fn codex_that_left_a_shell_prompt_is_deferred_and_never_typed_into() {
    let f = RealFixture::new();
    let id = &f.ids[2];
    let target = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, id).unwrap();
    let rollout = rollout_of(&f, &target.provider_session);
    f.f.store
        .save(
            None,
            "Exited".into(),
            "would run as a shell command".into(),
            target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap();
    let deferred = |at: u64, why: &str| {
        f.f.store.tick(at).unwrap();
        let row = f.f.row();
        let run = row.last_run.unwrap();
        assert_eq!(run.outcome, Outcome::Deferred, "{why}: {}", run.message);
        assert!(!row.paused && !row.review_required, "{why}");
        assert_eq!(row.next_run, Some(1000), "{why}: the occurrence stays due");
        assert!(!received_prompt(&f, id).exists(), "{why}: input delivered");
        run.message
    };
    // Codex exited: pane, pid and start command are unchanged, the rollout
    // still reads Done and the fixture composer sits at the `›` column.
    f.set_foreground("zsh");
    let message = deferred(1000, "shell in the foreground");
    assert!(message.contains("unproven"), "{message}");
    f.set_foreground("python3");
    deferred(1015, "another program");
    // The npm launcher is named node; it earns no trust without the proof.
    f.set_foreground("node");
    f.set_open_files(id, &[]);
    let message = deferred(1030, "node without a rollout");
    assert!(
        message.contains("no unique open primary rollout"),
        "{message}"
    );
    f.set_foreground("codex");
    deferred(1045, "codex without a rollout");
    f.set_foreground("node");
    f.set_open_files(id, &[&rollout]);
    f.f.store.tick(1060).unwrap();
    let run = f.f.row().last_run.unwrap();
    assert_eq!(run.outcome, Outcome::Submitted, "{}", run.message);
    for _ in 0..150 {
        if received_prompt(&f, id).exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read_to_string(received_prompt(&f, id)).unwrap(),
        "would run as a shell command\n"
    );
}

#[test]
fn unreadable_evidence_defers_without_pausing_and_recovers() {
    let f = RealFixture::new();
    let id = &f.ids[2];
    let target = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, id).unwrap();
    f.f.store
        .save(
            None,
            "Flaky".into(),
            "sent after the evidence recovers".into(),
            target,
            Timing::Interval {
                first: 1000,
                seconds: 3600,
            },
            999,
        )
        .unwrap();
    let deferred = |at: u64, why: &str| {
        f.f.store.tick(at).unwrap();
        let row = f.f.row();
        let run = row.last_run.unwrap();
        assert_eq!(run.outcome, Outcome::Deferred, "{why}: {}", run.message);
        assert!(
            !row.paused && !row.review_required,
            "{why}: {}",
            run.message
        );
        assert!(!received_prompt(&f, id).exists(), "{why}: input delivered");
        assert_eq!(row.check_after, at + 15, "{why}");
        run.message
    };
    // lsof hangs past its 2 s bound, then fails outright.
    fake_lsof(&f.f.home, "#!/bin/sh\nexec sleep 30\n");
    let started = std::time::Instant::now();
    let message = deferred(1000, "slow lsof");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(message.contains("timed out"), "{message}");
    fake_lsof(&f.f.home, "#!/bin/sh\nexit 1\n");
    deferred(1015, "failing lsof");
    fake_lsof(&f.f.home, FAKE_LSOF);
    // Unreadable session registry, then unreadable workspace state.
    let registry = f.f.home.join("sessions.json");
    let saved = fs::read(&registry).unwrap();
    fs::write(&registry, "not json").unwrap();
    deferred(1030, "unreadable registry");
    fs::write(&registry, saved).unwrap();
    let workspace = f.f.home.join("state.json");
    let saved = fs::read(&workspace).unwrap();
    fs::write(&workspace, "not json").unwrap();
    let message = deferred(1045, "unreadable workspace state");
    assert!(message.contains("Delivery check failed"), "{message}");
    fs::write(&workspace, saved).unwrap();
    f.f.store.tick(1060).unwrap();
    let row = f.f.row();
    assert_eq!(
        row.last_run.unwrap().outcome,
        Outcome::Submitted,
        "recovered evidence must dispatch"
    );
    assert!(!row.paused);
}

#[test]
fn a_target_that_is_truly_gone_or_replaced_still_fails_and_pauses() {
    let f = RealFixture::new();
    let id = &f.ids[2];
    let target = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, id).unwrap();
    f.f.store
        .save(
            None,
            "Gone".into(),
            "no such session".into(),
            target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap();
    f.sessions.close(id).unwrap();
    f.f.store.tick(1000).unwrap();
    let row = f.f.row();
    let run = row.last_run.unwrap();
    assert_eq!(run.outcome, Outcome::Failed, "{}", run.message);
    assert!(row.paused && row.review_required);
}

/// Stand-in `lsof` output for direct proof checks, one process per fixture.
struct ProofFixture {
    home: PathBuf,
    sessions: SessionManager,
    shell: ShellSession,
    thread: String,
    rollout: PathBuf,
    command: Arc<Mutex<String>>,
    pid: Arc<Mutex<u32>>,
    _probe: crate::sessions::schedule_probe::Installed,
}
impl ProofFixture {
    fn new(bind: bool) -> Self {
        let home = std::env::temp_dir().join(format!("riwork-proof-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let sessions = SessionManager::at(home.clone()).unwrap();
        let (id, thread) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
        let log_home = home.join("codex");
        let logs = log_home.join("sessions/2026/09/27");
        fs::create_dir_all(&logs).unwrap();
        let rollout = logs.join(format!("rollout-fixture-{thread}.jsonl"));
        fs::write(
            &rollout,
            format!(
                "{}\n{}\n{}\n",
                serde_json::json!({"type":"session_meta","payload":{"id":thread,"source":"cli"}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t"}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t"}})
            ),
        )
        .unwrap();
        if bind {
            crate::activity::bind_codex_thread(&home, &id, &thread, &log_home).unwrap();
        }
        let shell: ShellSession = serde_json::from_value(serde_json::json!({
            "id":id,"project_id":null,"worktree_id":null,"kind":"orchestrator","cwd":home,
            "command":null,"harness":"codex","codex_home":log_home,"created_at_unix":1
        }))
        .unwrap();
        let command = Arc::new(Mutex::new(String::from("codex")));
        let pid = Arc::new(Mutex::new(4242));
        let probe = crate::sessions::schedule_probe::install(
            &home,
            {
                let (command, pid) = (command.clone(), pid.clone());
                Arc::new(move |_: &str| {
                    Ok(format!(
                        "{}|{}",
                        pid.lock().unwrap(),
                        command.lock().unwrap()
                    ))
                })
            },
            fake_lsof(&home, ""),
        );
        let fixture = Self {
            home,
            sessions,
            shell,
            thread,
            rollout,
            command,
            pid,
            _probe: probe,
        };
        fixture.lsof(&format!(
            "printf 'p4242\\nf9\\nn%s\\n' '{}'",
            fixture.rollout.display()
        ));
        fixture
    }
    fn lsof(&self, body: &str) {
        fake_lsof(&self.home, &format!("#!/bin/sh\n{body}\n"));
    }
    fn foreground(&self, command: &str) {
        *self.command.lock().unwrap() = command.into();
    }
    fn proof(&self) -> Result<String, crate::sessions::IdentityError> {
        self.sessions.schedule_provider_proof(&self.shell)
    }
}
impl Drop for ProofFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}
macro_rules! assert_unproven {
    ($proof:expr) => {
        assert!(
            matches!($proof, Err(crate::sessions::IdentityError::Unproven(_))),
            "expected an unproven identity"
        )
    };
}
macro_rules! assert_changed {
    ($proof:expr) => {
        assert!(
            matches!($proof, Err(crate::sessions::IdentityError::Changed(_))),
            "expected a changed identity"
        )
    };
}

#[test]
fn codex_proof_needs_the_foreground_process_to_hold_the_bound_rollout() {
    let f = ProofFixture::new(true);
    assert_eq!(f.proof().unwrap(), f.thread);
    f.foreground("node");
    assert_eq!(f.proof().unwrap(), f.thread);
    for command in ["zsh", "-bash", "python3", "riwork"] {
        f.foreground(command);
        assert_unproven!(f.proof());
        assert!(f.sessions.schedule_provider_identity(&f.shell).is_err());
    }
    // Neither name is trusted on its own.
    f.foreground("node");
    f.lsof("printf 'p4242\\nf3\\nn/dev/null\\n'");
    assert_unproven!(f.proof());
    f.foreground("codex");
    assert_unproven!(f.proof());
}

#[test]
fn codex_proof_distinguishes_a_different_thread_from_unreadable_evidence() {
    let f = ProofFixture::new(true);
    let other = f
        .rollout
        .with_file_name(format!("rollout-fixture-{}.jsonl", Uuid::new_v4()));
    // The pane's Codex now holds another thread, or a rollout of another account.
    f.lsof(&format!("printf 'n%s\\n' '{}'", other.display()));
    assert_changed!(f.proof());
    let foreign = f
        .home
        .join("elsewhere")
        .join(f.rollout.file_name().unwrap());
    f.lsof(&format!("printf 'n%s\\n' '{}'", foreign.display()));
    assert_changed!(f.proof());
    // Two open rollouts, a failing lsof and a hung lsof are only unproven.
    f.lsof(&format!(
        "printf 'n%s\\nn%s\\n' '{}' '{}'",
        f.rollout.display(),
        other.display()
    ));
    assert_unproven!(f.proof());
    f.lsof("exit 3");
    assert_unproven!(f.proof());
    f.lsof("exec sleep 30");
    let started = std::time::Instant::now();
    assert_unproven!(f.proof());
    assert!(started.elapsed() < Duration::from_secs(10));
    // A missing lsof binary cannot prove anything either.
    fs::remove_file(f.home.join("fake-lsof")).unwrap();
    assert_unproven!(f.proof());
}

#[test]
fn real_lsof_sees_a_rollout_held_by_a_launcher_child_of_the_pane_group() {
    use std::os::unix::process::CommandExt;
    let f = ProofFixture::new(true);
    f.lsof("exec /usr/sbin/lsof \"$@\"");
    f.foreground("node");
    // Like the npm wrapper: the group leader holds nothing, its child holds the rollout.
    let mut launcher = Command::new("/bin/sh")
        .arg("-c")
        .arg("(exec 9<\"$0\"; exec sleep 30) & wait")
        .arg(&f.rollout)
        .process_group(0)
        .spawn()
        .unwrap();
    *f.pid.lock().unwrap() = launcher.id();
    let mut proof = f.proof();
    for _ in 0..100 {
        if proof.is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(30));
        proof = f.proof();
    }
    assert_eq!(proof.unwrap(), f.thread);
    // Nothing outside that group counts: an unrelated pid proves nothing.
    *f.pid.lock().unwrap() = std::process::id();
    assert_unproven!(f.proof());
    // SAFETY: the group was created by this test and holds only its own processes.
    unsafe { libc::killpg(launcher.id() as libc::pid_t, libc::SIGKILL) };
    let _ = launcher.wait();
}

#[test]
fn codex_proof_drains_large_descriptor_listings() {
    let f = ProofFixture::new(true);
    // Far more than a 64 KiB pipe: an undrained pipe would stall to the timeout.
    f.lsof(&format!(
        "awk 'BEGIN{{for(i=0;i<8000;i++)print \"n/private/var/tmp/some/long/open/file/path/\" i}}'\nprintf 'n%s\\n' '{}'",
        f.rollout.display()
    ));
    let started = std::time::Instant::now();
    assert_eq!(f.proof().unwrap(), f.thread);
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "listing stalled for {:?}",
        started.elapsed()
    );
    // Past the cap the rollout list may be incomplete, so it proves nothing.
    f.lsof(&format!(
        "awk 'BEGIN{{for(i=0;i<120000;i++)print \"n/private/var/tmp/some/long/open/file/path/\" i}}'\nprintf 'n%s\\n' '{}'",
        f.rollout.display()
    ));
    assert_unproven!(f.proof());
}

#[test]
fn codex_without_a_binding_binds_only_a_proven_rollout() {
    let f = ProofFixture::new(false);
    assert_eq!(f.proof().unwrap(), f.thread);
    // The binding written by the proof is now stale evidence at best.
    f.foreground("zsh");
    assert_unproven!(f.proof());
    let unbound = ProofFixture::new(false);
    unbound.foreground("zsh");
    assert_unproven!(unbound.proof());
}

#[test]
fn claude_identity_comes_from_the_hook_cursor_alone() {
    let f = ProofFixture::new(false);
    let mut shell = f.shell.clone();
    shell.harness = Some(HarnessKind::Claude);
    assert_unproven!(f.sessions.schedule_provider_proof(&shell));
    let hooks = f.home.join("agent-hooks/claude");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(
        hooks.join(format!("{}.json", shell.id)),
        r#"{"session_id":"claude-session","turn_id":"turn","completed":true}"#,
    )
    .unwrap();
    assert_eq!(
        f.sessions.schedule_provider_proof(&shell).unwrap(),
        "claude-session"
    );
    fs::write(hooks.join(format!("{}.json", shell.id)), "not json").unwrap();
    assert_unproven!(f.sessions.schedule_provider_proof(&shell));
}

// Dispatch errors are recorded and backed off, never left to wedge the queue.
#[test]
fn dispatch_errors_are_deferred_with_backoff_so_other_schedules_still_run() {
    let f = Fixture::new();
    let first = f.add(Timing::Once { at: 1000 });
    let second = f.add(Timing::Once { at: 1001 });
    f.store
        .tick_with(1002, |target, _, _| {
            assert_eq!(target.shell_id, first.target.shell_id);
            Err("tmux: server exited unexpectedly".into())
        })
        .unwrap();
    let row = f
        .store
        .list()
        .unwrap()
        .into_iter()
        .find(|s| s.id == first.id)
        .unwrap();
    let run = row.last_run.unwrap();
    assert_eq!(run.outcome, Outcome::Deferred);
    assert!(
        run.message.contains("tmux: server exited unexpectedly"),
        "{}",
        run.message
    );
    assert_eq!((row.check_after, row.next_run), (1017, Some(1000)));
    assert!(!row.paused && !row.review_required);
    // The same second no longer selects the failing schedule.
    f.store
        .tick_with(1002, |target, _, claim| {
            assert_eq!(target.shell_id, second.target.shell_id);
            assert!(claim("turn-2")?);
            Ok(Delivery::Submitted)
        })
        .unwrap();
    f.store
        .tick_with(1016, |_, _, _| panic!("backoff was ignored"))
        .unwrap();
    // It is retried after the backoff and can still succeed.
    f.store.tick_with(1017, submit).unwrap();
    let row = f
        .store
        .list()
        .unwrap()
        .into_iter()
        .find(|s| s.id == first.id)
        .unwrap();
    assert_eq!(row.last_run.unwrap().outcome, Outcome::Submitted);
}

#[test]
fn a_missed_window_keeps_the_last_deferral_reason() {
    let f = Fixture::new();
    f.add(Timing::Once { at: 1000 });
    f.store
        .tick_with(1000, |_, _, _| Err("Cannot spawn\u{1b}[31m tmux".into()))
        .unwrap();
    f.store
        .tick_with(1301, |_, _, _| panic!("too late"))
        .unwrap();
    let run = f.row().last_run.unwrap();
    assert_eq!(run.outcome, Outcome::Missed);
    assert!(run.message.contains("Last check:"), "{}", run.message);
    assert!(run.message.contains("Cannot spawn"), "{}", run.message);
    assert!(
        !run.message.chars().any(char::is_control),
        "{}",
        run.message
    );
}

#[test]
fn a_failed_claim_write_is_still_an_error_and_never_a_recorded_deferral() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let f = Fixture::new();
    let schedule = f.add(Timing::Once { at: 1000 });
    let _restore = Restore(f.home.clone());
    // The lock file exists; only creating the ledger's temporary file fails.
    fs::set_permissions(&f.home, fs::Permissions::from_mode(0o500)).unwrap();
    let result = f.store.tick_with(1000, |_, _, claim| {
        assert!(
            claim("turn")?,
            "claim must not succeed without a durable write"
        );
        Ok(Delivery::Submitted)
    });
    fs::set_permissions(&f.home, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    let row = f.row();
    assert_eq!(row.revision, schedule.revision);
    assert!(row.last_run.is_none() && row.next_run == Some(1000));
    // Nothing was consumed: the same occurrence can still be delivered once.
    f.store.tick_with(1001, submit).unwrap();
    assert_eq!(f.row().last_run.unwrap().outcome, Outcome::Submitted);
}

#[test]
fn rate_window_restarts_after_the_clock_steps_backward() {
    let f = Fixture::new();
    for _ in 0..5 {
        f.store
            .save(
                None,
                "Backward".into(),
                "Isolated test prompt".into(),
                f.target(),
                Timing::Once { at: 900 },
                899,
            )
            .unwrap();
    }
    for now in 1000..1004 {
        f.store.tick_with(now, submit).unwrap();
    }
    let submitted = || {
        f.store
            .list()
            .unwrap()
            .iter()
            .filter(|s| {
                s.last_run
                    .as_ref()
                    .is_some_and(|r| r.outcome == Outcome::Submitted)
            })
            .count()
    };
    assert_eq!(submitted(), 4);
    // Still inside the old window, but the clock moved back 30 s.
    f.store.tick_with(970, submit).unwrap();
    assert_eq!(submitted(), 5);
}

#[test]
fn titles_and_prompts_reject_control_characters() {
    let f = Fixture::new();
    let save = |title: &str, prompt: &str| {
        f.store.save(
            None,
            title.into(),
            prompt.into(),
            f.target(),
            Timing::Once { at: 1000 },
            999,
        )
    };
    for title in [
        "Bad\u{1b}]0;owned\u{7}",
        "two\nlines",
        "tab\there",
        "del\u{7f}",
        "csi\u{9b}31m",
    ] {
        let error = save(title, "prompt").unwrap_err();
        assert!(
            error.contains("title") && error.contains("control"),
            "{title:?}: {error}"
        );
    }
    assert!(save("Fine", "prompt\u{1b}[0m").is_err());
    // Only the stored, trimmed title matters.
    assert_eq!(save("  Padded\n", "prompt").unwrap().title, "Padded");
    assert!(save(&"é".repeat(61), "prompt").is_err());
    assert!(save("Unicode ✓ title", "prompt").is_ok());
}

#[test]
fn only_codex_and_claude_can_be_scheduled() {
    assert!(HarnessKind::Codex.schedulable() && HarnessKind::Claude.schedulable());
    assert!(!HarnessKind::Grok.schedulable());
    let f = Fixture::new();
    let mut target = f.target();
    target.harness = HarnessKind::Grok;
    let error = f
        .store
        .save(
            None,
            "Grok".into(),
            "prompt".into(),
            target,
            Timing::Once { at: 1000 },
            999,
        )
        .unwrap_err();
    assert!(
        error.contains("grok") && error.contains("cannot be scheduled"),
        "{error}"
    );
    assert!(f.store.list().unwrap().is_empty());
}

#[test]
fn a_grok_session_is_refused_at_bind() {
    let f = RealFixture::new();
    let id = &f.ids[2];
    let registry = f.f.home.join("sessions.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
    for entry in value["sessions"].as_array_mut().unwrap() {
        if entry["id"] == id.as_str() {
            entry["harness"] = "grok".into();
        }
    }
    fs::write(registry, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = Target::bind(f.scopes().remove(2), &f.state, &f.sessions, id).unwrap_err();
    assert!(
        error.contains("grok") && error.contains("cannot be scheduled"),
        "{error}"
    );
}

// Single-line inputs: pasted line breaks separate words, and copying nothing
// must not have anything to write to the clipboard.
#[test]
fn pasted_line_breaks_become_one_space() {
    use crate::project_settings::Input;
    let mut input = Input::new(String::new());
    input.replace(None, "Step one\nStep two\r\nStep three\r\n\r\nDone\n");
    assert_eq!(input.text, "Step one Step two Step three Done");
    assert_eq!(input.selection, input.text.len()..input.text.len());
    input.replace(None, "\n");
    assert_eq!(input.text, "Step one Step two Step three Done");
    let mut input = Input::new("ab".into());
    input.replace(Some(1..1), "x\u{2028}y");
    assert_eq!(input.text, "ax yb");
}

#[test]
fn copy_and_cut_have_nothing_to_write_without_a_selection() {
    use crate::project_settings::Input;
    let mut input = Input::new("draft".into());
    assert_eq!(input.selected_text(), None);
    input.selection = 1..3;
    assert_eq!(input.selected_text(), Some("ra"));
}
