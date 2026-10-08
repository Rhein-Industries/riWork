//! The summary flow with a fake agent, then with a chat on the in-process chat host, then
//! with a Claude in a real tmux pane that a script plays.

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::*;
use crate::{
    chat::{
        model::{ChatCommand, Provider},
        testing::{TestHost, eventually, short_home},
    },
    handoff::testing::Tmux,
};

fn quick(wait_ms: u64) -> Timing {
    Timing {
        wait: Duration::from_millis(wait_ms),
        poll: Duration::from_millis(10),
        grace: Duration::from_millis(150),
    }
}

/// An agent that writes `file` and finishes its turn when its script says, measured from
/// the moment it was asked.
struct FakeAgent {
    ready: Result<(), String>,
    file: PathBuf,
    content: String,
    write_after: Option<Duration>,
    end_after: Option<Duration>,
    fail_after: Option<Duration>,
    asked: Option<Instant>,
    messages: Arc<Mutex<Vec<String>>>,
}

impl FakeAgent {
    fn new(file: &Path) -> Self {
        Self {
            ready: Ok(()),
            file: file.to_owned(),
            content: "## Goal\nShip the handoff.\n".into(),
            write_after: Some(Duration::from_millis(30)),
            end_after: Some(Duration::from_millis(60)),
            fail_after: None,
            asked: None,
            messages: Arc::default(),
        }
    }
}

impl Agent for FakeAgent {
    fn ready(&mut self) -> Result<(), String> {
        self.ready.clone()
    }

    fn ask(&mut self, message: &str) -> Result<(), String> {
        self.messages.lock().unwrap().push(message.to_owned());
        self.asked = Some(Instant::now());
        Ok(())
    }

    fn turn_ended(&mut self, wait: Duration) -> Result<bool, String> {
        std::thread::sleep(wait);
        let elapsed = self.asked.expect("asked first").elapsed();
        if self.write_after.is_some_and(|after| elapsed >= after) && !self.file.exists() {
            fs::write(&self.file, &self.content).unwrap();
        }
        if self.fail_after.is_some_and(|after| elapsed >= after) {
            return Err("the agent went away".into());
        }
        Ok(self.end_after.is_some_and(|after| elapsed >= after))
    }
}

fn file_in(dir: &Path) -> PathBuf {
    dir.join("summary.md")
}

fn written(collected: Collected) -> String {
    match collected {
        Collected::Written(text) => text,
        Collected::Missing(reason) => panic!("no summary: {reason}"),
    }
}

fn missing(collected: Collected) -> String {
    match collected {
        Collected::Missing(reason) => reason,
        Collected::Written(text) => panic!("a summary: {text}"),
    }
}

