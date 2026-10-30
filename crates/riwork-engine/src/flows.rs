//! Running a flow (riwork_core::flows) in a chat: where the agent is, how it moves on, and
//! what it is told.
//!
//! A chat has at most one active run, kept as `RIWORK_HOME/flows/runs/<run>.json`. The flow is
//! read from the project's file on every step, so editing the file changes the run (a state the
//! run is in must stay). The agent moves on with `riwork flow next EVENT [--evidence TEXT]`
//! in its shell (RIWORK_CHAT_ID says which chat); the command runs the transition's check and
//! answers with the next step's instructions, or with why it cannot move. A transition that
//! needs approval waits for the person, in RiWork; their decision, and starting or stopping a
//! run, reach the agent as a message in its chat.
use std::{
    fs,
    io::Read,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use riwork_core::flows::{self, DEFAULT_CHECK_SECONDS, Flow, MAX_CHECK_SECONDS};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The agent works in a state.
    Active,
    /// A transition waits for the person's approval.
    Waiting,
    /// A final state was reached.
    Completed,
    Stopped,
}

impl Status {
    pub fn live(self) -> bool {
        matches!(self, Self::Active | Self::Waiting)
    }
}

/// A transition waiting for approval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub event: String,
    pub target: String,
    #[serde(default)]
    pub evidence: String,
    pub asked_at: u64,
}

/// One thing that happened in a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub at: u64,
    /// `start`, `moved`, `check_failed`, `asked`, `approved`, `denied`, `stopped`.
    pub kind: String,
    pub from: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub evidence: String,
    /// A check's command output, or the person's note.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// `agent` or `person`.
    pub by: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub flow_id: String,
    pub flow_name: String,
    pub project_id: String,
    pub root: PathBuf,
    pub chat_id: String,
    #[serde(default)]
    pub chat_title: String,
    /// Where checks run: the chat's folder.
    pub cwd: PathBuf,
    /// The leaf state the agent is in.
    pub state: String,
    pub status: Status,
    #[serde(default)]
    pub pending: Option<Pending>,
    #[serde(default)]
    pub history: Vec<Step>,
    pub started_at: u64,
    pub updated_at: u64,
}

/// The chat a run is started in.
#[derive(Clone, Debug)]
pub struct ChatRef {
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
}

/// How a message reaches a chat: the chat host in the app, a recorder in tests.
pub type Deliver<'a> = &'a dyn Fn(&str, &str) -> Result<(), String>;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_secs()).unwrap_or(0)
}

/// Where runs are kept: one JSON file each.
pub fn runs_dir(home: &Path) -> PathBuf {
    home.join("flows/runs")
}

fn lock(home: &Path) -> Result<fs::File, String> {
    use fs2::FileExt;
    let dir = runs_dir(home);
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let file = fs::File::create(dir.join(".lock")).map_err(|error| error.to_string())?;
    file.lock_exclusive().map_err(|error| error.to_string())?;
    Ok(file)
}

fn save(home: &Path, run: &Run) -> Result<(), String> {
    let path = runs_dir(home).join(format!("{}.json", run.id));
    let staged = path.with_extension("staged");
    fs::write(&staged, serde_json::to_vec_pretty(run).map_err(|error| error.to_string())?)
        .and_then(|_| fs::rename(&staged, &path))
        .map_err(|error| error.to_string())
}

/// Every run on this Mac, newest first.
pub fn runs(home: &Path) -> Vec<Run> {
    let Ok(entries) = fs::read_dir(runs_dir(home)) else { return Vec::new() };
    let mut runs: Vec<Run> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "json"))
        .filter_map(|entry| serde_json::from_slice(&fs::read(entry.path()).ok()?).ok())
        .collect();
    runs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.id.cmp(&b.id)));
    runs
}

pub fn find_run(home: &Path, id: &str) -> Result<Run, String> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(format!("{id:?} is not a run id"));
    }
    let bytes = fs::read(runs_dir(home).join(format!("{id}.json"))).map_err(|_| format!("There is no run {id}."))?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

