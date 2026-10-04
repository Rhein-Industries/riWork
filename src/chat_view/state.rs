//! What a chat tab knows about its chat, apart from how it is drawn: the transcript the host's
//! events build, how the tab is connected to the host, and what changed with each batch of
//! events, so that the list redraws only the rows that need it.

use std::collections::{BTreeSet, HashMap};

use crate::activity::AgentActivity;
use crate::activity::ChatActivity;
use crate::chat::model::{ChatEvent, ChatState, ItemStatus, Provider, Transcript};
use crate::chat::wire::Envelope;

/// How the tab is connected to the chat host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Link {
    /// Asking the host for the chat's events for the first time.
    #[default]
    Connecting,
    Live,
    /// The connection ended (the host restarted, or went away); a new one is being made.
    Reconnecting,
    /// The host does not know the chat any more.
    Deleted,
}

/// What a batch of events did to the list of items.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Items that did not exist before, at the end of the list.
    pub appended: usize,
    /// Items that were there and changed: their rows must be measured again.
    pub touched: BTreeSet<usize>,
    /// Events that were new to the model; a repeated one counts for nothing.
    pub events: usize,
}

#[cfg(test)]
impl Applied {
    pub fn is_empty(&self) -> bool {
        self.appended == 0 && self.touched.is_empty() && self.events == 0
    }
}

/// What the window needs to know about a chat tab, to title the tab and to count the chat
/// among the agents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub title: String,
    pub provider: Option<Provider>,
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
    pub state: ChatState,
    pub link: Link,
    pub activity: Option<AgentActivity>,
}

impl Summary {
    /// The chat as the project and worktree rows count it.
    pub fn counted(&self) -> Option<ChatActivity> {
        Some(ChatActivity {
            project_id: self.project_id.clone(),
            worktree_id: self.worktree_id.clone(),
            activity: self.activity?,
        })
    }
}

pub struct ChatModel {
    pub transcript: Transcript,
    pub link: Link,
    /// The last event applied; a new subscription starts after it.
    pub last_seq: u64,
    /// A turn has ended in this chat, so an idle chat is "done" and not just new.
    pub turn_finished: bool,
    /// Where each item of the transcript is in its list.
    positions: HashMap<String, usize>,
}

impl ChatModel {
    pub fn new() -> Self {
        Self {
            transcript: Transcript::default(),
            link: Link::Connecting,
            last_seq: 0,
            turn_finished: false,
            positions: HashMap::new(),
        }
    }

    /// Fold events in, oldest first. An event at or before the last one applied is a
    /// repeat (a subscription that restarted early) and is ignored.
    pub fn apply(&mut self, envelopes: &[Envelope]) -> Applied {
        let before = self.transcript.items.len();
        let mut touched = BTreeSet::new();
        let mut events = 0;
        for envelope in envelopes {
            if envelope.seq <= self.last_seq {
                continue;
            }
            self.last_seq = envelope.seq;
            events += 1;
            match &envelope.event {
                ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } => {
                    if let Some(&at) = self.positions.get(&item.id) {
                        touched.insert(at);
                    }
                }
                ChatEvent::ItemDelta { item_id, .. } => {
                    if let Some(&at) = self.positions.get(item_id) {
                        touched.insert(at);
                    }
                }
                ChatEvent::TurnCompleted { .. } => {
                    self.turn_finished = true;
                    // Whatever was still going on in the turn is over; those rows change.
                    touched.extend(
                        self.transcript
                            .items
                            .iter()
                            .enumerate()
                            .filter(|(_, item)| item.status == ItemStatus::InProgress)
                            .map(|(at, _)| at),
                    );
                }
                _ => {}
            }
            self.transcript.apply(&envelope.event);
            self.index_new_items();
        }
        touched.retain(|at| *at < before);
        Applied {
            appended: self.transcript.items.len() - before,
            touched,
            events,
        }
    }

    fn index_new_items(&mut self) {
        for (at, item) in self
            .transcript
            .items
            .iter()
            .enumerate()
            .skip(self.positions.len())
        {
            self.positions.insert(item.id.clone(), at);
        }
    }

    pub fn provider(&self) -> Option<Provider> {
        self.transcript.info.as_ref().map(|info| info.provider)
    }

    /// The chat's title: the host's, or the provider's name for a chat that has none yet.
    pub fn title(&self) -> String {
        let Some(info) = &self.transcript.info else {
            return "Chat".to_owned();
        };
        if info.title.trim().is_empty() {
            format!("{} chat", provider_name(info.provider))
        } else {
            info.title.clone()
        }
    }

    pub fn summary(&self) -> Summary {
        let info = self.transcript.info.as_ref();
        Summary {
            title: self.title(),
            provider: self.provider(),
            project_id: info.and_then(|info| info.project_id.clone()),
            worktree_id: info.and_then(|info| info.worktree_id.clone()),
            state: self.transcript.state.clone(),
            link: self.link,
            activity: ChatActivity::of_state(&self.transcript.state, self.turn_finished),
        }
    }
}

