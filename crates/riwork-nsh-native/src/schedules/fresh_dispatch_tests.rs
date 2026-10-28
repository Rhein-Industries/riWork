//! Adapter -> real disposable ledger -> unchanged scheduler -> observed Unix
//! protocol -> real in-process host with fake providers. No executable/provider
//! launch: a retained listening socket also answers system_ensure's probe.
use super::*;
use riwork_engine::chat::{
    client::Client,
    model::{ChatCommand, ChatInfo, ChatState, ContributorMode},
    testing::{TestHost, eventually, fake_for, fake_providers, quick_options},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::{Barrier, atomic::AtomicBool},
};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fault {
    None,
    CapabilityDenied,
    CreateRefused,
    CreateReplyLost,
    SendRefused,
    SendReplyLost,
}

struct Tap {
    socket: PathBuf,
    backend: PathBuf,
    // Keep a listener alive even if the observer panics: system_ensure must
    // never fall through to launching its current executable in a fixture.
    listener: Option<UnixListener>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    seen: Arc<Mutex<Vec<Value>>>,
}
impl Tap {
    fn new(home: &Path, fault: Fault) -> Self {
        let socket = riwork_engine::chat::client::socket_path(home);
        let backend = socket.with_file_name("chat-observed.sock");
        std::fs::rename(&socket, &backend).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = listener.try_clone().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let upstream = backend.clone();
        let ledger = home.join("schedules.json");
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let (mut client, _) = match server.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("private tap accept: {e}"),
                };
                client.set_nonblocking(false).unwrap();
                // macOS refuses SO_RCVTIMEO after the connect-only probe has
                // already closed (EINVAL); its next read is immediate EOF.
                if let Err(error) = client.set_read_timeout(Some(Duration::from_secs(15))) {
                    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
                }
                let mut reader = BufReader::new(client.try_clone().unwrap());
                // system_ensure's connect-only probe sends no frame.
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap() == 0 {
                    continue;
                }
                client
                    .set_write_timeout(Some(Duration::from_secs(15)))
                    .unwrap();
                let mut host = UnixStream::connect(&upstream).unwrap();
                host.set_read_timeout(Some(Duration::from_secs(15)))
                    .unwrap();
                host.set_write_timeout(Some(Duration::from_secs(15)))
                    .unwrap();
                let mut replies = BufReader::new(host.try_clone().unwrap());
                let mut line = first;
                loop {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let op = request["op"].as_str().unwrap();
                    let disk: Value =
                        serde_json::from_slice(&std::fs::read(&ledger).unwrap()).unwrap();
                    captured
                        .lock()
                        .unwrap()
                        .push(json!({"request":request,"ledger":disk}));
                    let send = op == "command" && request["command"]["command"] == "send";
                    let refusal = (fault == Fault::CreateRefused && op == "create")
                        || (fault == Fault::SendRefused && send);
                    let response = if fault == Fault::CapabilityDenied && op == "capabilities" {
                        json!({"id":request["id"],"ok":true,"result":{"identified_create":false}})
                            .to_string()
                    } else if refusal {
                        json!({"id":request["id"],"ok":false,"error":"inert fixture refusal"})
                            .to_string()
                    } else {
                        host.write_all(line.as_bytes()).unwrap();
                        let mut reply = String::new();
                        assert!(replies.read_line(&mut reply).unwrap() > 0);
                        reply
                    };
                    // Forward the real operation, deliberately lose only its
                    // reply. Actual fake-provider effects may already exist.
                    if (fault == Fault::CreateReplyLost && op == "create")
                        || (fault == Fault::SendReplyLost && send)
                    {
                        break;
                    }
                    writeln!(client, "{}", response.trim_end()).unwrap();
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        break;
                    }
                }
            }
        });
        Self {
            socket,
            backend,
            listener: Some(listener),
            stop,
            worker: Some(worker),
            seen,
        }
    }
    fn events(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
    fn requests(&self, op: &str) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter(|e| e["request"]["op"] == op)
            .collect()
    }
    fn chats(&self) -> Vec<ChatInfo> {
        Client::connect(&self.backend).unwrap().list().unwrap()
    }
}
impl Drop for Tap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let result = self.worker.take().unwrap().join();
        self.listener.take();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::rename(&self.backend, &self.socket);
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

