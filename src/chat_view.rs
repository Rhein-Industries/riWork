//! The chat tab: a native view of one Codex or Claude chat that the chat host keeps.
//!
//! The host (`riwork chat serve`) owns the provider process and the chat's event log. A tab
//! subscribes to the log by chat id, folds the events into a `Transcript` as they come, and
//! sends the user's commands back, so closing a tab, reloading the app or restarting the host
//! never loses a chat. This file holds the view and what happens to it; the parts that need
//! no window live in the modules beside it and are tested alone:
//!
//! - `markdown` reads an agent's message, `diff` a file change, `cards` shortens a transcript
//!   item to the line a card shows;
//! - `feed` keeps the subscription and the command queue alive across host restarts, `state`
//!   folds what it delivers;
//! - `approval`, `composer` and `toolbar` hold the words and keys of the matching bars;
//! - `rows`, `prose`, `panels` and `widgets` draw.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, FollowMode, KeyDownEvent, ListAlignment,
    ListState, Render, Subscription, Task, Window, actions, div, prelude::*, px, rgb,
};

use crate::{
    chat::{
        client::Client,
        model::{
            ApprovalMode, ChatCommand, ChatInfo, Decision, ItemBody, NewChat, NoticeLevel,
            Provider, Question,
        },
    },
    text_input::{self, EnterBehavior, InputEvent, InputState, TextareaState},
    ui_text,
};

mod approval;
mod attachment_draft;
mod attachment_ui;
pub(crate) use attachment_ui::init as init_attachments;
mod cards;
mod composer;
mod dictate;
mod diff;
mod display;
#[cfg(test)]
mod draft_tests;
#[cfg(test)]
mod editor_tests;
mod editors;
mod feed;
mod host;
mod links;
mod markdown;
mod media;
#[cfg(test)]
mod notice_tests;
mod notice_ui;
mod notices;
mod panels;
mod prose;
mod rows;
mod select;
mod selection_document;
#[cfg(test)]
mod selection_tests;
mod state;
#[cfg(test)]
mod testing;
mod toolbar;
mod usage_chip;
mod widgets;

pub use display::DisplayMode;
pub use host::HostConfig;
pub use links::resolve as resolve_file;
pub use state::{Link, Summary};

use feed::{Backoff, Feed, FeedMsg};
use state::{Applied, ChatModel, provider_name};
use widgets::Look;

actions!(riwork_chat, [InterruptChat, ToggleDictation]);

/// What a chat tab tells the window.
pub enum ChatViewEvent {
    /// The title, the agent behind it, or what it is doing changed: the tab and the counts
    /// of agents are drawn again.
    Changed,
    /// A chat that was being made now exists, under this id.
    Created(String),
    /// The tab has nothing left to show and asks to be closed.
    Close,
    /// **Hand off…** was chosen: the window asks where the conversation goes.
    HandOff,
    OpenFile {
        target: String,
    },
    /// A usage-limit notice's **Show usage** was pressed.
    ShowUsage,
}

impl EventEmitter<ChatViewEvent> for ChatView {}

/// A chat the host is still making, or could not make.
enum Creation {
    Pending(NewChat),
    Failed(NewChat, String),
}

/// The popover that is open under the toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Mode,
    Model,
    Effort,
    More,
    /// Asking again before the chat and its history are deleted.
    ConfirmDelete,
}

/// What has been picked so far for the questions that wait, by option number.
#[derive(Default)]
struct Draft {
    picked: Vec<Vec<usize>>,
    prompts: Vec<crate::chat::model::QuestionPrompt>,
}

/// Work done on the way to drawing, kept while its input is the same.
#[derive(Default)]
struct Caches {
    /// Parsed messages by item id, with a hash of the text they were parsed from.
    markdown: HashMap<String, (u64, Arc<Vec<markdown::Block>>)>,
    /// Counts invalidated by exact changes, including same-length replacements.
    changes: HashMap<String, (Vec<crate::chat::model::FileChange>, cards::FileStats)>,
    /// Rich diff text and its kind, including same-length streaming replacements.
    diffs: HashMap<String, (String, crate::chat::model::ChangeKind, Arc<DrawnDiff>)>,
    /// The boxes that scroll in the transcript, by name.
    scrollers: HashMap<String, widgets::Scroller>,
}

/// The lines of one file's diff that are drawn, and how many more there are.
struct DrawnDiff {
    lines: Vec<(diff::LineKind, String)>,
    hidden: usize,
}