/// The chat's active (or waiting) run.
pub fn run_for_chat(home: &Path, chat_id: &str) -> Option<Run> {
    runs(home).into_iter().find(|run| run.chat_id == chat_id && run.status.live())
}

fn load_flow(run: &Run) -> Result<Flow, String> {
    let flow = flows::find(&run.root, &run.flow_id)?;
    if flow.state(&run.state).is_none() {
        return Err(format!(
            "The flow {} was changed and has no state {:?} any more. Stop this run and start the flow again.",
            run.flow_id, run.state
        ));
    }
    Ok(flow)
}

/// What the agent is told in the run's state: where it is, what to do, how to move on.
pub fn briefing(flow: &Flow, run: &Run) -> String {
    let state = &run.state;
    let mut text = if flow.is_final(state) {
        format!("**Flow: {}** · last step: **{}**", flow.name, flow.location(state))
    } else {
        format!("**Flow: {}** · step: **{}**", flow.name, flow.location(state))
    };
    for part in flow.instructions(state) {
        text.push_str("\n\n");
        text.push_str(&part);
    }
    if run.status == Status::Waiting {
        if let Some(pending) = &run.pending {
            text.push_str(&format!(
                "\n\nYou asked to move on with `{}` to **{}**. The person decides in RiWork: stop here and wait for their message.",
                pending.event,
                flow.location(&pending.target)
            ));
        }
        return text;
    }
    if flow.is_final(state) {
        text.push_str("\n\nThis is the flow's last step: when it is done, the flow is complete.");
        return text;
    }
    let events = flow.events(state);
    if events.is_empty() {
        text.push_str("\n\nThis step has no way on: ask the person how to continue.");
        return text;
    }
    text.push_str("\n\nWhen this step is done, move on by running one of these in your shell:");
    for (event, transition) in &events {
        let mut line = format!("\n- `riwork flow next {event}{}`", if transition.evidence { " --evidence \"what you did\"" } else { "" });
        line.push_str(&format!(" → {}", flow.location(&flow.enter(&transition.target).unwrap_or_else(|| transition.target.clone()))));
        if !transition.description.is_empty() {
            line.push_str(&format!(": {}", transition.description));
        }
        match &transition.check {
            Some(check) if check.approval => line.push_str(" (the person approves it)"),
            Some(check) => line.push_str(&format!(" (only when `{}` passes)", check.command.as_deref().unwrap_or_default())),
            None => {}
        }
        text.push_str(&line);
    }
    text.push_str("\n\n`riwork flow status` shows this again.");
    text
}

fn step(kind: &str, from: &str, by: &str) -> Step {
    Step { at: now(), kind: kind.into(), from: from.into(), event: String::new(), to: String::new(), evidence: String::new(), detail: String::new(), by: by.into() }
}

/// Starts `flow_id` of the project at `root` in `chat`, and tells the chat its first step.
pub fn start(home: &Path, root: &Path, project_id: &str, flow_id: &str, chat: &ChatRef, deliver: Deliver) -> Result<Run, String> {
    let flow = flows::find(root, flow_id)?;
    let state = flow.enter(&flow.initial).ok_or("The flow has no initial state.")?;
    let _lock = lock(home)?;
    if let Some(existing) = run_for_chat(home, &chat.id) {
        return Err(format!("This chat already follows the flow {}. Stop that run first.", existing.flow_name));
    }
    let at = now();
    let mut run = Run {
        id: uuid::Uuid::new_v4().to_string(), flow_id: flow_id.into(), flow_name: flow.name.clone(),
        project_id: project_id.into(), root: root.into(), chat_id: chat.id.clone(), chat_title: chat.title.clone(),
        cwd: chat.cwd.clone(), state: state.clone(), status: Status::Active, pending: None, history: Vec::new(),
        started_at: at, updated_at: at,
    };
    if flow.is_final(&state) {
        run.status = Status::Completed;
    }
    run.history.push(Step { to: state, ..step("start", "", "person") });
    save(home, &run)?;
    let intro = format!(
        "{}\n\n(RiWork leads this chat through the flow **{}**. Follow its steps in order.)",
        briefing(&flow, &run),
        flow.name
    );
    if let Err(error) = deliver(&chat.id, &intro) {
        let _ = fs::remove_file(runs_dir(home).join(format!("{}.json", run.id)));
        return Err(format!("The flow did not start: the chat could not be told ({error})."));
    }
    Ok(run)
}