struct Dispatch {
    // Drop the tap before the host so its normal socket is restored first.
    tap: Tap,
    host: TestHost,
    adapter: EngineSchedules,
    row: ScheduleDto,
    due: u64,
}
impl Dispatch {
    fn new(provider: Provider, fault: Fault) -> Self {
        let mut providers = fake_providers();
        // Resolve only the private HOME/system-default binding, exactly as the
        // adapter does; fake_account's invented account-a would mismatch it.
        providers.account = riwork_engine::chat::launch::account_for;
        let host = TestHost::with_providers(quick_options(), providers);
        let project = Store::open(host.home.clone())
            .unwrap()
            .add_project(host.work(), Some("Fresh fixture"))
            .unwrap();
        let adapter = EngineSchedules::new(host.home.clone());
        let due = engine::now() + 10;
        let row = adapter
            .create(
                CreateScheduleDto {
                    scope: ScheduleScopeDto::from(&Scope::Project {
                        project_id: project.id,
                    }),
                    title: "Fresh fixture title".into(),
                    prompt: "inert scheduled fresh prompt".into(),
                    at: iso_time(due),
                    every_minutes: None,
                    binding: None,
                    options: Some(ScheduleChatOptionsDto {
                        provider: crate::chat::dto::wire_name(&provider),
                        model: Some("fixture-model".into()),
                        effort: Some("high".into()),
                        fast: false,
                        permission: "plan".into(),
                        account_id: None,
                    }),
                },
                true,
            )
            .unwrap();
        let tap = Tap::new(&host.home, fault);
        Self {
            tap,
            host,
            adapter,
            row,
            due,
        }
    }
    fn tick(&self, at: u64) {
        assert!(self.tap.socket.exists());
        assert!(!self.tap.worker.as_ref().unwrap().is_finished());
        ScheduleStore::at(self.host.home.clone())
            .unwrap()
            .tick_tracked(at, &mut HashMap::new())
            .unwrap();
    }
    fn saved(&self) -> ScheduleDto {
        self.adapter.show(&self.row.key.id).unwrap()
    }
    fn disk(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.host.home.join("schedules.json")).unwrap())
            .unwrap()
    }
    fn sends(&self) -> usize {
        fake_for(&self.host.work().canonicalize().unwrap())
            .commands()
            .iter()
            .filter(|c| matches!(c, ChatCommand::Send { .. }))
            .count()
    }
    fn audit(&self, label: &str) {
        let wire: Vec<_> = self
            .tap
            .events()
            .iter()
            .map(|e| {
                json!({
                    "request":e["request"],
                    "durable_run_before_request":e["ledger"]["schedules"][0]["last_run"],
                    "next_run_before_request":e["ledger"]["schedules"][0]["next_run"],
                    "consumed_before_request":e["ledger"]["consumed"],
                    "attempts_before_request":e["ledger"]["sends_in_window"],
                })
            })
            .collect();
        println!(
            "fresh audit {label}: {}",
            json!({"wire":wire,"final":self.saved(),"fake_sends":self.sends()})
        );
    }
    fn assert_claim_before_mutations(&self) {
        for event in self
            .tap
            .events()
            .iter()
            .filter(|e| matches!(e["request"]["op"].as_str(), Some("create" | "command")))
        {
            let row = &event["ledger"]["schedules"][0];
            let id = event["request"]["chat_id"].as_str().unwrap();
            assert_eq!(row["last_run"]["outcome"], "dispatching");
            assert_eq!(row["last_run"]["created_chat_id"], id);
            assert_eq!(row["next_run"], Value::Null);
            assert_eq!(event["ledger"]["sends_in_window"], 1);
            assert_eq!(
                event["ledger"]["consumed"][0],
                json!([self.row.key.target_id, format!("new-chat:{id}")])
            );
        }
    }
}