pub fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ApprovalMode, ChatInfo, Delta, Item, ItemBody, TurnOutcome};

    fn envelope(seq: u64, event: ChatEvent) -> Envelope {
        Envelope {
            chat_id: "chat".into(),
            seq,
            event,
        }
    }

    fn info(title: &str, state: ChatState) -> ChatInfo {
        ChatInfo {
            id: "chat".into(),
            provider: Provider::Claude,
            project_id: Some("project".into()),
            worktree_id: Some("tree".into()),
            cwd: "/work".into(),
            title: title.into(),
            created_at_unix: 1,
            provider_thread_id: None,
            model: None,
            effort: None,
            approval_mode: ApprovalMode::Supervised,
            codex_account_id: None,
            state,
        }
    }

    fn started(id: &str, body: ItemBody) -> ChatEvent {
        ChatEvent::ItemStarted {
            item: Item {
                id: id.into(),
                turn_id: Some("t1".into()),
                status: ItemStatus::InProgress,
                body,
            },
        }
    }

    fn agent(text: &str) -> ItemBody {
        ItemBody::AgentMessage { text: text.into() }
    }

    #[test]
    fn new_items_are_appended_and_changed_ones_are_touched_by_position() {
        let mut model = ChatModel::new();
        let first = model.apply(&[
            envelope(1, started("a", agent(""))),
            envelope(
                2,
                ChatEvent::ItemDelta {
                    item_id: "a".into(),
                    delta: Delta::Text("Hel".into()),
                },
            ),
            envelope(3, started("b", agent(""))),
        ]);
        // Both are new, so the list adds two rows and has nothing to measure again.
        assert_eq!(
            first,
            Applied {
                appended: 2,
                touched: BTreeSet::new(),
                events: 3
            }
        );

        let next = model.apply(&[
            envelope(
                4,
                ChatEvent::ItemDelta {
                    item_id: "a".into(),
                    delta: Delta::Text("lo".into()),
                },
            ),
            envelope(5, started("c", agent(""))),
        ]);
        assert_eq!(next.appended, 1);
        assert_eq!(next.touched, BTreeSet::from([0]));
        assert_eq!(model.last_seq, 5);
        assert_eq!(model.transcript.items[0].body, agent("Hello"));
    }

    #[test]
    fn an_event_seen_before_changes_nothing() {
        let mut model = ChatModel::new();
        let events = [
            envelope(1, started("a", agent("x"))),
            envelope(
                2,
                ChatEvent::ItemDelta {
                    item_id: "a".into(),
                    delta: Delta::Text("y".into()),
                },
            ),
        ];
        model.apply(&events);
        // A subscription that restarted from an earlier point replays them.
        let again = model.apply(&events);
        assert!(again.is_empty(), "{again:?}");
        assert_eq!(model.transcript.items[0].body, agent("xy"));
        assert_eq!(model.last_seq, 2);
    }

    #[test]
    fn the_end_of_a_turn_touches_the_rows_it_closed() {
        let mut model = ChatModel::new();
        model.apply(&[
            envelope(
                1,
                ChatEvent::TurnStarted {
                    turn_id: "t1".into(),
                },
            ),
            envelope(2, started("a", agent("done"))),
            envelope(
                3,
                ChatEvent::ItemCompleted {
                    item: Item {
                        id: "a".into(),
                        turn_id: Some("t1".into()),
                        status: ItemStatus::Completed,
                        body: agent("done"),
                    },
                },
            ),
            envelope(
                4,
                started(
                    "cmd",
                    ItemBody::Command {
                        command: "sleep 9".into(),
                        cwd: None,
                        output: String::new(),
                        exit_code: None,
                    },
                ),
            ),
        ]);
        assert!(!model.turn_finished);
        let end = model.apply(&[envelope(
            5,
            ChatEvent::TurnCompleted {
                turn_id: "t1".into(),
                outcome: TurnOutcome::Interrupted,
            },
        )]);
        assert_eq!(end.touched, BTreeSet::from([1]));
        assert_eq!(model.transcript.items[1].status, ItemStatus::Interrupted);
        assert!(model.turn_finished);
    }

    #[test]
    fn the_tab_is_titled_by_the_chat_and_counted_by_its_state() {
        let mut model = ChatModel::new();
        assert_eq!(model.title(), "Chat");
        assert_eq!(model.summary().provider, None);
        model.apply(&[envelope(
            1,
            ChatEvent::Info {
                info: info("", ChatState::Idle),
            },
        )]);
        assert_eq!(model.title(), "Claude chat");
        let idle = model.summary();
        assert_eq!(idle.activity, None, "nothing has happened in the chat yet");
        assert_eq!(idle.counted(), None);

        model.apply(&[envelope(
            2,
            ChatEvent::State {
                state: ChatState::Running,
            },
        )]);
        let running = model.summary();
        assert_eq!(running.activity, Some(AgentActivity::Working));
        assert_eq!(
            running.counted(),
            Some(ChatActivity {
                project_id: Some("project".into()),
                worktree_id: Some("tree".into()),
                activity: AgentActivity::Working,
            })
        );

        model.apply(&[
            envelope(
                3,
                ChatEvent::TurnCompleted {
                    turn_id: "t".into(),
                    outcome: TurnOutcome::Completed,
                },
            ),
            envelope(
                4,
                ChatEvent::State {
                    state: ChatState::Idle,
                },
            ),
            envelope(
                5,
                ChatEvent::Info {
                    info: info("Fix the build", ChatState::Idle),
                },
            ),
        ]);
        let done = model.summary();
        assert_eq!(done.title, "Fix the build");
        assert_eq!(done.activity, Some(AgentActivity::Done));
        assert_ne!(done, running);
    }
}