/// Where the chat's run is, as the agent is told.
pub fn status(home: &Path, chat_id: &str) -> Result<String, String> {
    let run = run_for_chat(home, chat_id).ok_or("This chat follows no flow.")?;
    let flow = load_flow(&run)?;
    Ok(briefing(&flow, &run))
}

/// The tail of a check's output: enough to see why it failed.
fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Runs a check's command in `cwd`, in its own process group, within its timeout.
fn run_check(command: &str, cwd: &Path, seconds: u64) -> (bool, String) {
    let mut child = match Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return (false, format!("could not run the check: {error}")),
    };
    let readers = [child.stdout.take().map(|out| Box::new(out) as Box<dyn Read + Send>), child.stderr.take().map(|err| Box::new(err) as Box<dyn Read + Send>)]
        .into_iter()
        .flatten()
        .map(|mut stream| std::thread::spawn(move || { let mut text = String::new(); let _ = stream.read_to_string(&mut text); text }))
        .collect::<Vec<_>>();
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= Duration::from_secs(seconds) => {
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break None,
        }
    };
    let output: String = readers.into_iter().filter_map(|reader| reader.join().ok()).collect::<Vec<_>>().join("\n");
    match status {
        Some(status) => (status.success(), tail(&output, 40)),
        None => (false, format!("The check took longer than {seconds} s and was stopped.\n{}", tail(&output, 20))),
    }
}

/// The agent moves on with `event`. Answers what it is told next.
pub fn next(home: &Path, chat_id: &str, event: &str, evidence: &str) -> Result<String, String> {
    let before = run_for_chat(home, chat_id).ok_or("This chat follows no flow.")?;
    if before.status == Status::Waiting {
        let flow = load_flow(&before)?;
        return Err(briefing(&flow, &before));
    }
    let flow = load_flow(&before)?;
    let events = flow.events(&before.state);
    let Some((_, transition)) = events.iter().find(|(name, _)| name == event) else {
        let names: Vec<String> = events.iter().map(|(name, _)| format!("`{name}`")).collect();
        return Err(format!(
            "`{event}` is not a way on from **{}**. {}",
            flow.location(&before.state),
            if names.is_empty() { "This step has none.".to_owned() } else { format!("Use one of: {}.", names.join(", ")) }
        ));
    };
    let evidence = evidence.trim();
    if transition.evidence && evidence.is_empty() {
        return Err(format!("Moving on with `{event}` needs evidence: run `riwork flow next {event} --evidence \"what you did\"`."));
    }
    // A command check runs without the lock: it may take minutes.
    let checked = match transition.check.as_ref().and_then(|check| check.command.as_deref().map(|command| (command, check.timeout))) {
        Some((command, timeout)) => Some(run_check(command, &before.cwd, timeout.unwrap_or(DEFAULT_CHECK_SECONDS).min(MAX_CHECK_SECONDS))),
        None => None,
    };
    let _lock = lock(home)?;
    let mut run = run_for_chat(home, chat_id).ok_or("The run stopped meanwhile.")?;
    if run.id != before.id || run.state != before.state || run.status != Status::Active {
        return Err("The run changed while the check ran. Run `riwork flow status` to see where it is now.".into());
    }
    run.updated_at = now();
    if let Some((passed, output)) = checked {
        if !passed {
            run.history.push(Step { event: event.into(), evidence: evidence.into(), detail: output.clone(), ..step("check_failed", &run.state, "agent") });
            save(home, &run)?;
            return Err(format!(
                "Not yet: the check for `{event}` failed (`{}`). Fix what it says, then run `riwork flow next {event}` again.\n\n{output}",
                transition.check.as_ref().and_then(|check| check.command.as_deref()).unwrap_or_default()
            ));
        }
    }
    if transition.check.as_ref().is_some_and(|check| check.approval) {
        run.status = Status::Waiting;
        run.pending = Some(Pending { event: event.into(), target: transition.target.clone(), evidence: evidence.into(), asked_at: run.updated_at });
        run.history.push(Step { event: event.into(), to: transition.target.clone(), evidence: evidence.into(), ..step("asked", &run.state, "agent") });
        save(home, &run)?;
        return Ok(briefing(&flow, &run));
    }
    let from = run.state.clone();
    run.state = flow.enter(&transition.target).ok_or("The transition goes to no state.")?;
    if flow.is_final(&run.state) {
        run.status = Status::Completed;
    }
    run.history.push(Step { event: event.into(), to: run.state.clone(), evidence: evidence.into(), ..step("moved", &from, "agent") });
    save(home, &run)?;
    Ok(briefing(&flow, &run))
}

