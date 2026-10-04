//! Asking the source agent for a summary of its own conversation.
//!
//! `--context summary` sends the live agent a message that asks it to write a handoff
//! summary to a file, and waits, for a bounded time, for the file and for the end of the
//! turn that wrote it. Which agent it is, a chat or a terminal's Codex or Claude, is only
//! how the message goes in and how the end of the turn is seen (`Agent`); the waiting is
//! the same.
//!
//! An agent that is not idle is refused up front: a message sent to a busy agent steers
//! it or queues behind its work, and an agent that hands *itself* over is busy running
//! the very command that waits. Once the message is sent, whatever goes wrong (no file in
//! time, a turn that ends without one) is not an error but a reason to read the
//! transcript instead; `Collected::Missing` says which.

use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    activity::ActivityTracker,
    chat::{
        client::{Client, Poll, Subscription},
        model::{ChatCommand, ChatEvent, ChatState},
    },
    sessions::{HarnessKind, SessionManager, ShellSession},
};

/// The most of a summary that is read.
const MAX_SUMMARY: u64 = 256 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// How long to wait for the summary after asking.
    pub wait: Duration,
    /// How often the agent is looked at.
    pub poll: Duration,
    /// How long to wait for the file once the turn has ended without it.
    pub grace: Duration,
}

impl Timing {
    pub const DEFAULT: Self = Self {
        wait: Duration::from_secs(5 * 60),
        poll: Duration::from_millis(500),
        grace: Duration::from_secs(3),
    };
}

/// What `collect` needs of the agent that is asked.
pub trait Agent {
    /// Ok if the agent is idle and can be asked now, else why not.
    fn ready(&mut self) -> Result<(), String>;
    /// Sends the message.
    fn ask(&mut self, message: &str) -> Result<(), String>;
    /// Whether the turn that `ask` started has ended, looking for up to `wait`.
    fn turn_ended(&mut self, wait: Duration) -> Result<bool, String>;
}

pub enum Collected {
    Written(String),
    /// No summary, and why.
    Missing(String),
}

/// The message that asks for the summary. One paragraph: it is typed into a terminal.
pub fn request(file: &Path) -> String {
    format!(
        "Please write a concise handoff summary of this conversation to {} (a Markdown file; create it, and change nothing else). Cover the goal, the current state, the decisions made, the open tasks, the files touched, and the next steps. When the file is written, reply with one short line.",
        file.display()
    )
}

/// Asks the agent for a summary in `file` and waits for it. `Err` is for an agent that
/// could not be asked, with nothing sent.
pub fn collect(agent: &mut dyn Agent, file: &Path, timing: Timing) -> Result<Collected, String> {
    agent.ready()?;
    let _ = fs::remove_file(file);
    agent.ask(&request(file))?;
    let deadline = Instant::now() + timing.wait;
    let mut ended_at: Option<Instant> = None;
    loop {
        match agent.turn_ended(timing.poll) {
            Ok(true) => {
                ended_at.get_or_insert_with(Instant::now);
            }
            Ok(false) => {}
            Err(error) => return Ok(Collected::Missing(error)),
        }
        if ended_at.is_some()
            && let Some(summary) = read(file)
        {
            return Ok(Collected::Written(summary));
        }
        if ended_at.is_some_and(|at| at.elapsed() >= timing.grace) {
            return Ok(Collected::Missing(format!(
                "the agent's turn ended without a summary at {}",
                file.display()
            )));
        }
        if Instant::now() >= deadline {
            return Ok(Collected::Missing(format!(
                "no summary after {}",
                waited(timing.wait)
            )));
        }
        // An agent that has finished does not make the wait for its file; do it here.
        if ended_at.is_some() {
            std::thread::sleep(timing.poll);
        }
    }
}

/// How long a wait was, in the words a person would use.
fn waited(wait: Duration) -> String {
    match wait.as_secs() {
        seconds if seconds >= 120 => format!("{} minutes", seconds.div_ceil(60)),
        60..=119 => "1 minute".into(),
        seconds => format!("{} seconds", seconds.max(1)),
    }
}

/// The number of lines in a file, which for a chat's log is the `seq` of its last event.
fn count_lines(path: &Path) -> Option<u64> {
    let mut file = fs::File::open(path).ok()?;
    let (mut buffer, mut lines) = (vec![0; 64 * 1024], 0);
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            return Some(lines);
        }
        lines += buffer[..read].iter().filter(|byte| **byte == b'\n').count() as u64;
    }
}

/// The summary in `file`, if there is one with something in it.
fn read(file: &Path) -> Option<String> {
    let mut text = String::new();
    fs::File::open(file)
        .ok()?
        .take(MAX_SUMMARY)
        .read_to_string(&mut text)
        .ok()?;
    (!text.trim().is_empty()).then_some(text)
}

// ---- A chat ----------------------------------------------------------------------------

/// A chat, asked through the chat host.
pub struct ChatAgent {
    pub home: PathBuf,
    pub socket: PathBuf,
    pub chat_id: String,
    events: Option<Subscription>,
    started: bool,
    ended: bool,
}

impl ChatAgent {
    pub fn new(home: PathBuf, socket: PathBuf, chat_id: String) -> Self {
        Self {
            home,
            socket,
            chat_id,
            events: None,
            started: false,
            ended: false,
        }
    }
}