#[test]
fn the_summary_is_used_once_the_file_is_there_and_the_turn_has_ended() {
    let dir = short_home();
    let file = file_in(&dir);
    let mut agent = FakeAgent::new(&file);
    let messages = agent.messages.clone();
    let text = written(collect(&mut agent, &file, quick(5_000)).unwrap());
    assert_eq!(text, "## Goal\nShip the handoff.\n");
    // It was asked once, in one line, naming the file and what to cover.
    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(!messages[0].contains('\n'));
    assert!(messages[0].contains(&file.display().to_string()));
    for covered in [
        "goal",
        "current state",
        "decisions",
        "open tasks",
        "files touched",
        "next steps",
    ] {
        assert!(messages[0].contains(covered), "{covered}");
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_file_that_is_there_before_the_turn_ends_waits_for_the_end() {
    let dir = short_home();
    let file = file_in(&dir);
    let mut agent = FakeAgent::new(&file);
    agent.write_after = Some(Duration::from_millis(10));
    agent.end_after = Some(Duration::from_millis(400));
    let started = Instant::now();
    written(collect(&mut agent, &file, quick(5_000)).unwrap());
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn no_summary_in_time_is_a_missing_summary_not_an_error() {
    let dir = short_home();
    let file = file_in(&dir);
    let mut agent = FakeAgent::new(&file);
    (agent.write_after, agent.end_after) = (None, None);
    let started = Instant::now();
    let reason = missing(collect(&mut agent, &file, quick(300)).unwrap());
    assert_eq!(reason, "no summary after 1 seconds");
    assert!(started.elapsed() >= Duration::from_millis(300));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_turn_that_ends_without_a_file_does_not_wait_for_the_whole_time() {
    let dir = short_home();
    let file = file_in(&dir);
    let mut agent = FakeAgent::new(&file);
    agent.write_after = None;
    let started = Instant::now();
    let reason = missing(collect(&mut agent, &file, quick(60_000)).unwrap());
    assert!(
        reason.starts_with("the agent's turn ended without a summary at "),
        "{reason}"
    );
    assert!(reason.ends_with(&file.display().to_string()));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_empty_file_is_not_a_summary_and_an_old_one_is_not_this_ones() {
    let dir = short_home();
    let file = file_in(&dir);
    let mut agent = FakeAgent::new(&file);
    agent.content = " \n\n".into();
    assert!(matches!(
        collect(&mut agent, &file, quick(2_000)).unwrap(),
        Collected::Missing(_)
    ));
    // A file left by an earlier attempt is removed before the agent is asked.
    fs::write(&file, "an old summary").unwrap();
    let mut agent = FakeAgent::new(&file);
    agent.write_after = None;
    assert!(matches!(
        collect(&mut agent, &file, quick(2_000)).unwrap(),
        Collected::Missing(_)
    ));
    assert!(!file.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_agent_that_is_not_idle_is_never_asked() {
    let dir = short_home();
    let file = file_in(&dir);
    let mut agent = FakeAgent::new(&file);
    agent.ready = Err("The agent is busy.".into());
    let messages = agent.messages.clone();
    let error = collect(&mut agent, &file, quick(1_000)).err().unwrap();
    assert_eq!(error, "The agent is busy.");
    assert!(messages.lock().unwrap().is_empty());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_agent_that_goes_away_after_it_was_asked_leaves_the_transcript_to_fall_back_on() {
    let dir = short_home();
    let file = file_in(&dir);
    let mut agent = FakeAgent::new(&file);
    (agent.write_after, agent.end_after) = (None, None);
    agent.fail_after = Some(Duration::from_millis(30));
    assert_eq!(
        missing(collect(&mut agent, &file, quick(5_000)).unwrap()),
        "the agent went away"
    );
    fs::remove_dir_all(dir).unwrap();
}

// ---- A chat on the chat host ---------------------------------------------------------------

#[test]
fn a_chat_that_works_or_is_stopped_is_refused_before_anything_is_sent() {
    let host = TestHost::new();
    let file = host.home.join("summary.md");
    let timing = quick(1_000);

    let busy = host.create(Provider::Codex);
    host.wait_for_state(&busy.id, |state| *state == ChatState::Idle);
    host.client()
        .command(
            &busy.id,
            ChatCommand::Send {
                text: "hang".into(),
            },
        )
        .unwrap();
    host.wait_for_state(&busy.id, |state| *state == ChatState::Running);
    let mut agent = ChatAgent::new(host.home.clone(), host.socket(), busy.id.clone());
    let error = collect(&mut agent, &file, timing).err().unwrap();
    assert!(error.contains("The chat is busy"), "{error}");
    // Only the message that kept it busy reached the driver.
    assert_eq!(host.fake().commands().len(), 1);

    let stopped = host.create_in("other", Provider::Codex);
    host.client().close(&stopped.id).unwrap();
    eventually(|| host.info(&stopped.id).state == ChatState::Stopped);
    let mut agent = ChatAgent::new(host.home.clone(), host.socket(), stopped.id.clone());
    let error = collect(&mut agent, &file, timing).err().unwrap();
    assert!(error.contains("not running"), "{error}");
    assert!(
        crate::chat::testing::fake_for(&stopped.cwd)
            .commands()
            .is_empty()
    );

    let error = collect(
        &mut ChatAgent::new(
            host.home.clone(),
            host.socket(),
            uuid::Uuid::new_v4().to_string(),
        ),
        &file,
        timing,
    )
    .err()
    .unwrap();
    assert!(error.contains("does not know this chat"), "{error}");
}

// ---- A Claude in a terminal ----------------------------------------------------------------

/// What a Claude does, as far as the flow sees it: an empty prompt, a hook cursor that
/// says whether a turn is open, and a summary written for a request typed at the prompt.
const PLAY_CLAUDE: &str = r#"
dir="$RIWORK_HOME/agent-hooks/claude"; mkdir -p "$dir"
cursor="$dir/$RIWORK_SHELL_ID.json"
state() { printf '{"session_id":"session-1","turn_id":"%s","completed":%s}\n' "$1" "$2" > "$cursor"; }
state turn-1 true
printf 'claude ready\n'
while true; do
  printf '❯ '
  IFS= read -r line || exit
  printf '%s\n' "$line" >> "$RIWORK_HOME/received"
  state turn-2 false
  file=$(printf '%s' "$line" | sed -n 's/.* to \(\/[^ ]*\) (a Markdown.*/\1/p')
  sleep 0.3
  printf '## Goal\nPlayed by a script.\n' > "$file"
  state turn-2 true
done
"#;

#[test]
#[ignore = "slow: real tmux pane played by a script"]
fn a_claude_terminal_at_its_prompt_is_asked_the_way_a_schedule_asks() {
    let Some(tmux) = Tmux::new() else {
        return;
    };
    let script = tmux.home.join("claude.sh");
    fs::write(&script, PLAY_CLAUDE).unwrap();
    let mut shell = tmux.shell(&format!("exec sh {}", script.display()), "claude ready");
    shell.harness = Some(HarnessKind::Claude);
    // Let the prompt be drawn, then ask.
    std::thread::sleep(Duration::from_millis(300));
    let file = tmux.home.join("summary.md");
    let timing = Timing {
        wait: Duration::from_secs(30),
        poll: Duration::from_millis(100),
        grace: Duration::from_secs(5),
    };
    let mut agent = TerminalAgent::new(tmux.manager.clone(), shell.clone());
    let text = written(collect(&mut agent, &file, timing).unwrap());
    assert_eq!(text, "## Goal\nPlayed by a script.\n");
    // What reached the terminal is the request, whole.
    let received = fs::read_to_string(tmux.home.join("received")).unwrap();
    assert_eq!(received.trim_end(), request(&file));
}

#[test]
#[ignore = "slow: real tmux pane"]
fn a_terminal_that_is_not_idle_or_not_an_agent_is_refused() {
    let Some(tmux) = Tmux::new() else {
        return;
    };
    let plain = tmux.shell("echo plain-ready; exec sleep 300", "plain-ready");
    let file = tmux.home.join("summary.md");
    // A plain shell has no one to ask.
    let error = collect(
        &mut TerminalAgent::new(tmux.manager.clone(), plain.clone()),
        &file,
        quick(1_000),
    )
    .err()
    .unwrap();
    assert!(error.contains("Only a Codex or Claude terminal"), "{error}");
    // A Claude whose turn is open (no completed turn to be idle after) is busy.
    let mut claude = plain.clone();
    claude.harness = Some(HarnessKind::Claude);
    let directory = tmux.home.join("agent-hooks/claude");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join(format!("{}.json", claude.id)),
        r#"{"session_id":"session-1","turn_id":"turn-1","completed":false}"#,
    )
    .unwrap();
    let error = collect(
        &mut TerminalAgent::new(tmux.manager.clone(), claude.clone()),
        &file,
        quick(1_000),
    )
    .err()
    .unwrap();
    assert!(error.contains("The agent is busy"), "{error}");
    assert!(!file.exists());
}