#[test]
fn p6b_fresh_01_adapter_dispatches_codex_claude_and_muse_once_with_durable_wire_claim() {
    for provider in [Provider::Codex, Provider::Claude, Provider::Muse] {
        let fixture = Dispatch::new(provider, Fault::None);
        fixture.tick(fixture.due - 1);
        assert!(fixture.tap.events().is_empty());
        fixture.tick(fixture.due);
        let row = fixture.saved();
        assert_eq!(
            row.last_run.as_ref().unwrap().outcome,
            "submitted",
            "{row:?}"
        );
        let id = row
            .last_run
            .as_ref()
            .unwrap()
            .created_chat_id
            .as_ref()
            .unwrap();
        let chat = fixture
            .tap
            .chats()
            .into_iter()
            .find(|c| &c.id == id)
            .unwrap();
        assert_eq!(chat.provider, provider);
        assert_eq!(chat.project_id, row.key.scope.project_id);
        assert_eq!(chat.worktree_id, None);
        assert_eq!(chat.orchestrator, None);
        assert_eq!(chat.cwd, fixture.host.work().canonicalize().unwrap());
        assert_eq!(
            chat.codex_account_id,
            row.options.as_ref().unwrap().account_id
        );
        assert_eq!(chat.title, "Fresh fixture title");
        assert_eq!(chat.model.as_deref(), Some("fixture-model"));
        assert_eq!(chat.effort.as_deref(), Some("high"));
        assert_eq!(chat.approval_mode, ApprovalMode::Plan);
        assert!(!chat.fast);
        if provider == Provider::Muse {
            assert_eq!(chat.reported_contributor_mode(), Some(ContributorMode::Off));
        }
        assert_eq!(fixture.tap.requests("capabilities").len(), 1);
        assert_eq!(
            fixture.tap.requests("capabilities")[0]["ledger"]["schedules"][0]["last_run"],
            Value::Null
        );
        assert_eq!(fixture.tap.requests("create").len(), 1);
        assert_eq!(fixture.tap.requests("command").len(), 1);
        fixture.assert_claim_before_mutations();
        assert_eq!(fixture.sends(), 1);
        assert!(fixture.host.log(id).iter().any(|event| matches!(&event.event,
            riwork_engine::chat::model::ChatEvent::ItemStarted { item }
            if matches!(&item.body, riwork_engine::chat::model::ItemBody::UserMessage { text } if text == "inert scheduled fresh prompt"))));
        let before = fixture.tap.events().len();
        for at in [fixture.due, fixture.due + 1, fixture.due + 301] {
            fixture.tick(at);
        }
        assert_eq!(fixture.tap.events().len(), before);
        assert_eq!(fixture.saved(), row);
        fixture.audit(&format!("{provider:?}"));
        println!(
            "fresh {provider:?}: capability -> durable claim -> Create -> Send -> submitted; reopen/repeat created=1 sent=1"
        );
    }
}

#[test]
fn p6b_fresh_02_capability_denial_never_claims_creates_or_sends() {
    let fixture = Dispatch::new(Provider::Claude, Fault::CapabilityDenied);
    for at in [fixture.due, fixture.due + 15] {
        fixture.tick(at);
    }
    let row = fixture.saved();
    assert_eq!(row.last_run.as_ref().unwrap().outcome, "deferred");
    assert_eq!(row.last_run.unwrap().created_chat_id, None);
    assert_eq!(row.next_run, Some(fixture.due));
    assert!(!row.paused && !row.review_required);
    assert_eq!(fixture.tap.requests("capabilities").len(), 2);
    assert_eq!(fixture.tap.events().len(), 2);
    assert_eq!(fixture.disk()["sends_in_window"], 0);
    assert_eq!(fixture.disk()["consumed"], json!([]));
    assert!(fixture.tap.chats().is_empty());
    assert_eq!(fixture.sends(), 0);
    fixture.tick(fixture.due + 301);
    assert_eq!(fixture.saved().last_run.unwrap().outcome, "missed");
    fixture.audit("capability denied -> expiry");
}