/// The person approves the transition the run waits for; the chat is told its next step.
pub fn approve(home: &Path, run_id: &str, note: &str, deliver: Deliver) -> Result<Run, String> {
    let _lock = lock(home)?;
    let mut run = find_run(home, run_id)?;
    let pending = run.pending.clone().filter(|_| run.status == Status::Waiting).ok_or("This run waits for no approval.")?;
    let flow = load_flow(&run)?;
    let from = run.state.clone();
    run.state = flow.enter(&pending.target).ok_or("The approved transition goes to no state.")?;
    run.status = if flow.is_final(&run.state) { Status::Completed } else { Status::Active };
    run.pending = None;
    run.updated_at = now();
    run.history.push(Step { event: pending.event.clone(), to: run.state.clone(), detail: note.trim().into(), ..step("approved", &from, "person") });
    save(home, &run)?;
    let note = if note.trim().is_empty() { String::new() } else { format!(" They noted: {}", note.trim()) };
    deliver(&run.chat_id, &format!("The person approved `{}`.{note}\n\n{}", pending.event, briefing(&flow, &run)))
        .map_err(|error| format!("Approved, but the chat could not be told ({error}): `riwork flow status` shows it its step."))?;
    Ok(run)
}

/// The person turns the transition down: the agent stays where it is, told why.
pub fn deny(home: &Path, run_id: &str, note: &str, deliver: Deliver) -> Result<Run, String> {
    let _lock = lock(home)?;
    let mut run = find_run(home, run_id)?;
    let pending = run.pending.clone().filter(|_| run.status == Status::Waiting).ok_or("This run waits for no approval.")?;
    let flow = load_flow(&run)?;
    run.status = Status::Active;
    run.pending = None;
    run.updated_at = now();
    run.history.push(Step { event: pending.event.clone(), to: pending.target.clone(), detail: note.trim().into(), ..step("denied", &run.state, "person") });
    save(home, &run)?;
    let why = if note.trim().is_empty() { "They gave no reason; ask them if it is unclear.".to_owned() } else { format!("Their note: {}", note.trim()) };
    deliver(&run.chat_id, &format!("The person did not approve `{}` yet. {why}\n\n{}", pending.event, briefing(&flow, &run)))
        .map_err(|error| format!("Turned down, but the chat could not be told ({error}): `riwork flow status` shows it its step."))?;
    Ok(run)
}

