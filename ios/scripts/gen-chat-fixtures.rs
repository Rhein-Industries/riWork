//! Writes `ios/Tests/Fixtures/chat-serde.json`: chat events, commands and infos in the exact serde form of `src/chat/model.rs`, made by
//! the real types, so the phone's decoder is tested against what the desktop really sends (and its encoder against what it expects).
//! Run `ios/scripts/gen-chat-fixtures.sh`, never by hand; the output is checked in and `ios/Tests/ChatTests.swift` reads it.
#![allow(dead_code)]
#[path = "../../src/chat/model.rs"]
mod model;
/// `model.rs` names the attachment type of `SendAttachments`, which reads files; no fixture has one, so a stand-in will do.
mod attachments {
    #[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
    pub struct Attachment {}
}
use model::*;
use serde_json::{json, to_value, Value};

fn main() {
    let info = ChatInfo {
        id: "11111111-1111-4111-8111-111111111111".into(),
        provider: Provider::Codex,
        project_id: Some("22222222-2222-4222-8222-222222222222".into()),
        worktree_id: None,
        cwd: "/Users/me/proj".into(),
        title: "Codex chat".into(),
        created_at_unix: 1790000000,
        provider_thread_id: Some("thread-1".into()),
        model: Some("gpt-5".into()),
        effort: Some("high".into()),
        fast: true,
        approval_mode: ApprovalMode::AutoEdit,
        codex_account_id: Some("acct".into()),
        orchestrator: None,
        state: ChatState::Failed { message: "gone".into() },
        carried_over: None,
    };
    let minimal = ChatInfo {
        id: "33333333-3333-4333-8333-333333333333".into(),
        provider: Provider::Claude,
        project_id: None,
        worktree_id: Some("44444444-4444-4444-8444-444444444444".into()),
        cwd: "/c".into(),
        title: "t".into(),
        created_at_unix: 1,
        provider_thread_id: None,
        model: None,
        effort: None,
        fast: false,
        approval_mode: ApprovalMode::default(),
        codex_account_id: None,
        orchestrator: None,
        state: ChatState::default(),
        carried_over: None,
    };
    // A chat that went on with the other provider (`switch`): the same id, the new provider, and what it was given.
    let switched = ChatInfo {
        provider: Provider::Claude,
        provider_thread_id: None,
        model: Some("opus".into()),
        effort: None,
        fast: false,
        title: "Claude chat".into(),
        state: ChatState::Idle,
        carried_over: Some(CarriedOver {
            document: "/Users/me/.riwork/chats/11111111-1111-4111-8111-111111111111/context.md".into(),
            from: "Codex chat \"Codex chat\" (11111111)".into(),
        }),
        ..info.clone()
    };
    // What a driver sends once after its handshake: a model with efforts and Fast, one with efforts only, and one with nothing to choose
    // (only the id and the name are required, so the last is the shortest a model can be).
    let model_options = vec![
        ModelOption {
            id: "gpt-5.5".into(),
            name: "GPT-5.5".into(),
            description: "Frontier model for coding and agents".into(),
            efforts: vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()],
            default_effort: Some("medium".into()),
            supports_fast: true,
            is_default: true,
        },
        ModelOption {
            id: "gpt-5.4-mini".into(),
            name: "GPT-5.4 mini".into(),
            description: "Faster, cheaper".into(),
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            default_effort: Some("low".into()),
            supports_fast: false,
            is_default: false,
        },
        ModelOption { id: "bare".into(), name: "Bare".into(), ..ModelOption::default() },
    ];
    let item = |id: &str, body: ItemBody, status: ItemStatus| Item { presentation: Default::default(), id: id.into(), turn_id: Some("t1".into()), status, body };
    let step = |text: &str, status: StepStatus| Step { text: text.into(), status };
    let events: Vec<ChatEvent> = vec![
        ChatEvent::Info { info: info.clone() },
        ChatEvent::State { state: ChatState::Starting },
        ChatEvent::State { state: ChatState::Idle },
        ChatEvent::State { state: ChatState::Running },
        ChatEvent::State { state: ChatState::Waiting },
        ChatEvent::State { state: ChatState::Stopped },
        ChatEvent::State { state: ChatState::Failed { message: "gone".into() } },
        ChatEvent::TurnStarted { turn_id: "t1".into() },
        ChatEvent::TurnCompleted { turn_id: "t1".into(), outcome: TurnOutcome::Completed },
        ChatEvent::TurnCompleted { turn_id: "t1".into(), outcome: TurnOutcome::Interrupted },
        ChatEvent::TurnCompleted { turn_id: "t1".into(), outcome: TurnOutcome::Failed { message: "boom".into() } },
        ChatEvent::ItemStarted { item: item("u", ItemBody::UserMessage { text: "hi".into() }, ItemStatus::Completed) },
        ChatEvent::ItemStarted { item: item("a", ItemBody::AgentMessage { text: String::new() }, ItemStatus::InProgress) },
        ChatEvent::ItemDelta { item_id: "a".into(), delta: Delta::Text("Hel".into()) },
        ChatEvent::ItemDelta { item_id: "c".into(), delta: Delta::Output("a\n".into()) },
        ChatEvent::ItemCompleted { item: item("r", ItemBody::Reasoning { text: "thinking".into() }, ItemStatus::Completed) },
        ChatEvent::ItemCompleted { item: item("p", ItemBody::Plan { explanation: None, steps: vec![step("one", StepStatus::InProgress), step("two", StepStatus::Pending), step("three", StepStatus::Completed)] }, ItemStatus::Completed) },
        ChatEvent::ItemCompleted { item: item("p2", ItemBody::Plan { explanation: Some("why".into()), steps: vec![] }, ItemStatus::Completed) },
        ChatEvent::ItemStarted { item: item("c", ItemBody::Command { command: "ls -la".into(), cwd: Some("/tmp".into()), output: String::new(), exit_code: None }, ItemStatus::InProgress) },
        ChatEvent::ItemCompleted { item: item("c2", ItemBody::Command { command: "false".into(), cwd: None, output: "x".into(), exit_code: Some(1) }, ItemStatus::Failed) },
        ChatEvent::ItemCompleted { item: item("f", ItemBody::FileChange { changes: vec![
            FileChange { path: "src/a.rs".into(), kind: ChangeKind::Modify, diff: Some("@@ -1 +1 @@\n-a\n+b\n".into()) },
            FileChange { path: "new.txt".into(), kind: ChangeKind::Add, diff: None },
            FileChange { path: "old.txt".into(), kind: ChangeKind::Delete, diff: None },
            FileChange { path: "moved.txt".into(), kind: ChangeKind::Rename, diff: None },
        ] }, ItemStatus::Completed) },
        ChatEvent::ItemCompleted { item: item("t", ItemBody::ToolCall { server: Some("cua".into()), tool: "click".into(), input: json!({"x": 1, "y": [2, 3], "label": "ok"}), output: Some("done".into()) }, ItemStatus::Completed) },
        ChatEvent::ItemCompleted { item: item("t2", ItemBody::ToolCall { server: None, tool: "Read".into(), input: Value::Null, output: None }, ItemStatus::Declined) },
        ChatEvent::ItemCompleted { item: item("w", ItemBody::WebSearch { query: "rust serde".into() }, ItemStatus::Completed) },
        ChatEvent::ItemCompleted { item: item("d", ItemBody::Todo { items: vec![step("x", StepStatus::Completed), step("y", StepStatus::Pending)] }, ItemStatus::Completed) },
        ChatEvent::ItemCompleted { item: item("k", ItemBody::Compaction, ItemStatus::Completed) },
        ChatEvent::ItemCompleted { item: item("n1", ItemBody::Notice { level: NoticeLevel::Info, text: "fyi".into() }, ItemStatus::Completed) },
        ChatEvent::ItemCompleted { item: item("n2", ItemBody::Notice { level: NoticeLevel::Warning, text: "careful".into() }, ItemStatus::Interrupted) },
        ChatEvent::ItemCompleted { item: item("n3", ItemBody::Notice { level: NoticeLevel::Error, text: "bad".into() }, ItemStatus::Failed) },
        ChatEvent::ApprovalRequested { approval: Approval { request_id: "r1".into(), item_id: Some("c".into()), kind: ApprovalKind::Command, title: "rm -rf build".into(), detail: "why".into(), choices: vec![Decision::Accept, Decision::AcceptForSession, Decision::Decline, Decision::Cancel] } },
        ChatEvent::ApprovalRequested { approval: Approval { request_id: "r2".into(), item_id: None, kind: ApprovalKind::FileChange, title: "a.rs".into(), detail: String::new(), choices: vec![Decision::Accept, Decision::Decline] } },
        ChatEvent::ApprovalRequested { approval: Approval { request_id: "r3".into(), item_id: None, kind: ApprovalKind::Permissions, title: "network".into(), detail: String::new(), choices: vec![Decision::Accept] } },
        ChatEvent::ApprovalRequested { approval: Approval { request_id: "r4".into(), item_id: None, kind: ApprovalKind::Tool, title: "Bash".into(), detail: String::new(), choices: vec![Decision::Accept, Decision::Decline] } },
        ChatEvent::ApprovalResolved { request_id: "r1".into(), decision: Decision::AcceptForSession },
        ChatEvent::QuestionRequested { question: Question { request_id: "q1".into(), questions: vec![
            QuestionPrompt { header: Some("Pick".into()), question: "Which?".into(), options: vec![QuestionOption { label: "A".into(), description: "first".into() }, QuestionOption { label: "B".into(), description: String::new() }], multi_select: true },
            QuestionPrompt { header: None, question: "Name?".into(), options: vec![], multi_select: false },
        ] } },
        ChatEvent::QuestionResolved { request_id: "q1".into() },
        ChatEvent::Usage { usage: Usage { input_tokens: 1200, output_tokens: 300, cached_input_tokens: 100, context_window: Some(200000), context_used: Some(84000), cost_usd: Some(0.4234) } },
        ChatEvent::Usage { usage: Usage::default() },
        ChatEvent::Info { info: minimal.clone() },
        ChatEvent::Models { models: model_options.clone() },
        ChatEvent::Models { models: vec![model_options[2].clone()] },
        ChatEvent::Models { models: Vec::new() },
        ChatEvent::Info { info: switched.clone() },
        ChatEvent::ItemCompleted { item: Item { presentation: Default::default(), id: "switch-58".into(), turn_id: None, status: ItemStatus::Completed, body: ItemBody::Notice { level: NoticeLevel::Info, text: "Continued with Claude (opus), which has the conversation so far.".into() } } },
    ];
    let commands: Vec<ChatCommand> = vec![
        ChatCommand::Send { text: "go".into() },
        ChatCommand::Interrupt,
        ChatCommand::Approve { request_id: "r".into(), decision: Decision::AcceptForSession },
        ChatCommand::Approve { request_id: "r".into(), decision: Decision::Cancel },
        ChatCommand::Answer { request_id: "q".into(), answers: vec![vec!["A".into(), "B".into()], vec!["free".into()]] },
        ChatCommand::Configure { model: None, effort: None, approval_mode: Some(ApprovalMode::Plan), fast: None },
        ChatCommand::Configure { model: Some("m".into()), effort: Some("high".into()), approval_mode: None, fast: None },
        ChatCommand::Configure { model: None, effort: None, approval_mode: None, fast: None },
        ChatCommand::Configure { model: None, effort: None, approval_mode: None, fast: Some(true) },
        ChatCommand::Configure { model: None, effort: None, approval_mode: None, fast: Some(false) },
        ChatCommand::Configure { model: Some("gpt-5.5".into()), effort: Some("xhigh".into()), approval_mode: Some(ApprovalMode::Full), fast: Some(true) },
        ChatCommand::Compact,
        ChatCommand::Stop,
        ChatCommand::Switch { provider: Provider::Claude, model: None, effort: None, fast: None },
        ChatCommand::Switch { provider: Provider::Codex, model: Some("gpt-5.5".into()), effort: Some("high".into()), fast: Some(false) },
    ];
    let events: Vec<Value> = events.iter().map(|e| to_value(e).unwrap()).collect();
    let commands: Vec<Value> = commands.iter().map(|c| to_value(c).unwrap()).collect();
    let modes: Vec<Value> = [ApprovalMode::Supervised, ApprovalMode::AutoEdit, ApprovalMode::Full, ApprovalMode::Plan].iter().map(|m| to_value(m).unwrap()).collect();
    let providers: Vec<Value> = [Provider::Codex, Provider::Claude].iter().map(|p| to_value(p).unwrap()).collect();
    let doc = json!({
        "generated_by": "ios/scripts/gen-chat-fixtures.sh from src/chat/model.rs; do not edit by hand",
        "events": events,
        "commands": commands,
        "chats": [to_value(&info).unwrap(), to_value(&minimal).unwrap(), to_value(&switched).unwrap()],
        "modes": modes,
        "providers": providers,
        "model_options": model_options.iter().map(|m| to_value(m).unwrap()).collect::<Vec<Value>>(),
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}