#[test]
fn p6b_fresh_03_concurrent_ticks_compete_for_one_occurrence_and_one_claim() {
    let fixture = Dispatch::new(Provider::Claude, Fault::None);
    let barrier = Arc::new(Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let gate = barrier.clone();
            let home = fixture.host.home.clone();
            let due = fixture.due;
            thread::spawn(move || {
                gate.wait();
                ScheduleStore::at(home)
                    .unwrap()
                    .tick_tracked(due, &mut HashMap::new())
                    .unwrap();
            })
        })
        .collect();
    // Join every contender before asserting: a failed worker must not leave
    // another tick running after the fixture's retained listener is dropped.
    let completed: Vec<_> = workers.into_iter().map(|worker| worker.join()).collect();
    for result in completed {
        result.unwrap();
    }
    assert_eq!(fixture.saved().last_run.unwrap().outcome, "submitted");
    assert_eq!(fixture.tap.requests("capabilities").len(), 1);
    assert_eq!(fixture.tap.requests("create").len(), 1);
    assert_eq!(fixture.sends(), 1);
    fixture.assert_claim_before_mutations();
    fixture.audit("four competing ticks");
}

#[test]
fn p6b_fresh_04_denied_claim_after_capability_never_creates() {
    let fixture = Dispatch::new(Provider::Claude, Fault::None);
    let mut disk = fixture.disk();
    disk["window_started"] = fixture.due.into();
    disk["sends_in_window"] = 4.into();
    std::fs::write(
        fixture.host.home.join("schedules.json"),
        serde_json::to_vec(&disk).unwrap(),
    )
    .unwrap();
    fixture.tick(fixture.due);
    let row = fixture.saved();
    assert_eq!(row.last_run.as_ref().unwrap().outcome, "deferred");
    assert_eq!(row.last_run.unwrap().created_chat_id, None);
    assert_eq!(fixture.tap.events().len(), 1);
    assert_eq!(fixture.disk()["sends_in_window"], 4);
    assert_eq!(fixture.disk()["consumed"], json!([]));
    assert!(fixture.tap.chats().is_empty());
    assert_eq!(fixture.sends(), 0);
    fixture.audit("claim denied by existing four-attempt window");
}

#[test]
fn p6b_fresh_05_create_and_send_refusals_and_lost_replies_pause_without_retry() {
    for fault in [
        Fault::CreateRefused,
        Fault::CreateReplyLost,
        Fault::SendRefused,
        Fault::SendReplyLost,
    ] {
        let fixture = Dispatch::new(Provider::Claude, fault);
        fixture.tick(fixture.due);
        let row = fixture.saved();
        let expected = if matches!(fault, Fault::CreateRefused | Fault::SendRefused) {
            "failed"
        } else {
            "uncertain"
        };
        assert_eq!(
            row.last_run.as_ref().unwrap().outcome,
            expected,
            "{fault:?}: {row:?}"
        );
        assert!(row.last_run.as_ref().unwrap().created_chat_id.is_some());
        assert!(row.paused && row.review_required);
        fixture.assert_claim_before_mutations();
        assert_eq!(fixture.tap.requests("create").len(), 1);
        if fault == Fault::SendReplyLost {
            eventually(|| fixture.sends() == 1);
        } else {
            assert_eq!(fixture.sends(), 0);
        }
        assert_eq!(
            fixture.tap.chats().len(),
            usize::from(fault != Fault::CreateRefused)
        );
        let attempts = fixture.tap.events().len();
        fixture.tick(fixture.due + 1);
        fixture.tick(fixture.due + 60);
        assert_eq!(fixture.tap.events().len(), attempts);
        assert_eq!(fixture.saved(), row);
        fixture.audit(&format!("{fault:?}"));
        println!(
            "fresh {fault:?}: {expected}, retained chat id, paused/review required, no retry; fake Sends={}",
            fixture.sends()
        );
    }
}

#[test]
fn p6b_fresh_06_created_chat_start_failure_never_receives_prompt() {
    let fixture = Dispatch::new(Provider::Claude, Fault::None);
    *fake_for(&fixture.host.work().canonicalize().unwrap())
        .fail_start
        .lock()
        .unwrap() = Some("inert startup refusal".into());
    fixture.tick(fixture.due);
    let row = fixture.saved();
    assert_eq!(row.last_run.as_ref().unwrap().outcome, "failed");
    assert!(row.paused && row.review_required);
    let chat = fixture.tap.chats().remove(0);
    assert!(matches!(chat.state, ChatState::Failed { .. }));
    assert_eq!(fixture.sends(), 0);
    fixture.tick(fixture.due + 1);
    assert_eq!(fixture.tap.requests("create").len(), 1);
    fixture.audit("fake provider startup failed");
}