impl Agent for ChatAgent {
    fn ready(&mut self) -> Result<(), String> {
        let chats = Client::connect(&self.socket)?.list()?;
        let chat = chats
            .iter()
            .find(|chat| chat.id == self.chat_id)
            .ok_or("The chat host does not know this chat.")?;
        match &chat.state {
            ChatState::Idle => Ok(()),
            ChatState::Starting | ChatState::Running | ChatState::Waiting => Err(
                "The chat is busy. A summary can only be asked of a chat that is idle; wait for it, or use --context transcript."
                    .into(),
            ),
            ChatState::Stopped | ChatState::Failed { .. } => Err(
                "The chat is not running, so there is no one to ask. Use --context transcript."
                    .into(),
            ),
        }
    }

    fn ask(&mut self, message: &str) -> Result<(), String> {
        // Follow from the end of the log as it is now: the events of this turn only.
        let seen = crate::chat::log::chat_dir(&self.home, &self.chat_id)
            .and_then(|dir| count_lines(&dir.join("events.jsonl")))
            .unwrap_or(0);
        self.events = Some(Subscription::open(&self.socket, &self.chat_id, seen)?);
        Client::connect(&self.socket)?.command(
            &self.chat_id,
            ChatCommand::Send {
                text: message.to_owned(),
            },
        )
    }

    fn turn_ended(&mut self, wait: Duration) -> Result<bool, String> {
        if self.ended {
            return Ok(true);
        }
        let Some(events) = self.events.as_mut() else {
            return Err("the chat was not asked".into());
        };
        let end = Instant::now() + wait;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match events.next_within(left)? {
                Poll::Event(envelope) => match envelope.event {
                    ChatEvent::TurnStarted { .. } => self.started = true,
                    ChatEvent::TurnCompleted { .. } if self.started => {
                        self.ended = true;
                        return Ok(true);
                    }
                    ChatEvent::State {
                        state: ChatState::Failed { message },
                    } => return Err(format!("the chat failed: {message}")),
                    _ => {}
                },
                Poll::TimedOut => return Ok(false),
                Poll::Closed => return Err("the chat host closed the connection".into()),
            }
            if Instant::now() >= end {
                return Ok(false);
            }
        }
    }
}

// ---- A terminal's Codex or Claude -----------------------------------------------------

/// A Codex or Claude in a terminal. Its turns are seen as a schedule sees them: Codex
/// from its rollout, Claude from the hooks' cursor, each giving a token for the last
/// completed turn while the agent is idle and none while it works.
pub struct TerminalAgent {
    manager: SessionManager,
    shell: ShellSession,
    tracker: ActivityTracker,
    /// The conversation asked, so that another one cannot answer.
    conversation: Option<String>,
    before: Option<String>,
}

impl TerminalAgent {
    pub fn new(manager: SessionManager, shell: ShellSession) -> Self {
        let tracker = ActivityTracker::at(manager.state_home().to_path_buf());
        Self {
            manager,
            shell,
            tracker,
            conversation: None,
            before: None,
        }
    }

    /// The token of the last completed turn, or `None` while the agent works or its
    /// state is unknown.
    fn idle_token(&mut self) -> Option<String> {
        let conversation = self.conversation.clone()?;
        match self.shell.harness {
            Some(HarnessKind::Codex) => {
                self.tracker.schedule_idle_token(&self.shell, &conversation)
            }
            Some(HarnessKind::Claude) => {
                crate::agent_hooks::schedule_state(self.manager.state_home(), &self.shell.id)
                    .filter(|(session, _)| *session == conversation)
                    .and_then(|(_, token)| token)
            }
            _ => None,
        }
    }
}

impl Agent for TerminalAgent {
    fn ready(&mut self) -> Result<(), String> {
        if !matches!(
            self.shell.harness,
            Some(HarnessKind::Codex | HarnessKind::Claude)
        ) {
            return Err("Only a Codex or Claude terminal can be asked for a summary.".into());
        }
        let current = self.manager.get(&self.shell.id)?;
        if !current.alive {
            return Err(
                "The terminal has exited, so there is no one to ask. Use --context transcript."
                    .into(),
            );
        }
        self.conversation = match self.shell.harness {
            Some(HarnessKind::Codex) => self.tracker.schedule_identity(&self.shell),
            _ => crate::agent_hooks::schedule_state(self.manager.state_home(), &self.shell.id)
                .map(|(session, _)| session),
        };
        self.before = self.idle_token();
        if self.before.is_none() {
            return Err(
                "The agent is busy, or RiWork cannot tell yet that it is idle (it learns that after a first completed turn). A summary can only be asked of an idle agent; wait for it, or use --context transcript."
                    .into(),
            );
        }
        Ok(())
    }

    fn ask(&mut self, message: &str) -> Result<(), String> {
        self.manager.send_at_empty_prompt(&self.shell, message)
    }

    fn turn_ended(&mut self, wait: Duration) -> Result<bool, String> {
        std::thread::sleep(wait);
        Ok(self
            .idle_token()
            .is_some_and(|token| Some(&token) != self.before.as_ref()))
    }
}

#[cfg(test)]
mod tests;