pub struct ChatView {
    config: HostConfig,
    window_handle: gpui::AnyWindowHandle,
    /// The host's id for the chat; none while a new chat is being made.
    chat_id: Option<String>,
    creation: Option<Creation>,
    creation_coordinator: Option<crate::chat_tabs::Coordinator>,
    creation_reservation: Option<crate::chat_tabs::Reservation>,
    model: ChatModel,
    feed: Option<Feed>,
    feed_started: bool,
    inventory_info: Option<ChatInfo>,
    /// Hands what the feed delivers to the view; dropped with it.
    pump: Option<Task<()>>,
    /// The request to make a chat, or to delete one, in flight.
    working: Option<Task<()>>,
    list: ListState,
    /// How many of the list's rows are transcript items; the row after them is the footer.
    items_in_list: usize,
    visible: Vec<display::Row>,
    display_mode: DisplayMode,
    pending_display: Option<DisplayMode>,
    media: media::MediaState,
    preview_root: Option<std::path::PathBuf>,
    /// The interface text scale the list's rows were measured at.
    scale: f32,
    focus: FocusHandle,
    composer: Entity<TextareaState>,
    model_input: Entity<InputState>,
    answers: HashMap<editors::AnswerKey, editors::AnswerEditor>,
    subscriptions: Vec<Subscription>,
    model_seeded: bool,
    focus_composer: bool,
    editor_generation: u64,
    programmatic_changes: usize,
    enter_down: bool,
    enter_repeated: bool,
    escape_down: bool,
    attachments: Vec<attachment_ui::Chip>,
    attachment_tasks: Vec<Task<()>>,
    submissions: Vec<attachment_draft::Submission>,
    pending_submission: Option<u64>,
    next_submission: u64,
    answer_submissions: HashMap<u64, editors::AnswerSnapshot>,
    answer_failures: HashMap<String, editors::AnswerFailure>,
    menu: Option<Menu>,
    /// The menu that a click outside just closed, and when. The click that closes an open
    /// menu on its own button must not open it again.
    menu_closed: Option<(Menu, Instant)>,
    /// When the last message was sent: an Enter right after it is not an answer to a request.
    sent_at: Option<Instant>,
    /// Item ids (and `item#n` for the files of a change) that are expanded.
    open: HashSet<String>,
    /// Requests this tab already answered, until the host says they are resolved.
    answered: HashSet<String>,
    drafts: HashMap<String, Draft>,
    /// The banners above the message box: the provider's notices and this tab's errors.
    notices: notices::Notices,
    /// The redraw due when a shown limit resets (`schedule_notice_expiry`), and when.
    notice_expiry: Option<(u64, Task<()>)>,
    /// The copy button that was just pressed, until it is forgotten.
    copied: Option<String>,
    forget_copy: Option<Task<()>>,
    caches: RefCell<Caches>,
    /// The message box row's width as last laid out, which decides where its buttons go
    /// (see `composer::fit`); zero until it is first drawn.
    composer_width: std::rc::Rc<std::cell::Cell<f32>>,
    /// Base participants for the visible, styled transcript; Base owns selection.
    transcript_selection: select::TranscriptSelection,
    /// What the window was last told, so it hears of changes only.
    announced: Option<Summary>,
    /// Dictation into the message box.
    dictation: dictate::Dictation,
    /// The names and folders around the chat (project, worktrees and their branches), which
    /// dictation listens for; set by the window.
    speech_names: Vec<String>,
    speech_paths: Vec<String>,
}

impl ChatView {
    pub fn set_preview_root(&mut self, root: Option<std::path::PathBuf>) {
        self.preview_root = root;
    }
    pub fn info(&self) -> Option<&ChatInfo> {
        self.model.transcript.info.as_ref()
    }
    /// The tab of a chat the host already has: it subscribes from the start and builds the
    /// transcript again.
    pub fn open(
        chat_id: String,
        config: HostConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::blank(config, window, cx);
        view.chat_id = Some(chat_id.clone());
        view.restore_draft(window, cx);
        view.follow_display_setting(cx);
        view.start_feed(chat_id, 0, window, cx);
        view
    }

    /// A background tab carries inventory metadata without a feed until first rendered.
    pub fn open_deferred(
        chat_id: String,
        info: Option<ChatInfo>,
        config: HostConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::blank(config, window, cx);
        view.chat_id = Some(chat_id);
        view.inventory_info = info;
        view
    }

