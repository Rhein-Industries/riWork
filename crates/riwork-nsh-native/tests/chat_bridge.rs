//! Phase 4a chat bridge tests (contracts §1.6, D-PERF-1 structural guards).
//!
//! Hosts are the engine's in-process chat host on isolated short homes; the
//! Codex chats run the real Codex driver against `fake_provider.py` replaying the
//! engine's recorded provider streams (`testdata/codex/*.ndjson`). Nothing here
//! starts `riwork chat ensure`, reads a real home, or touches a live window.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use riwork_engine::chat::{
    host::Providers,
    model::{
        ChatEvent, ChatState, Delta, Item, ItemBody, ItemStatus, NewChat, Presentation, Provider,
    },
    projection::DisplayMode,
    testing::{TestHost, quick_options},
    wire::Envelope,
};
use riwork_nsh_native::chat::{
    ChatBackend, ChatHandle, drafts,
    dto::{BodyDto, ChatDelivery, ChatPanelDto, RowDto, SubmitRequest},
    service::fixture,
    view::{ChatView, OVERSCAN, system_clock},
    worker::WorkerMsg,
};
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(20);

/// One draft home for the whole test process (the draft store is a process
/// singleton, as in the app); sockets are per host.
fn drafts_home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = std::env::temp_dir().join(format!(
            "rwd-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir_all(&home).unwrap();
        home
    })
}

/// The recorded provider programs, written once for this process.
fn replay_providers() -> Providers {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = fixture::write_providers(drafts_home()).unwrap();
        fixture::set_providers(dir.clone());
        dir
    });
    Providers {
        codex: riwork_engine::chat::codex::start,
        claude: riwork_engine::chat::codex::start,
        muse: riwork_engine::chat::codex::start,
        account: fixture::no_account,
        config: fixture::replay_config,
    }
}

struct TestBackend {
    socket: PathBuf,
}

impl ChatBackend for TestBackend {
    fn home(&self) -> Result<PathBuf, String> {
        Ok(drafts_home().clone())
    }
    fn existing(&self) -> Result<PathBuf, String> {
        Ok(self.socket.clone())
    }
    fn ensure(&self) -> Result<PathBuf, String> {
        Ok(self.socket.clone())
    }
}

/// A replay host and a Codex chat whose provider replays `recording`.
fn replay_chat(recording: &str) -> (TestHost, String) {
    let host = TestHost::with_providers(quick_options(), replay_providers());
    let cwd = host.work().join(recording);
    std::fs::create_dir_all(&cwd).unwrap();
    let info = host
        .client()
        .create(NewChat {
            cwd,
            ..host.new_chat(Provider::Codex)
        })
        .unwrap();
    (host, info.id)
}

fn open(host: &TestHost, chat_id: &str) -> ChatHandle {
    ChatHandle::start(
        Arc::new(TestBackend {
            socket: host.socket(),
        }),
        chat_id.to_owned(),
        false,
        Some(DisplayMode::Normal),
    )
}

/// What TSX keeps: the same patch application the transcript List does.
#[derive(Default)]
struct Mirror {
    rows: Vec<RowDto>,
    bodies: HashMap<String, BodyDto>,
    panel: Option<ChatPanelDto>,
    deliveries: Vec<(Instant, ChatDelivery)>,
}

impl Mirror {
    fn apply(&mut self, delivery: ChatDelivery) {
        if delivery.kind == "reset" {
            self.rows = delivery.rows.clone().unwrap_or_default();
            self.bodies = delivery.bodies.clone().into_iter().collect();
        } else {
            let previous: Vec<String> = self.rows.iter().map(|row| row.key.clone()).collect();
            let mut by_key: HashMap<String, RowDto> = self
                .rows
                .drain(..)
                .map(|row| (row.key.clone(), row))
                .collect();
            for key in &delivery.remove {
                by_key.remove(key);
            }
            let mut fresh = Vec::new();
            for row in &delivery.upsert {
                if by_key.insert(row.key.clone(), row.clone()).is_none() {
                    fresh.push(row.key.clone());
                }
            }
            let order = delivery.order.clone().unwrap_or_else(|| {
                assert!(fresh.is_empty(), "new rows always come with an order");
                previous
            });
            self.rows = order
                .iter()
                .filter(|key| !delivery.remove.contains(key))
                .map(|key| by_key.remove(key).expect("ordered key has a row"))
                .collect();
            for key in &delivery.drop {
                self.bodies.remove(key);
            }
            for (key, body) in &delivery.bodies {
                self.bodies.insert(key.clone(), body.clone());
            }
            for splice in &delivery.splice {
                match self.bodies.get_mut(&splice.key) {
                    Some(BodyDto::Markdown { blocks }) => {
                        blocks.truncate(splice.from);
                        blocks.extend(splice.blocks.iter().cloned());
                        assert_eq!(blocks.len(), splice.len);
                    }
                    other => panic!("splice for a body that is not Markdown: {other:?}"),
                }
            }
            for append in &delivery.append {
                match self.bodies.get_mut(&append.key) {
                    Some(BodyDto::Text { text, .. }) => text.push_str(&append.text),
                    Some(BodyDto::Command { output, .. }) => output.push_str(&append.text),
                    other => panic!("append for a body that is not text: {other:?}"),
                }
            }
        }
        if let Some(panel) = &delivery.panel {
            self.panel = Some(panel.clone());
        }
        self.deliveries.push((Instant::now(), delivery));
    }