/// Ends the run; the chat is told it is no longer led.
pub fn stop(home: &Path, run_id: &str, deliver: Deliver) -> Result<Run, String> {
    let _lock = lock(home)?;
    let mut run = find_run(home, run_id)?;
    if !run.status.live() {
        return Ok(run);
    }
    run.status = Status::Stopped;
    run.pending = None;
    run.updated_at = now();
    run.history.push(step("stopped", &run.state, "person"));
    save(home, &run)?;
    // A chat that is gone needs no word.
    let _ = deliver(&run.chat_id, &format!("The flow **{}** was stopped: it no longer leads this chat.", run.flow_name));
    Ok(run)
}

/// The real delivery: a user message through the chat host, started if it is not running (it
/// resumes a stopped chat, starts a turn, or steers one). Blocks up to a few seconds.
pub fn deliver_to_chat(home: &Path) -> impl Fn(&str, &str) -> Result<(), String> + '_ {
    move |chat_id: &str, text: &str| {
        let socket = crate::chat::host::ensure_host(home)?;
        let mut client = crate::chat::client::Client::connect(&socket)?;
        client.command(chat_id, crate::chat::model::ChatCommand::Send { text: text.to_owned() })
    }
}

/// No delivery: for a chat that starts its own run and reads the briefing from the command.
pub fn deliver_nowhere(_: &str, _: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fixture {
        home: PathBuf,
        root: PathBuf,
        sent: RefCell<Vec<(String, String)>>,
    }

    impl Fixture {
        fn new(flow: &str) -> Self {
            let base = std::env::temp_dir().join(format!("riwork-flow-runs-{}", uuid::Uuid::new_v4()));
            let root = base.join("project");
            fs::create_dir_all(flows::project_dir(&root)).unwrap();
            fs::write(flows::project_dir(&root).join("study.json"), flow).unwrap();
            Self { home: base.join("home"), root, sent: RefCell::default() }
        }
        fn deliver(&self) -> impl Fn(&str, &str) -> Result<(), String> + '_ {
            |chat: &str, text: &str| { self.sent.borrow_mut().push((chat.into(), text.into())); Ok(()) }
        }
        fn chat(&self) -> ChatRef {
            ChatRef { id: "chat-1".into(), title: "Study".into(), cwd: self.root.clone() }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(self.root.parent().unwrap()).ok();
        }
    }

    const FLOW: &str = r#"{ "name": "Study", "initial": "plan",
      "states": {
        "plan": { "title": "Plan", "instructions": "Plan it.", "on": { "planned": { "target": "work", "check": { "approval": true } } } },
        "work": { "title": "Work", "initial": "build", "states": {
            "build": { "on": { "built": { "target": "verify", "check": { "command": "test -f done.txt", "timeout": 5 } } } },
            "verify": { "on": { "verified": { "target": "done", "evidence": true } } } },
          "on": { "abandon": { "target": "done" } } },
        "done": { "final": true, "instructions": "Report." } } }"#;

    #[test]
    fn a_run_leads_the_agent_through_checks_approvals_and_evidence() {
        let fixture = Fixture::new(FLOW);
        let deliver = fixture.deliver();
        let run = start(&fixture.home, &fixture.root, "p1", "study", &fixture.chat(), &deliver).unwrap();
        assert_eq!((run.state.as_str(), run.status), ("plan", Status::Active));
        let first = fixture.sent.borrow()[0].clone();
        assert_eq!(first.0, "chat-1");
        assert!(first.1.contains("**Flow: Study** · step: **Plan**") && first.1.contains("riwork flow next planned"), "{}", first.1);
        assert!(start(&fixture.home, &fixture.root, "p1", "study", &fixture.chat(), &deliver).unwrap_err().contains("already follows"));

        // Approval: the agent waits; the person decides.
        assert!(next(&fixture.home, "chat-1", "nope", "").unwrap_err().contains("Use one of: `planned`"));
        let asked = next(&fixture.home, "chat-1", "planned", "").unwrap();
        assert!(asked.contains("stop here and wait"), "{asked}");
        assert_eq!(run_for_chat(&fixture.home, "chat-1").unwrap().status, Status::Waiting);
        assert!(next(&fixture.home, "chat-1", "planned", "").is_err(), "waiting answers with the briefing");
        let denied = deny(&fixture.home, &run.id, "Plan the tests too.", &deliver).unwrap();
        assert_eq!((denied.state.as_str(), denied.status), ("plan", Status::Active));
        assert!(fixture.sent.borrow().last().unwrap().1.contains("Plan the tests too."));
        next(&fixture.home, "chat-1", "planned", "").unwrap();
        let approved = approve(&fixture.home, &run.id, "", &deliver).unwrap();
        // A nested state is entered at its initial child; its outer state's events apply.
        assert_eq!(approved.state, "build");
        let told = fixture.sent.borrow().last().unwrap().1.clone();
        assert!(told.contains("Work › build") && told.contains("riwork flow next built") && told.contains("riwork flow next abandon"), "{told}");

        // A command check: it fails until its file exists, and says why.
        let failed = next(&fixture.home, "chat-1", "built", "").unwrap_err();
        assert!(failed.contains("check for `built` failed"), "{failed}");
        fs::write(fixture.root.join("done.txt"), "").unwrap();
        let moved = next(&fixture.home, "chat-1", "built", "").unwrap();
        assert!(moved.contains("Work › verify"), "{moved}");

        // Evidence is required where the transition asks for it.
        assert!(next(&fixture.home, "chat-1", "verified", " ").unwrap_err().contains("needs evidence"));
        let last = next(&fixture.home, "chat-1", "verified", "All 12 cases match.").unwrap();
        assert!(last.contains("last step") && last.contains("Report."), "{last}");
        let done = find_run(&fixture.home, &run.id).unwrap();
        assert_eq!(done.status, Status::Completed);
        let kinds: Vec<&str> = done.history.iter().map(|step| step.kind.as_str()).collect();
        assert_eq!(kinds, ["start", "asked", "denied", "asked", "approved", "check_failed", "moved", "moved"]);
        assert_eq!(done.history.last().unwrap().evidence, "All 12 cases match.");
        assert!(run_for_chat(&fixture.home, "chat-1").is_none());
        assert!(status(&fixture.home, "chat-1").is_err());
    }

    #[test]
    fn a_slow_check_is_stopped_and_stopping_a_run_tells_the_chat() {
        let fixture = Fixture::new(&FLOW.replace("test -f done.txt\", \"timeout\": 5", "sleep 30\", \"timeout\": 1"));
        let deliver = fixture.deliver();
        let run = start(&fixture.home, &fixture.root, "p1", "study", &fixture.chat(), &deliver).unwrap();
        next(&fixture.home, "chat-1", "planned", "").unwrap();
        approve(&fixture.home, &run.id, "go", &deliver).unwrap();
        let started = Instant::now();
        assert!(next(&fixture.home, "chat-1", "built", "").unwrap_err().contains("longer than 1 s"));
        assert!(started.elapsed() < Duration::from_secs(10));
        let stopped = stop(&fixture.home, &run.id, &deliver).unwrap();
        assert_eq!(stopped.status, Status::Stopped);
        assert!(fixture.sent.borrow().last().unwrap().1.contains("was stopped"));
        // A new run may start once the old one stopped.
        start(&fixture.home, &fixture.root, "p1", "study", &fixture.chat(), &deliver).unwrap();
    }

    #[test]
    fn a_flow_edited_out_from_under_its_run_says_so() {
        let fixture = Fixture::new(FLOW);
        let deliver = fixture.deliver();
        start(&fixture.home, &fixture.root, "p1", "study", &fixture.chat(), &deliver).unwrap();
        fs::write(flows::project_dir(&fixture.root).join("study.json"), FLOW.replace("\"plan\"", "\"draft\"")).unwrap();
        assert!(status(&fixture.home, "chat-1").unwrap_err().contains("has no state \"plan\" any more"));
    }
}