    /// Keep tab metadata fresh without subscribing to the transcript.
    pub fn update_inventory(&mut self, info: &ChatInfo, cx: &mut Context<Self>) {
        if self.inventory_info.as_ref() != Some(info) {
            self.inventory_info = Some(info.clone());
            self.announce(cx);
            cx.notify();
        }
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.feed_started
            && self.feed.is_none()
            && !self.deleted()
            && let Some(id) = self.chat_id.clone()
        {
            self.start_feed(id, 0, window, cx);
        }
    }

    /// The tab of a chat that is yet to be made: the host makes it, and the tab follows it.
    pub fn create(
        chat: NewChat,
        config: HostConfig,
        coordinator: crate::chat_tabs::Coordinator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::blank(config, window, cx);
        view.creation_coordinator = Some(coordinator);
        view.begin_creation(chat, window, cx);
        view
    }

    fn blank(config: HostConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // One row for the footer. Rows are added before it as items arrive, and the list
        // follows the end while the user has not scrolled up.
        let list = ListState::new(1, ListAlignment::Top, px(600.0));
        list.set_follow_mode(FollowMode::Tail);
        // Settings shows or hides the mic in every open chat at once.
        cx.observe_global_in::<crate::settings::Settings>(window, |view, window, cx| {
            view.follow_mic_setting(window, cx);
            view.follow_display_setting(cx);
        })
        .detach();
        cx.on_release(|view, cx| {
            view.transcript_selection.retire(view.window_handle, cx);
            view.release_images(cx);
            view.release_attachment_previews(cx);
        })
        .detach();
        let composer =
            text_input::multiline("", "Message", 1, 8, EnterBehavior::Submit, window, cx);
        let model_input = text_input::single_line("", "model name, then ⏎", window, cx);
        let subscriptions = vec![
            cx.subscribe_in(&composer, window, Self::composer_event),
            cx.subscribe_in(&model_input, window, Self::model_event),
        ];
        let focus = cx.focus_handle();
        let transcript_selection =
            select::TranscriptSelection::new(window.window_handle(), cx.weak_entity(), cx);
        Self {
            config,
            window_handle: window.window_handle(),
            chat_id: None,
            creation: None,
            creation_coordinator: None,
            creation_reservation: None,
            model: ChatModel::new(),
            feed: None,
            feed_started: false,
            inventory_info: None,
            pump: None,
            working: None,
            list,
            items_in_list: 0,
            visible: Vec::new(),
            display_mode: cx
                .try_global::<crate::settings::Settings>()
                .map(|s| s.chat_display)
                .unwrap_or_default(),
            pending_display: None,
            media: Default::default(),
            preview_root: None,
            scale: ui_text::scale(),
            focus,
            composer,
            model_input,
            subscriptions,
            answers: HashMap::new(),
            model_seeded: false,
            focus_composer: false,
            composer_width: Default::default(),
            editor_generation: 0,
            programmatic_changes: 0,
            enter_down: false,
            enter_repeated: false,
            escape_down: false,
            attachments: Vec::new(),
            attachment_tasks: Vec::new(),
            submissions: Vec::new(),
            pending_submission: None,
            next_submission: 0,
            answer_submissions: HashMap::new(),
            answer_failures: HashMap::new(),
            menu: None,
            menu_closed: None,
            sent_at: None,
            open: HashSet::new(),
            answered: HashSet::new(),
            drafts: HashMap::new(),
            notices: notices::Notices::default(),
            notice_expiry: None,
            copied: None,
            forget_copy: None,
            caches: RefCell::new(Caches::default()),
            transcript_selection,
            announced: None,
            dictation: dictate::Dictation::default(),
            speech_names: Vec::new(),
            speech_paths: Vec::new(),
        }
    }

    // -----------------------------------------------------------------------------------
    // What the window asks
    // -----------------------------------------------------------------------------------

    /// What the tab strip and the agent counts need.
    pub fn summary(&self) -> Summary {
        let mut summary = self.model.summary();
        if !self.feed_started || self.model.transcript.info.is_none() {
            if let Some(info) = &self.inventory_info {
                summary.title = if info.title.trim().is_empty() {
                    format!("{} chat", provider_name(info.provider))
                } else {
                    info.title.clone()
                };
                summary.provider = Some(info.provider);
                summary.project_id = info.project_id.clone();
                summary.worktree_id = info.worktree_id.clone();
                summary.state = info.state.clone();
                summary.activity = crate::activity::ChatActivity::of_state(&info.state, false);
            }
        }
        if let Some(chat) = self.creation.as_ref().map(Creation::chat) {
            summary.provider.get_or_insert(chat.provider);
            if summary.title == "Chat" {
                summary.title = format!("{} chat", provider_name(chat.provider));
            }
            summary.project_id = summary.project_id.or_else(|| chat.project_id.clone());
            summary.worktree_id = summary.worktree_id.or_else(|| chat.worktree_id.clone());
        }
        summary
    }