    fn panel(&self) -> &ChatPanelDto {
        self.panel.as_ref().expect("a reset came first")
    }

    fn body_text(&self, key: &str) -> Option<String> {
        match self.bodies.get(key)? {
            BodyDto::Markdown { blocks } => Some(plain(blocks)),
            BodyDto::Text { text, .. } => Some(text.clone()),
            BodyDto::Command { output, .. } => Some(output.clone()),
            BodyDto::Detail { text } => Some(text.clone()),
            BodyDto::Changes { .. } | BodyDto::Tool { .. } => None,
            BodyDto::Steps { steps, .. } => Some(
                steps
                    .iter()
                    .map(|step| step.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }

    /// Receive until `done` holds, folding every delivery in.
    fn until(
        &mut self,
        receiver: &async_channel::Receiver<ChatDelivery>,
        what: &str,
        done: impl Fn(&Mirror) -> bool,
    ) {
        let end = Instant::now() + WAIT;
        while !(self.panel.is_some() && done(self)) {
            let left = end.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "timed out waiting for {what}; panel {:#?}",
                self.panel
            );
            match recv_timeout(receiver, left) {
                Some(delivery) => self.apply(delivery),
                None => panic!("timed out waiting for {what}; panel {:#?}", self.panel),
            }
        }
    }
}

/// The patches folded so far equal a fresh reset of the worker's state: patch
/// application (the TSX side) and the view never disagree.
fn assert_mirror_matches(handle: &ChatHandle, mirror: &Mirror) {
    let fresh = handle.subscribe_channel();
    let reset = recv_timeout(&fresh, WAIT).expect("a reset");
    assert_eq!(reset.kind, "reset");
    assert_eq!(reset.rows.as_ref().unwrap(), &mirror.rows, "rows diverged");
    for (key, body) in &reset.bodies {
        assert_eq!(mirror.bodies.get(key), Some(body), "body {key} diverged");
    }
}

fn recv_timeout(
    receiver: &async_channel::Receiver<ChatDelivery>,
    timeout: Duration,
) -> Option<ChatDelivery> {
    let end = Instant::now() + timeout;
    loop {
        match receiver.try_recv() {
            Ok(delivery) => return Some(delivery),
            Err(async_channel::TryRecvError::Closed) => return None,
            Err(async_channel::TryRecvError::Empty) if Instant::now() >= end => return None,
            Err(async_channel::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

fn plain(blocks: &[Value]) -> String {
    fn walk(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                if let Some(Value::String(text)) = map.get("text") {
                    out.push_str(text);
                }
                if let Some(Value::String(code)) = map.get("code") {
                    out.push_str(code);
                }
                for key in ["content", "children", "items"] {
                    if let Some(child) = map.get(key) {
                        walk(child, out);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    blocks.iter().for_each(|block| walk(block, &mut out));
    out
}

fn submit(
    handle: &ChatHandle,
    text: &str,
    generation: u64,
) -> riwork_nsh_native::chat::dto::ReceiptDto {
    let parts = handle.parts();
    let request = SubmitRequest {
        attachments: Vec::new(),
        text: text.into(),
        editor: 1,
        generation,
    };
    runtime().block_on(async move { parts.submit(request).await })
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    })
}

fn exchange(
    handle: &ChatHandle,
    command: riwork_engine::chat::model::ChatCommand,
) -> Result<(), String> {
    let parts = handle.parts();
    runtime().block_on(async move { parts.exchange(command, false).await })
}

fn record_saw_no_mismatch(recording: &str) {
    let record = drafts_home()
        .join("fixture-providers")
        .join(format!("{recording}.record.jsonl"));
    let text = std::fs::read_to_string(&record).unwrap_or_default();
    assert!(!text.contains("mismatch"), "{recording}: {text}");
}

#[test]
fn approval_round_trip_through_a_replayed_codex_stream() {
    let (host, chat) = replay_chat("approval_accept");
    let handle = open(&host, &chat);
    let receiver = handle.subscribe_channel();
    let mut mirror = Mirror::default();
    mirror.until(&receiver, "the chat to be idle", |m| {
        m.panel().state == "idle"
    });
    let receipt = submit(&handle, "write it", 1);
    assert_eq!(receipt.status, "submitted", "{receipt:?}");
    assert!(receipt.clear);
    mirror.until(&receiver, "an approval", |m| {
        !m.panel().approvals.is_empty()
    });
    let approval = mirror.panel().approvals[0].clone();
    assert_eq!(approval.heading, "Run a command?");
    assert_eq!(approval.kind, "command");
    let offered: Vec<_> = approval
        .choices
        .iter()
        .map(|c| c.decision.as_str())
        .collect();
    assert!(offered.contains(&"accept"), "{offered:?}");
    let allow = approval
        .choices
        .iter()
        .find(|c| c.decision == "accept")
        .unwrap();
    assert_eq!(
        (allow.label.as_str(), allow.key.as_deref()),
        ("Allow", Some("⏎"))
    );
    // The provider reports Running or Waiting while it asks; either keeps Stop and ⌘.
    assert!(matches!(
        mirror.panel().state.as_str(),
        "running" | "waiting"
    ));
    assert!(
        mirror.panel().running,
        "a pending request keeps Stop and ⌘. available"
    );
    exchange(
        &handle,
        riwork_engine::chat::model::ChatCommand::Approve {
            request_id: approval.request_id.clone(),
            decision: riwork_engine::chat::model::Decision::Accept,
        },
    )
    .unwrap();
    mirror.until(&receiver, "the answer after the approval", |m| {
        m.panel().approvals.is_empty()
            && m.panel().state == "idle"
            && m.rows.iter().any(|row| row.kind == "final")
    });
    let answer = mirror.rows.iter().find(|row| row.kind == "final").unwrap();
    assert_eq!(mirror.body_text(&answer.key).as_deref(), Some("Done"));
    let request = mirror
        .rows
        .iter()
        .find(|row| row.kind == "request")
        .unwrap();
    assert!(request.title.starts_with("Allowed"), "{request:?}");
    let activity = mirror
        .rows
        .iter()
        .find(|row| row.kind == "activity")
        .unwrap();
    assert!(activity.title.starts_with("Worked"), "{activity:?}");
    assert!(!activity.running);
    assert_mirror_matches(&handle, &mirror);
    drop(handle);
    drop(host);
    record_saw_no_mismatch("approval_accept");
}

#[test]
fn an_interrupted_turn_closes_as_interrupted_and_stops_running() {
    let (host, chat) = replay_chat("interrupt");
    let handle = open(&host, &chat);
    let receiver = handle.subscribe_channel();
    let mut mirror = Mirror::default();
    mirror.until(&receiver, "idle", |m| m.panel().state == "idle");
    assert_eq!(submit(&handle, "sleep", 1).status, "submitted");
    mirror.until(&receiver, "the running command", |m| {
        m.panel().running
            && m.rows
                .iter()
                .any(|row| row.kind == "activity" && row.running)
    });
    assert_eq!(mirror.panel().status.as_deref(), Some("Working…"));
    exchange(&handle, riwork_engine::chat::model::ChatCommand::Interrupt).unwrap();
    mirror.until(&receiver, "the interrupted turn", |m| {
        !m.panel().running
            && m.rows
                .iter()
                .any(|row| row.kind == "activity" && row.title.ends_with("Interrupted"))
    });
    let activity = mirror
        .rows
        .iter()
        .find(|row| row.kind == "activity")
        .unwrap();
    assert!(!activity.running, "{activity:?}");
    assert_eq!(mirror.panel().status, None);
    assert_mirror_matches(&handle, &mirror);
    // The command that was cut short is closed as interrupted, not left running.
    let view_items: Vec<_> = host
        .log(&chat)
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            ChatEvent::TurnCompleted { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(
        view_items,
        vec![riwork_engine::chat::model::TurnOutcome::Interrupted]
    );
    drop(handle);
    drop(host);
    record_saw_no_mismatch("interrupt");
}

#[test]
fn a_message_sent_while_a_turn_runs_steers_it() {
    let (host, chat) = replay_chat("steer");
    let handle = open(&host, &chat);
    let receiver = handle.subscribe_channel();
    let mut mirror = Mirror::default();
    mirror.until(&receiver, "idle", |m| m.panel().state == "idle");
    assert_eq!(submit(&handle, "first", 1).status, "submitted");
    mirror.until(&receiver, "the running turn", |m| m.panel().running);
    assert_eq!(submit(&handle, "second", 2).status, "submitted");
    mirror.until(&receiver, "the steered answer", |m| {
        m.panel().state == "idle" && m.rows.iter().any(|row| row.kind == "final")
    });
    assert_mirror_matches(&handle, &mirror);
    let users: Vec<_> = mirror
        .rows
        .iter()
        .filter(|row| row.kind == "user")
        .filter_map(|row| mirror.body_text(&row.key))
        .collect();
    assert_eq!(users, ["first", "second"]);
    let answer = mirror.rows.iter().find(|row| row.kind == "final").unwrap();
    assert_eq!(
        mirror.body_text(&answer.key).as_deref(),
        Some("second observed")
    );
    drop(handle);
    drop(host);
    record_saw_no_mismatch("steer");
}

#[test]
fn questions_are_answered_once_then_the_access_request_is_allowed_for_the_session() {
    let (host, chat) = replay_chat("question");
    let handle = open(&host, &chat);
    let receiver = handle.subscribe_channel();
    let mut mirror = Mirror::default();
    mirror.until(&receiver, "idle", |m| m.panel().state == "idle");
    assert_eq!(submit(&handle, "ask me", 1).status, "submitted");
    mirror.until(&receiver, "questions", |m| !m.panel().questions.is_empty());
    let question = mirror.panel().questions[0].clone();
    assert_eq!(question.prompts.len(), 2);
    let parts = handle.parts();
    let request_id = question.request_id.clone();
    let result = runtime().block_on(async move {
        parts
            .exchange(
                riwork_engine::chat::model::ChatCommand::Answer {
                    request_id,
                    answers: vec![
                        vec!["Strict (Recommended)".into()],
                        vec!["no thanks".into()],
                    ],
                },
                true,
            )
            .await
    });
    assert_eq!(result, Ok(()));
    mirror.until(&receiver, "the access request", |m| {
        m.panel().questions.is_empty() && !m.panel().approvals.is_empty()
    });
    let approval = mirror.panel().approvals[0].clone();
    assert_eq!(approval.heading, "Grant more access?");
    let session = approval
        .choices
        .iter()
        .find(|choice| choice.decision == "accept_for_session")
        .expect("allow for session is offered");
    assert_eq!(session.key, None, "never a key hint on Allow for session");
    exchange(
        &handle,
        riwork_engine::chat::model::ChatCommand::Approve {
            request_id: approval.request_id,
            decision: riwork_engine::chat::model::Decision::AcceptForSession,
        },
    )
    .unwrap();
    mirror.until(&receiver, "the turn to finish", |m| {
        m.panel().approvals.is_empty() && m.panel().state == "idle" && !m.panel().running
    });
    assert_mirror_matches(&handle, &mirror);
    drop(handle);
    drop(host);
    record_saw_no_mismatch("question");
}

#[test]
fn drafts_save_restore_survive_a_reload_and_clear_only_after_a_receipt() {
    // The fake host: `Send` scripts a whole echo turn; a failing command is refused.
    let host = TestHost::new();
    let chat = host.create(Provider::Claude).id;
    let handle = open(&host, &chat);
    let receiver = handle.subscribe_channel();
    let mut mirror = Mirror::default();
    mirror.until(&receiver, "idle", |m| m.panel().state == "idle");
    // Every change saves the whole draft; the writer keeps the newest.
    for (generation, text) in ["h", "half", "half a thought\n  with indent"]
        .iter()
        .enumerate()
    {
        handle.post(riwork_nsh_native::chat::save_draft_work(
            chat.clone(),
            (*text).into(),
            (1, generation as u64 + 1),
        ));
    }
    let (sync, done) = std::sync::mpsc::channel();
    handle.post(WorkerMsg::Run(Box::new(move |_| sync.send(()).unwrap())));
    done.recv_timeout(WAIT).unwrap();
    drafts::flush_pending(Duration::from_secs(5)).unwrap();
    let file = drafts_home().join("chat-drafts").join(&chat);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "half a thought\n  with indent",
        "a text-only draft is the plain legacy file"
    );
    // A fresh store (an app restart) reads it back. It loads a copy: a second
    // live store on one directory would sweep the writer's temp files (one
    // process per home is the host's job).
    let restarted = std::env::temp_dir().join(format!("rwr-{}", &chat[..8]));
    std::fs::create_dir_all(restarted.join("chat-drafts")).unwrap();
    std::fs::copy(&file, restarted.join("chat-drafts").join(&chat)).unwrap();
    let reloaded = drafts::Drafts::load(&restarted, std::time::SystemTime::now());
    assert_eq!(
        reloaded.recover(&chat).unwrap().0.draft.text,
        "half a thought\n  with indent"
    );
    drop(reloaded);
    // A reopened tab restores it into an empty composer.
    let reopened = open(&host, &chat);
    let parts = reopened.parts();
    let restored = runtime()
        .block_on(async move { parts.restore_draft().await })
        .unwrap()
        .unwrap();
    assert_eq!(restored.text, "half a thought\n  with indent");
    assert!(restored.submissions.is_empty());
    // A refused send keeps a saved submission and the composer's text.
    *host.fake().fail_command.lock().unwrap() = Some("provider said no".into());
    let refused = submit(&handle, "half a thought\n  with indent", 3);
    assert_eq!(refused.status, "refused", "{refused:?}");
    assert!(!refused.clear);
    assert!(
        refused
            .error
            .as_deref()
            .unwrap()
            .contains("provider said no")
    );
    let parts = reopened.parts();
    let after = runtime()
        .block_on(async move { parts.restore_draft().await })
        .unwrap()
        .unwrap();
    assert_eq!(after.submissions.len(), 1);
    assert_eq!(after.submissions[0].status, "refused");
    // Dismissing it and sending again succeeds and clears the draft record.
    reopened.post(WorkerMsg::Run(Box::new({
        let (chat, key) = (chat.clone(), after.submissions[0].key.clone());
        move |_| drafts::dismiss_submission(&chat, &key)
    })));
    let receipt = submit(&handle, "half a thought\n  with indent", 4);
    assert_eq!(receipt.status, "submitted", "{receipt:?}");
    mirror.until(&receiver, "the echo", |m| {
        m.rows.iter().any(|row| row.kind == "final") && m.panel().state == "idle"
    });
    drafts::flush_pending(Duration::from_secs(5)).unwrap();
    let parts = reopened.parts();
    let cleared = runtime()
        .block_on(async move { parts.restore_draft().await })
        .unwrap();
    assert!(
        cleared
            .as_ref()
            .is_none_or(|draft| draft.text.is_empty() && draft.submissions.is_empty()),
        "{cleared:?}"
    );
    assert!(!file.exists(), "an empty record deletes the file");
}

#[test]
fn a_deleted_chat_reports_deleted_and_forgets_its_draft() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude).id;
    let handle = open(&host, &chat);
    let receiver = handle.subscribe_channel();
    let mut mirror = Mirror::default();
    mirror.until(&receiver, "live", |m| m.panel().link == "live");
    handle.post(riwork_nsh_native::chat::save_draft_work(
        chat.clone(),
        "keep?".into(),
        (1, 1),
    ));
    host.client().delete(&chat).unwrap();
    mirror.until(&receiver, "deleted", |m| m.panel().link == "deleted");
    let (sync, done) = std::sync::mpsc::channel();
    handle.post(WorkerMsg::Run(Box::new(move |_| sync.send(()).unwrap())));
    done.recv_timeout(WAIT).unwrap();
    assert_eq!(drafts::draft(&chat), None);
}

#[test]
fn streaming_is_coalesced_to_one_delivery_per_16_ms_and_a_quiet_chat_delivers_nothing() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude).id;
    let handle = open(&host, &chat);
    let receiver = handle.subscribe_channel();
    let mut mirror = Mirror::default();
    mirror.until(&receiver, "idle", |m| m.panel().state == "idle");
    let start = mirror.deliveries.len();
    // 300 deltas of 64 bytes, paced 2 ms apart by the fake driver. The fake
    // replies only after its whole turn, so read while the send is in flight.
    let parts = handle.parts();
    let sending = std::thread::spawn(move || {
        let started = Instant::now();
        let receipt = runtime().block_on(async move {
            parts
                .submit(SubmitRequest {
                    attachments: Vec::new(),
                    text: "flood:300:64".into(),
                    editor: 1,
                    generation: 1,
                })
                .await
        });
        eprintln!("flood receipt after {:?}: {receipt:?}", started.elapsed());
        receipt
    });
    let streaming = Instant::now();
    // The fake completes the message with empty text, so record how far it grew.
    let mut longest = 0;
    let mut grew = 0;
    let mut ran = false;
    let end = Instant::now() + WAIT;
    while !(ran && !mirror.panel().running && mirror.panel().state == "idle") {
        let Some(delivery) = recv_timeout(&receiver, end.saturating_duration_since(Instant::now()))
        else {
            let receipt = sending.is_finished().then(|| format!("{:?}", "finished"));
            panic!(
                "the flood did not finish: send {receipt:?}, ran {ran}, longest {longest}, {} deliveries, state {}, rows {:?}, log tail {:?}",
                mirror.deliveries.len(),
                mirror.panel().state,
                mirror
                    .rows
                    .iter()
                    .map(|row| (&row.kind, row.running))
                    .collect::<Vec<_>>(),
                host.log(&chat)
                    .iter()
                    .rev()
                    .take(4)
                    .map(|e| format!("{}:{:?}", e.seq, e.event))
                    .map(|t| t.chars().take(120).collect::<String>())
                    .collect::<Vec<_>>()
            );
        };
        mirror.apply(delivery);
        ran |= mirror.panel().running;
        if let Some(answer) = mirror.rows.iter().find(|row| row.kind == "final") {
            let length = mirror.body_text(&answer.key).map_or(0, |text| text.len());
            if length > longest {
                grew += 1;
                longest = length;
            }
        }
    }
    let streamed = streaming.elapsed();
    let receipt = sending.join().unwrap();
    assert_eq!(receipt.status, "submitted", "{receipt:?}");
    let deliveries = &mirror.deliveries[start..];
    let gaps: Vec<_> = deliveries
        .windows(2)
        .map(|pair| pair[1].0.duration_since(pair[0].0))
        .collect();
    let bound = (streamed.as_millis() / 16) as usize + 8;
    assert!(
        deliveries.len() <= bound,
        "{} deliveries for 300 deltas in {streamed:?} (bound {bound})",
        deliveries.len()
    );
    // The receiving side may batch, so allow a little jitter below 16 ms.
    let short = gaps
        .iter()
        .filter(|gap| **gap < Duration::from_millis(12))
        .count();
    assert!(short <= gaps.len() / 10, "too many short gaps: {gaps:?}");
    // Streaming text arrived as splices/appends, never as a whole resent message.
    let resent = deliveries
        .iter()
        .filter(|(_, delivery)| delivery.bodies.values().any(|body| matches!(body, BodyDto::Markdown { blocks } if plain(blocks).len() > 64 * 50)))
        .count();
    assert!(
        resent <= 1,
        "{resent} deliveries re-sent the streamed message"
    );
    // The last deltas may share a delivery with the completion (which the fake
    // sends with empty text): coalescing, not loss.
    assert!(longest >= 300 * 64 * 9 / 10, "streamed {longest} bytes");
    assert!(grew >= 10, "the message grew in {grew} deliveries");
    // Quiet: nothing more arrives.
    assert!(recv_timeout(&receiver, Duration::from_millis(600)).is_none());
    assert_mirror_matches(&handle, &mirror);
}

// ---- Structural guards on the 2,200-call disclosed turn (D-PERF-1 rule 1) ----

fn large_turn(calls: usize) -> Vec<Envelope> {
    let chat = "perf-chat".to_owned();
    let mut seq = 0;
    let mut next = |event| {
        seq += 1;
        Envelope {
            chat_id: chat.clone(),
            seq,
            event,
        }
    };
    let mut events = vec![
        next(ChatEvent::TurnStarted {
            turn_id: "perf".into(),
            started_at: None,
        }),
        next(ChatEvent::ItemStarted {
            item: Item {
                id: "user".into(),
                turn_id: Some("perf".into()),
                status: ItemStatus::Completed,
                presentation: Presentation::default(),
                started_at: None,
                ended_at: None,
                body: ItemBody::UserMessage {
                    text: "read everything".into(),
                },
            },
        }),
    ];
    for n in 0..calls {
        events.push(next(ChatEvent::ItemStarted {
            item: Item {
                id: format!("perf-{n}"),
                turn_id: Some("perf".into()),
                status: if n + 1 == calls {
                    ItemStatus::InProgress
                } else {
                    ItemStatus::Completed
                },
                presentation: Presentation::default(),
                started_at: None,
                ended_at: None,
                body: ItemBody::Command {
                    command: "cat src/main.rs".into(),
                    cwd: None,
                    output: "bounded output line\n".repeat(3500),
                    exit_code: (n + 1 != calls).then_some(0),
                },
            },
        }));
    }
    events
}

fn delta(seq: u64, item: &str, delta: Delta) -> Envelope {
    Envelope {
        chat_id: "perf-chat".into(),
        seq,
        event: ChatEvent::ItemDelta {
            item_id: item.into(),
            delta,
        },
    }
}

fn bodies_in(delivery: &ChatDelivery) -> usize {
    delivery.bodies.len()
}

#[test]
fn a_2200_call_disclosed_turn_in_normal_mode_materializes_under_100_bodies_and_never_reprojects_tokens()
 {
    let events = large_turn(2200);
    let mut view = ChatView::new("perf-chat", DisplayMode::Normal, system_clock());
    view.apply(&events);
    // Disclose the turn: the activity line, then every group inside it.
    view.toggle("activity:perf");
    let groups: Vec<String> = view
        .rows()
        .iter()
        .filter(|row| row.kind == "group")
        .map(|row| row.id.clone())
        .collect();
    for group in groups {
        view.toggle(&group);
    }
    let reset = view.reset();
    let rows = reset.rows.as_ref().unwrap();
    assert!(
        rows.len() >= 2200,
        "the disclosed turn lists every call: {}",
        rows.len()
    );
    assert!(
        bodies_in(&reset) < 100,
        "reset bodies {}",
        bodies_in(&reset)
    );
    // Scroll to the middle: only rows near the viewport get bodies.
    let built = view.stats.bodies_built;
    view.set_viewport(1000, 1030);
    let patch = view.take_patch().unwrap_or_default();
    assert!(
        bodies_in(&patch) < 100,
        "viewport bodies {}",
        bodies_in(&patch)
    );
    assert!(
        view.stats.bodies_built - built < 100,
        "built {} bodies for one viewport",
        view.stats.bodies_built - built
    );
    // Back to the tail and stream into the running call's open body.
    view.toggle("perf-2199");
    let at = view
        .rows()
        .iter()
        .position(|row| row.id == "perf-2199")
        .unwrap();
    view.set_viewport(at.saturating_sub(20), at);
    let _ = view.take_patch();
    let runs = view.stats.projection_runs;
    let mut next_seq = view.last_seq() + 1;
    let started = Instant::now();
    let mut appended = 0;
    for _ in 0..120 {
        view.apply(&[delta(next_seq, "perf-2199", Delta::Output("token ".into()))]);
        next_seq += 1;
        if let Some(patch) = view.take_patch() {
            appended += patch.append.len();
            assert!(
                patch.upsert.is_empty() && patch.order.is_none(),
                "{patch:?}"
            );
            // The bridge's cost includes making the JS payload.
            serde_json::to_value(&patch).unwrap();
        }
    }
    let per_event = started.elapsed().as_secs_f64() * 1000.0 / 120.0;
    assert_eq!(
        view.stats.projection_runs, runs,
        "tokens reprojected history"
    );
    assert_eq!(appended, 120);
    eprintln!("chat bridge: stream event {per_event:.4} ms (Normal, 2,200 calls disclosed)");
    assert!(
        per_event < 1.5,
        "stream event {per_event:.3} ms exceeded 1.5 ms"
    );
}

#[test]
fn verbose_expand_all_on_2200_calls_keeps_bodies_windowed_and_markdown_streams_as_tail_splices() {
    let mut events = large_turn(2200);
    let seq = events.len() as u64;
    events.push(Envelope {
        chat_id: "perf-chat".into(),
        seq: seq + 1,
        event: ChatEvent::ItemStarted {
            item: Item {
                id: "answer".into(),
                turn_id: Some("perf".into()),
                status: ItemStatus::InProgress,
                presentation: Presentation::default(),
                started_at: None,
                ended_at: None,
                body: ItemBody::AgentMessage {
                    text: "# Findings\n\n".into(),
                },
            },
        },
    });
    let mut view = ChatView::new("perf-chat", DisplayMode::Verbose, system_clock());
    view.apply(&events);
    view.expand_all(true);
    let reset = view.reset();
    let rows = reset.rows.as_ref().unwrap();
    assert!(rows.iter().filter(|row| row.open && row.has_body).count() >= 2199);
    assert!(bodies_in(&reset) < 100);
    let built = view.stats.bodies_built;
    for first in [200, 900, 1600] {
        view.set_viewport(first, first + 30);
        let patch = view.take_patch().unwrap_or_default();
        assert!(
            bodies_in(&patch) <= 30 + 2 * OVERSCAN + 1,
            "{}",
            bodies_in(&patch)
        );
    }
    assert!(view.stats.bodies_built - built < 300);
    // Follow the tail and stream Markdown: each delta is an O(tail) splice.
    let len = view.rows().len();
    view.set_viewport(len - 20, len - 1);
    let _ = view.take_patch();
    let runs = view.stats.projection_runs;
    let mut seq = view.last_seq() + 1;
    let started = Instant::now();
    for n in 0..120 {
        let text = if n % 10 == 9 {
            "line\n\n".to_owned()
        } else {
            format!("word{n} ")
        };
        view.apply(&[delta(seq, "answer", Delta::Text(text))]);
        seq += 1;
        let patch = view.take_patch().expect("a delta in view is delivered");
        assert!(patch.bodies.is_empty(), "the message was re-sent whole");
        assert_eq!(patch.splice.len(), 1);
        assert!(patch.splice[0].blocks.len() <= 2, "{:?}", patch.splice[0]);
        serde_json::to_value(&patch).unwrap();
    }
    let per_event = started.elapsed().as_secs_f64() * 1000.0 / 120.0;
    assert_eq!(
        view.stats.projection_runs, runs,
        "tokens reprojected history"
    );
    eprintln!("chat bridge: markdown stream event {per_event:.4} ms (Verbose, 2,200 open)");
    assert!(
        per_event < 1.5,
        "stream event {per_event:.3} ms exceeded 1.5 ms"
    );
}

#[test]
fn a_toggle_reprojects_once_and_reuses_cached_turns() {
    let events = large_turn(2200);
    let mut view = ChatView::new("perf-chat", DisplayMode::Normal, system_clock());
    view.apply(&events);
    let _ = view.reset();
    let runs = view.stats.projection_runs;
    let started = Instant::now();
    view.toggle("activity:perf");
    let elapsed = started.elapsed();
    assert_eq!(view.stats.projection_runs, runs + 1);
    let patch = view.take_patch().unwrap();
    assert!(patch.order.is_some() && !patch.upsert.is_empty());
    eprintln!(
        "chat bridge: Normal activity toggle on 2,200 calls {:.2} ms (debug, reported)",
        elapsed.as_secs_f64() * 1000.0
    );
}

#[allow(dead_code)]
fn _unused(_: &Path, _: ChatState) {}

#[test]
fn a_replayed_2200_call_turn_reaches_the_subscriber_with_bounded_bodies() {
    let (host, chat) = replay_chat("large_turn");
    let handle = open(&host, &chat);
    let receiver = handle.subscribe_channel();
    let mut mirror = Mirror::default();
    mirror.until(&receiver, "idle", |m| m.panel().state == "idle");
    assert_eq!(submit(&handle, "read everything", 1).status, "submitted");
    mirror.until(&receiver, "the answer", |m| {
        m.panel().state == "idle"
            && m.rows.iter().any(|row| row.kind == "final")
            && m.rows
                .iter()
                .any(|row| row.kind == "activity" && !row.running)
    });
    let activity = mirror
        .rows
        .iter()
        .find(|row| row.kind == "activity")
        .unwrap()
        .id
        .clone();
    handle.post(WorkerMsg::Toggle(activity));
    mirror.until(&receiver, "the disclosed activity", |m| {
        m.rows.iter().any(|row| row.kind == "activity" && row.open)
    });
    let groups: Vec<_> = mirror
        .rows
        .iter()
        .filter(|row| row.kind == "group" && !row.open)
        .map(|row| row.id.clone())
        .collect();
    for group in groups {
        handle.post(WorkerMsg::Toggle(group));
    }
    mirror.until(&receiver, "every call listed", |m| {
        m.rows
            .iter()
            .filter(|row| row.kind == "shell" || row.kind == "read")
            .count()
            >= fixture::LARGE_TURN_CALLS
    });
    assert!(
        mirror.bodies.len() < 100,
        "{} bodies materialized for {} rows",
        mirror.bodies.len(),
        mirror.rows.len()
    );
    handle.post(WorkerMsg::Viewport(1000, 1030));
    let (sync, done) = std::sync::mpsc::channel();
    handle.post(WorkerMsg::Stats(sync));
    let stats = done.recv_timeout(WAIT).unwrap();
    assert!(stats.bodies_built < 200, "{stats:?}");
    std::thread::sleep(Duration::from_millis(50));
    while let Some(delivery) = recv_timeout(&receiver, Duration::from_millis(100)) {
        mirror.apply(delivery);
    }
    assert!(
        mirror.bodies.len() < 100,
        "{} bodies after scrolling",
        mirror.bodies.len()
    );
    assert_mirror_matches(&handle, &mirror);
    drop(handle);
    drop(host);
    record_saw_no_mismatch("large_turn");
}