    /// Give the keys to the message box.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        gpui_kit::base::TextSelection::clear(window, cx);
        self.composer.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// The names and folders around the chat that dictation should know: the project, its
    /// worktrees and their branches.
    pub fn set_speech_context(&mut self, names: Vec<String>, paths: Vec<String>) {
        self.speech_names = names;
        self.speech_paths = paths;
    }

    /// Ask the window to hand this chat's conversation over; a chat that is still being
    /// made has no conversation to hand.
    pub fn hand_off(&mut self, cx: &mut Context<Self>) {
        if self.chat_id.is_some() {
            cx.emit(ChatViewEvent::HandOff);
        }
    }

    /// Stop the provider process. The chat and its history stay; the next message resumes it.
    pub fn stop_chat(&mut self, cx: &mut Context<Self>) {
        self.command(ChatCommand::Stop);
        cx.notify();
    }

    // -----------------------------------------------------------------------------------
    // Making and following a chat
    // -----------------------------------------------------------------------------------

    fn begin_creation(&mut self, chat: NewChat, window: &mut Window, cx: &mut Context<Self>) {
        self.creation_reservation = self
            .creation_coordinator
            .as_ref()
            .map(|c| c.reserve(chat.project_id.as_deref().unwrap_or("")));
        self.creation = Some(Creation::Pending(chat.clone()));
        let ensure = self.config.ensure.clone();
        let work = cx.background_executor().spawn(async move {
            let socket = ensure()?;
            Client::connect(&socket)?.create(chat)
        });
        self.working = Some(cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |view, window, cx| view.created(result, window, cx));
        }));
        cx.notify();
    }

    fn created(
        &mut self,
        result: Result<ChatInfo, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(creation) = self.creation.take() else {
            return;
        };
        match result {
            Ok(info) => {
                if let Some(reservation) = self.creation_reservation.take() {
                    reservation.finish(&info.id);
                }
                self.chat_id = Some(info.id.clone());
                // What was typed while the host made the chat is its first draft.
                self.remember_draft(cx);
                if let Some(mode) = self.pending_display.take() {
                    self.choose_display(mode, cx);
                } else {
                    self.follow_display_setting(cx);
                }
                self.start_feed(info.id.clone(), 0, window, cx);
                cx.emit(ChatViewEvent::Created(info.id));
            }
            Err(error) => {
                self.creation_reservation.take();
                self.creation = Some(Creation::Failed(creation.into_chat(), error));
            }
        }
        self.announce(cx);
        cx.notify();
    }

    fn retry_creation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Creation::Failed(chat, _)) = self.creation.take() {
            self.begin_creation(chat, window, cx);
        }
    }

    fn start_feed(
        &mut self,
        chat_id: String,
        since: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.feed_started = true;
        let (sender, receiver) = async_channel::unbounded();
        self.feed = Some(Feed::start(
            self.config.ensure.clone(),
            chat_id,
            since,
            sender,
            Backoff::DEFAULT,
        ));
        self.pump = Some(cx.spawn_in(window, async move |this, cx| {
            while let Ok(first) = receiver.recv().await {
                // Everything that is waiting is one redraw, however fast it arrives.
                let mut batch = vec![first];
                while let Ok(more) = receiver.try_recv() {
                    batch.push(more);
                }
                if this
                    .update_in(cx, |view, window, cx| view.accept(batch, window, cx))
                    .is_err()
                {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
            }
        }));
    }

    fn accept(&mut self, batch: Vec<FeedMsg>, window: &mut Window, cx: &mut Context<Self>) {
        let mut events = Vec::new();
        for message in batch {
            match message {
                FeedMsg::Events(mut more) => {
                    if !more.is_empty() {
                        self.notices.clear(notices::LocalKey::Link);
                    }
                    events.append(&mut more)
                }
                FeedMsg::Link(link) => {
                    self.model.link = link;
                    // The host answers again: an error about reaching it is over.
                    if link == Link::Live {
                        self.notices.clear(notices::LocalKey::Link);
                    }
                    if link == Link::Deleted {
                        if let Some(id) = &self.chat_id {
                            crate::chat_drafts::forget(id, cx);
                        }
                        self.transcript_selection.retire(self.window_handle, cx);
                        self.feed = None;
                    }
                }
                FeedMsg::Submission {
                    id,
                    command,
                    result,
                } => {
                    self.submission_receipt(id, command, result, window, cx);
                }
                FeedMsg::AttachmentSubmission { result, .. } => {
                    if let Err(error) = result {
                        self.notices.set(
                            notices::LocalKey::Send,
                            NoticeLevel::Error,
                            error.to_string(),
                        );
                    }
                }
                FeedMsg::CommandFailed { command, error } => {
                    self.command_failed(command, error);
                }
            }
        }
        let applied = self.model.apply(&events);
        self.notices.observe(self.model.transcript.items.len());
        self.schedule_notice_expiry(cx);
        self.sync_list(&applied, cx);
        self.forget_settled();
        self.ready_inputs(window, cx);
        self.announce(cx);
        cx.notify();
    }

    /// Tell the list about the rows that came or changed. Rows are measured when they are
    /// drawn, so a row that changed must be measured again.
    fn sync_list(&mut self, applied: &Applied, cx: &mut Context<Self>) {
        self.transcript_selection.changed();
        // Streaming content cannot leave an old copied projection. Unrelated
        // tail updates preserve selection and participant/editor identities.
        let selected_changed = self
            .transcript_selection
            .selected_rows(cx)
            .into_iter()
            .any(|row| match self.visible.get(row) {
                Some(display::Row::Item(at) | display::Row::Details(at)) => {
                    applied.touched.contains(at)
                }
                _ => false,
            });
        if selected_changed {
            self.transcript_selection.clear(self.window_handle, cx);
        }
        if applied.projection_changed {
            self.refresh_projection(cx);
        }
        for (row, item) in self.visible.iter().enumerate() {
            let touched = match item {
                display::Row::Item(at) | display::Row::Details(at) => applied.touched.contains(at),
                display::Row::Artifacts { items, .. } => {
                    items.iter().any(|at| applied.touched.contains(at))
                }
                _ => false,
            };
            if touched {
                self.list.remeasure_items(row..row + 1);
            }
        }
        self.list
            .remeasure_items(self.items_in_list..self.items_in_list + 1);
    }

    fn refresh_projection(&mut self, cx: &mut Context<Self>) {
        self.transcript_selection.changed();
        let rows = display::rows(
            &self.model.transcript,
            &self.model.completed,
            self.display_mode,
        );
        if rows != self.visible {
            if !rows.starts_with(&self.visible) {
                self.transcript_selection.retire(self.window_handle, cx);
            }
            let prefix = rows
                .iter()
                .zip(&self.visible)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = rows[prefix..]
                .iter()
                .rev()
                .zip(self.visible[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            self.list.splice(
                prefix..self.visible.len() - suffix,
                rows.len() - prefix - suffix,
            );
            self.items_in_list = rows.len();
            self.visible = rows;
        }
    }

    fn follow_display_setting(&mut self, cx: &mut Context<Self>) {
        let mode = self.pending_display.unwrap_or_else(|| {
            cx.global::<crate::settings::Settings>()
                .chat_display_for(self.chat_id.as_deref())
        });
        self.apply_display_mode(mode, cx);
    }

    fn apply_display_mode(&mut self, mode: DisplayMode, cx: &mut Context<Self>) {
        if self.display_mode != mode {
            let tail = self.list.is_following_tail();
            let offset = self.list.logical_scroll_top();
            let anchor = self.visible.get(offset.item_ix).cloned();
            self.display_mode = mode;
            self.transcript_selection.retire(self.window_handle, cx);
            self.refresh_projection(cx);
            self.list.remeasure();
            if tail {
                self.list.scroll_to_end();
            } else if let Some(anchor) = anchor {
                if let Some(at) =
                    display::remap_anchor(&anchor, &self.visible, &self.model.completed)
                {
                    self.list.scroll_to(gpui::ListOffset {
                        item_ix: at,
                        offset_in_item: if self.visible.get(at) == Some(&anchor) {
                            offset.offset_in_item
                        } else {
                            px(0.0)
                        },
                    });
                }
            } else {
                self.list.scroll_to(gpui::ListOffset {
                    item_ix: self.visible.len(),
                    offset_in_item: offset.offset_in_item,
                });
            }
            cx.notify();
        }
    }

    fn choose_display(&mut self, mode: DisplayMode, cx: &mut Context<Self>) {
        let Some(chat_id) = self.chat_id.clone() else {
            self.pending_display = Some(mode);
            self.apply_display_mode(mode, cx);
            return;
        };
        match crate::settings::SettingsStore::open_default().and_then(|store| {
            store.update(|settings| {
                settings.chat_display_modes.insert(chat_id, mode);
            })
        }) {
            Ok(settings) => {
                self.notices.clear(notices::LocalKey::Settings);
                cx.set_global(settings);
                self.follow_display_setting(cx);
            }
            Err(error) => {
                self.notices
                    .set(notices::LocalKey::Settings, NoticeLevel::Error, error);
                cx.notify();
            }
        }
    }

    /// Requests the host has settled need no memory here, and their drafts go with them.
    fn forget_settled(&mut self) {
        let transcript = &self.model.transcript;
        self.answered.retain(|id| {
            transcript.approvals.iter().any(|a| &a.request_id == id)
                || transcript.questions.iter().any(|q| &q.request_id == id)
        });
        self.drafts
            .retain(|id, _| transcript.questions.iter().any(|q| &q.request_id == id));
    }

    fn announce(&mut self, cx: &mut Context<Self>) {
        let summary = self.summary();
        if self.announced.as_ref() != Some(&summary) {
            self.announced = Some(summary);
            cx.emit(ChatViewEvent::Changed);
        }
    }

    /// The list's rows were measured at another text size.
    fn follow_text_size(&mut self, cx: &mut Context<Self>) {
        let scale = ui_text::scale();
        if scale != self.scale {
            self.transcript_selection.clear(self.window_handle, cx);
            self.scale = scale;
            self.list.remeasure();
        }
    }

    // -----------------------------------------------------------------------------------
    // What the user does
    // -----------------------------------------------------------------------------------

    fn command(&mut self, command: ChatCommand) {
        match &self.feed {
            Some(feed) => feed.send(command),
            None => self.notices.set(
                notices::LocalKey::Link,
                NoticeLevel::Warning,
                "Not connected to the chat yet.",
            ),
        }
    }

    fn command_failed(&mut self, command: ChatCommand, error: String) {
        match command {
            ChatCommand::Approve { request_id, .. } | ChatCommand::Answer { request_id, .. } => {
                self.answered.remove(&request_id);
            }
            _ => {}
        }
        self.notices.set(
            notices::LocalKey::Link,
            NoticeLevel::Error,
            format!("Could not reach the chat: {error}"),
        );
    }

    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if self.running() {
            self.command(ChatCommand::Interrupt);
            cx.notify();
        }
    }

    fn interrupt_action(&mut self, _: &InterruptChat, _: &mut Window, cx: &mut Context<Self>) {
        self.interrupt(cx);
    }

    /// ⌃⌥D. With the mic hidden the key is not this tab's: it goes on as if unbound.
    fn dictation_action(
        &mut self,
        _: &ToggleDictation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if dictate::mic_shown(cx) {
            self.toggle_dictation(window, cx);
        } else {
            cx.propagate();
        }
    }

    fn approve(&mut self, request_id: String, decision: Decision, cx: &mut Context<Self>) {
        if self.answered.insert(request_id.clone()) {
            self.command(ChatCommand::Approve {
                request_id,
                decision,
            });
            cx.notify();
        }
    }

    fn configure(
        &mut self,
        model: Option<String>,
        effort: Option<String>,
        approval_mode: Option<ApprovalMode>,
        fast: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        self.focus_composer = true;
        self.command(ChatCommand::Configure {
            model,
            effort,
            approval_mode,
            fast,
        });
        cx.notify();
    }

    /// Choose a model from the driver's list. An effort the model does not take is replaced
    /// by the model's own default, so the chat never shows one that is not used.
    fn choose_model(&mut self, model: &str, cx: &mut Context<Self>) {
        let effort = self
            .model
            .transcript
            .info
            .as_ref()
            .and_then(|info| info.effort.as_deref());
        let (model, effort) = toolbar::model_choice(&self.model.transcript.models, model, effort);
        self.configure(Some(model), effort, None, None, cx);
    }

    /// Turn the provider's fast mode on or off for the chat.
    fn toggle_fast(&mut self, cx: &mut Context<Self>) {
        let on = self
            .model
            .transcript
            .info
            .as_ref()
            .is_some_and(|info| info.fast);
        self.configure(None, None, None, Some(!on), cx);
    }

    /// Start the provider process again for a chat that stopped or failed. Any command
    /// resumes it, and one that changes nothing is a way to ask without a message.
    fn resume(&mut self, cx: &mut Context<Self>) {
        self.configure(None, None, None, None, cx);
    }

    fn compact(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        self.command(ChatCommand::Compact);
        cx.notify();
    }

    fn delete_chat(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self.chat_id.clone() else {
            return;
        };
        self.menu = None;
        self.notices.clear(notices::LocalKey::Delete);
        let ensure = self.config.ensure.clone();
        let work = cx.background_executor().spawn(async move {
            let socket = ensure()?;
            Client::connect(&socket)?.delete(&chat_id)
        });
        self.working = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| view.delete_result(result, cx));
        }));
        cx.notify();
    }

    fn delete_result(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        match result {
            Ok(()) => {
                if let Some(id) = &self.chat_id {
                    crate::chat_drafts::forget(id, cx);
                }
                cx.emit(ChatViewEvent::Close);
            }
            Err(error) => {
                self.notices.set(
                    notices::LocalKey::Delete,
                    NoticeLevel::Error,
                    format!("Could not delete the chat: {error}"),
                );
                cx.notify();
            }
        }
    }

    /// Copy what an item holds: a message's text, or a command's output as it shows on screen.
    fn copy_item(&mut self, id: &str, cx: &mut Context<Self>) {
        let text = match self
            .model
            .transcript
            .items
            .iter()
            .find(|item| item.id == id)
            .map(|item| &item.body)
        {
            Some(ItemBody::AgentMessage { text }) => text.clone(),
            Some(ItemBody::Command { output, .. }) => cards::clean_output(output),
            _ => return,
        };
        self.copy(format!("copy:{id}"), text, cx);
    }

    /// The scrolling box called `name`.
    fn scroller(&self, name: &str) -> widgets::Scroller {
        self.caches
            .borrow_mut()
            .scrollers
            .entry(name.to_owned())
            .or_insert_with(widgets::Scroller::new)
            .clone()
    }

    fn copy(&mut self, key: String, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.copied = Some(key);
        self.forget_copy = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            let _ = this.update(cx, |view, cx| {
                view.copied = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn toggle(&mut self, key: &str, item: Option<usize>, cx: &mut Context<Self>) {
        self.transcript_selection.changed();
        // Disclosed/hidden text must not survive as a clipboard selection.
        self.transcript_selection.clear(self.window_handle, cx);
        if !self.open.remove(key) {
            self.open.insert(key.to_owned());
        }
        // The row changes height.
        if let Some(at) = item {
            if let Some(row) = self.visible.iter().position(|row| match row {
                display::Row::Item(index) | display::Row::Details(index) => *index == at,
                display::Row::Artifacts { items, .. } => items.contains(&at),
                _ => false,
            }) {
                self.list.remeasure_items(row..row + 1);
            }
        }
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(menu) = self.menu.take() {
            self.menu_closed = Some((menu, Instant::now()));
            self.focus_composer = true;
            cx.notify();
        }
    }

    fn toggle_menu(&mut self, menu: Menu, window: &mut Window, cx: &mut Context<Self>) {
        // The press that closed this menu is the one that is now a click on its button.
        if self.menu.is_none()
            && self.menu_closed.take().is_some_and(|(closed, at)| {
                closed == menu && at.elapsed() < Duration::from_millis(300)
            })
        {
            return;
        }
        if self.menu == Some(menu) {
            self.menu = None;
            self.focus_composer = true;
        } else {
            self.menu = Some(menu);
            if menu == Menu::Model && self.model.transcript.models.is_empty() {
                if !self.model_seeded {
                    let value = self
                        .model
                        .transcript
                        .info
                        .as_ref()
                        .and_then(|info| info.model.clone())
                        .unwrap_or_default();
                    self.model_input
                        .update(cx, |state, cx| state.set_value(value, window, cx));
                    self.model_seeded = true;
                }
                self.model_input.read(cx).focus_handle(cx).focus(window, cx);
            } else {
                self.focus(window, cx);
            }
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------------------
    // What the chat is doing
    // -----------------------------------------------------------------------------------

    fn provider(&self) -> Option<Provider> {
        self.model
            .provider()
            .or_else(|| self.creation.as_ref().map(|c| c.chat().provider))
    }

    fn running(&self) -> bool {
        use crate::chat::model::ChatState::{Running, Waiting};
        matches!(self.model.transcript.state, Running | Waiting)
    }

    fn deleted(&self) -> bool {
        self.model.link == Link::Deleted
    }

    /// The approval the bar shows now, with the number of those behind it.
    fn pending_approval(&self) -> Option<(&crate::chat::model::Approval, usize)> {
        let approvals = &self.model.transcript.approvals;
        approvals
            .first()
            .map(|approval| (approval, approvals.len() - 1))
    }

    /// The decisions the bar offers now: none for a request already answered.
    fn offered(&self) -> Vec<Decision> {
        match self.pending_approval() {
            Some((approval, _)) if !self.answered.contains(&approval.request_id) => {
                approval.choices.clone()
            }
            _ => Vec::new(),
        }
    }

    fn pending_question(&self) -> Option<&Question> {
        self.model.transcript.questions.first()
    }

    // -----------------------------------------------------------------------------------
    // Keys
    // -----------------------------------------------------------------------------------

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // Editing, Enter, clipboard text, and cursor motion belong exclusively to Kit.
        if event.keystroke.modifiers.platform && event.keystroke.key == "." {
            self.interrupt(cx);
            cx.stop_propagation();
        }
    }

    // -----------------------------------------------------------------------------------
    // Questions
    // -----------------------------------------------------------------------------------

    /// Pick option `option` of prompt `prompt` of the question that waits. A prompt that takes
    /// one answer is changed to it; one that takes several toggles it.
    fn pick(
        &mut self,
        request: &str,
        expected: &crate::chat::model::QuestionPrompt,
        prompt: usize,
        option: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(question) = self.pending_question() else {
            return;
        };
        if question.request_id != request {
            return;
        }
        if question.questions.get(prompt) != Some(expected) || option >= expected.options.len() {
            return;
        }
        let request_id = question.request_id.clone();
        let prompts = question.questions.len();
        // The question may have changed since this button was drawn.
        let Some(multi) = question.questions.get(prompt).map(|p| p.multi_select) else {
            return;
        };
        let draft = self.drafts.entry(request_id).or_default();
        draft.picked.resize_with(prompts, Vec::new);
        let picked = &mut draft.picked[prompt];
        match picked.iter().position(|chosen| *chosen == option) {
            Some(at) => {
                picked.remove(at);
            }
            None if multi => picked.push(option),
            None => *picked = vec![option],
        }
        // A single question with a single answer needs nothing more than the click.
        if prompts == 1 && !multi {
            self.submit_answers(cx);
        } else {
            cx.notify();
        }
    }

    fn accepts_input(&self) -> bool {
        self.chat_id.is_some() && !self.deleted()
    }
}

impl Creation {
    fn chat(&self) -> &NewChat {
        match self {
            Self::Pending(chat) | Self::Failed(chat, _) => chat,
        }
    }

    fn into_chat(self) -> NewChat {
        match self {
            Self::Pending(chat) | Self::Failed(chat, _) => chat,
        }
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.activate(window, cx);
        let look = Look::of(cx);
        let colors = look.colors;
        if self.window_handle != window.window_handle() {
            self.transcript_selection.retire(self.window_handle, cx);
            // A participant and its refresh subscription belong to one
            // window. Old-frame sweeping must never clear the new window's
            // selection through a reused participant entity.
            self.transcript_selection =
                select::TranscriptSelection::new(window.window_handle(), cx.weak_entity(), cx);
            self.window_handle = window.window_handle();
        }
        self.follow_text_size(cx);
        self.refresh_selection_document(look, cx);
        self.ready_inputs(window, cx);
        if self.focus_composer {
            self.focus_composer = false;
            self.focus(window, cx);
        }
        let body = if self.chat_id.is_none() {
            self.render_starting(look, cx)
        } else if self.deleted() {
            self.render_deleted(look, cx)
        } else {
            self.render_chat(look, window, cx)
        };
        div()
            .id("chat-view")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(colors.bg))
            .text_color(rgb(colors.text))
            // Menlo in the colorful themes; Native's interface face, with code, diffs and
            // output set in `ui_text::code_family` where they are drawn.
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(12.0))
            .track_focus(&self.focus)
            .key_context("ChatView")
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::interrupt_action))
            .on_action(cx.listener(Self::dictation_action))
            .on_action(cx.listener(Self::copy_transcript))
            .on_key_up(cx.listener(|view, event: &gpui::KeyUpEvent, _, _| {
                if matches!(event.keystroke.key.as_str(), "enter" | "return") {
                    view.enter_down = false;
                    view.enter_repeated = false;
                }
                if event.keystroke.key == "escape" {
                    view.escape_down = false;
                }
            }))
            .capture_action(cx.listener(Self::escape_action))
            // Scope ownership only: Base's window layer handles every gesture.
            .capture_any_mouse_down(cx.listener(
                |view, event: &gpui::MouseDownEvent, window, cx| {
                    if event.button == gpui::MouseButton::Left {
                        crate::behavior_controls::activate_content_scope(
                            view.transcript_selection.scope,
                            window,
                            cx,
                        );
                    }
                },
            ))
            .child(body)
    }
}
