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
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Bounds, Context, EntityInputHandler, EventEmitter, FocusHandle, FollowMode, KeyDownEvent,
    ListAlignment, ListState, Pixels, Point, Render, ScrollHandle, Task, UTF16Selection, Window,
    actions, div, prelude::*, px, rgb,
};

use crate::{
    chat::{
        client::Client,
        model::{
            ApprovalMode, ChatCommand, ChatInfo, Decision, ItemBody, NewChat, Provider, Question,
        },
    },
    project_settings::Input,
    ui_text, utf16_to_byte,
};

mod approval;
mod attachment_draft;
mod cards;
mod composer;
mod dictate;
mod diff;
mod display;
mod feed;
mod host;
mod links;
mod markdown;
mod media;
mod panels;
mod prose;
mod rows;
mod select;
mod state;
#[cfg(test)]
mod testing;
mod toolbar;
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
}

impl EventEmitter<ChatViewEvent> for ChatView {}

/// A chat the host is still making, or could not make.
enum Creation {
    Pending(NewChat),
    Failed(NewChat, String),
}

/// Which input the keys type into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Composer,
    /// The model name in the model menu.
    Model,
    /// The free-text answer to question number `n` of the question that waits.
    Answer(usize),
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
}

/// Work done on the way to drawing, kept while its input is the same.
#[derive(Default)]
struct Caches {
    /// Parsed messages by item id, with a hash of the text they were parsed from.
    markdown: HashMap<String, (u64, Arc<Vec<markdown::Block>>)>,
    /// Lines added and removed per file of a file change, by item id and the size of the
    /// diffs it was counted at.
    changes: HashMap<String, (usize, cards::FileStats)>,
    /// The lines of a file's diff that are drawn, by item id and file, with the size of the
    /// diff they were read at.
    diffs: HashMap<String, (usize, Arc<DrawnDiff>)>,
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
    /// The host's id for the chat; none while a new chat is being made.
    chat_id: Option<String>,
    creation: Option<Creation>,
    model: ChatModel,
    feed: Option<Feed>,
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
    field: Field,
    composer: Input,
    model_input: Input,
    answers: Vec<Input>,
    composer_scroll: ScrollHandle,
    menu: Option<Menu>,
    /// The menu that a click outside just closed, and when. The click that closes an open
    /// menu on its own button must not open it again.
    menu_closed: Option<(Menu, Instant)>,
    /// Whether the tab had the keys when it was last drawn.
    has_focus: bool,
    /// When the last message was sent: an Enter right after it is not an answer to a request.
    sent_at: Option<Instant>,
    /// Item ids (and `item#n` for the files of a change) that are expanded.
    open: HashSet<String>,
    /// Requests this tab already answered, until the host says they are resolved.
    answered: HashSet<String>,
    drafts: HashMap<String, Draft>,
    /// A line about something that went wrong, until it is dismissed.
    notice: Option<String>,
    /// The copy button that was just pressed, until it is forgotten.
    copied: Option<String>,
    forget_copy: Option<Task<()>>,
    caches: RefCell<Caches>,
    /// What the mouse has selected in the transcript.
    selection: Option<select::Selection>,
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
    pub fn open(chat_id: String, config: HostConfig, cx: &mut Context<Self>) -> Self {
        let mut view = Self::blank(config, cx);
        view.chat_id = Some(chat_id.clone());
        view.follow_display_setting(cx);
        view.start_feed(chat_id, 0, cx);
        view
    }

    /// The tab of a chat that is yet to be made: the host makes it, and the tab follows it.
    pub fn create(chat: NewChat, config: HostConfig, cx: &mut Context<Self>) -> Self {
        let mut view = Self::blank(config, cx);
        view.begin_creation(chat, cx);
        view
    }

    fn blank(config: HostConfig, cx: &mut Context<Self>) -> Self {
        // One row for the footer. Rows are added before it as items arrive, and the list
        // follows the end while the user has not scrolled up.
        let list = ListState::new(1, ListAlignment::Top, px(600.0));
        list.set_follow_mode(FollowMode::Tail);
        // Settings shows or hides the mic in every open chat at once.
        cx.observe_global::<crate::settings::Settings>(|view, cx| {
            view.follow_mic_setting(cx);
            view.follow_display_setting(cx);
        })
        .detach();
        cx.on_release(|view, cx| view.release_images(cx)).detach();
        Self {
            config,
            chat_id: None,
            creation: None,
            model: ChatModel::new(),
            feed: None,
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
            focus: cx.focus_handle(),
            field: Field::Composer,
            composer: Input::default(),
            model_input: Input::default(),
            answers: Vec::new(),
            composer_scroll: ScrollHandle::new(),
            menu: None,
            menu_closed: None,
            has_focus: false,
            sent_at: None,
            open: HashSet::new(),
            answered: HashSet::new(),
            drafts: HashMap::new(),
            notice: None,
            copied: None,
            forget_copy: None,
            caches: RefCell::new(Caches::default()),
            selection: None,
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
        self.field = Field::Composer;
        self.focus.focus(window, cx);
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

    fn begin_creation(&mut self, chat: NewChat, cx: &mut Context<Self>) {
        self.creation = Some(Creation::Pending(chat.clone()));
        let ensure = self.config.ensure.clone();
        let work = cx.background_executor().spawn(async move {
            let socket = ensure()?;
            Client::connect(&socket)?.create(chat)
        });
        self.working = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| view.created(result, cx));
        }));
        cx.notify();
    }

    fn created(&mut self, result: Result<ChatInfo, String>, cx: &mut Context<Self>) {
        let Some(creation) = self.creation.take() else {
            return;
        };
        match result {
            Ok(info) => {
                self.chat_id = Some(info.id.clone());
                if let Some(mode) = self.pending_display.take() {
                    self.choose_display(mode, cx);
                } else {
                    self.follow_display_setting(cx);
                }
                self.start_feed(info.id.clone(), 0, cx);
                cx.emit(ChatViewEvent::Created(info.id));
            }
            Err(error) => self.creation = Some(Creation::Failed(creation.into_chat(), error)),
        }
        self.announce(cx);
        cx.notify();
    }

    fn retry_creation(&mut self, cx: &mut Context<Self>) {
        if let Some(Creation::Failed(chat, _)) = self.creation.take() {
            self.begin_creation(chat, cx);
        }
    }

    fn start_feed(&mut self, chat_id: String, since: u64, cx: &mut Context<Self>) {
        let (sender, receiver) = async_channel::unbounded();
        self.feed = Some(Feed::start(
            self.config.ensure.clone(),
            chat_id,
            since,
            sender,
            Backoff::DEFAULT,
        ));
        self.pump = Some(cx.spawn(async move |this, cx| {
            while let Ok(first) = receiver.recv().await {
                // Everything that is waiting is one redraw, however fast it arrives.
                let mut batch = vec![first];
                while let Ok(more) = receiver.try_recv() {
                    batch.push(more);
                }
                if this.update(cx, |view, cx| view.accept(batch, cx)).is_err() {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
            }
        }));
    }

    fn accept(&mut self, batch: Vec<FeedMsg>, cx: &mut Context<Self>) {
        let mut events = Vec::new();
        for message in batch {
            match message {
                FeedMsg::Events(mut more) => events.append(&mut more),
                FeedMsg::Link(link) => {
                    self.model.link = link;
                    if link == Link::Deleted {
                        self.feed = None;
                    }
                }
                // The Kit composer integration will settle its snapshot from this receipt.
                // Current text-only controls never enqueue SendAttachments.
                FeedMsg::AttachmentSubmission {
                    result: Err(error), ..
                } => {
                    self.notice = Some(error.to_string());
                }
                FeedMsg::AttachmentSubmission { result: Ok(()), .. } => {}
                FeedMsg::CommandFailed { command, error } => {
                    self.command_failed(command, error);
                }
            }
        }
        let applied = self.model.apply(&events);
        self.sync_list(&applied);
        self.forget_settled();
        self.announce(cx);
        cx.notify();
    }

    /// Tell the list about the rows that came or changed. Rows are measured when they are
    /// drawn, so a row that changed must be measured again.
    fn sync_list(&mut self, applied: &Applied) {
        if applied.projection_changed {
            self.refresh_projection();
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

    fn refresh_projection(&mut self) {
        let rows = display::rows(
            &self.model.transcript,
            &self.model.completed,
            self.display_mode,
        );
        if rows != self.visible {
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
            self.selection = None;
            self.refresh_projection();
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
                cx.set_global(settings);
                self.follow_display_setting(cx);
            }
            Err(error) => {
                self.notice = Some(error);
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
    fn follow_text_size(&mut self) {
        let scale = ui_text::scale();
        if scale != self.scale {
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
            None => self.notice = Some("Not connected to the chat yet.".to_owned()),
        }
    }

    fn command_failed(&mut self, command: ChatCommand, error: String) {
        match command {
            // The words go back in the box, unless the user has started on something else.
            ChatCommand::Send { text } if self.composer.text.is_empty() => {
                self.composer = Input::new(text);
            }
            ChatCommand::Approve { request_id, .. } | ChatCommand::Answer { request_id, .. } => {
                self.answered.remove(&request_id);
            }
            _ => {}
        }
        self.notice = Some(format!("Could not reach the chat: {error}"));
    }

    fn send_message(&mut self, cx: &mut Context<Self>) {
        let Some(text) = composer::message(&self.composer.text) else {
            return;
        };
        self.composer = Input::default();
        self.sent_at = Some(Instant::now());
        self.notice = None;
        // Sending is asking to see the answer.
        self.list.set_follow_mode(FollowMode::Tail);
        self.command(ChatCommand::Send { text });
        cx.notify();
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
    fn dictation_action(&mut self, _: &ToggleDictation, _: &mut Window, cx: &mut Context<Self>) {
        if dictate::mic_shown(cx) {
            self.toggle_dictation(cx);
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
        self.field = Field::Composer;
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
        let ensure = self.config.ensure.clone();
        let work = cx.background_executor().spawn(async move {
            let socket = ensure()?;
            Client::connect(&socket)?.delete(&chat_id)
        });
        self.working = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(()) => cx.emit(ChatViewEvent::Close),
                Err(error) => {
                    view.notice = Some(format!("Could not delete the chat: {error}"));
                    cx.notify();
                }
            });
        }));
        cx.notify();
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
            self.field = Field::Composer;
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
            self.field = Field::Composer;
        } else {
            self.menu = Some(menu);
            // The model name is typed only when the driver has not listed its models.
            self.field = if menu == Menu::Model && self.model.transcript.models.is_empty() {
                self.model_input = Input::new(
                    self.model
                        .transcript
                        .info
                        .as_ref()
                        .and_then(|info| info.model.clone())
                        .unwrap_or_default(),
                );
                Field::Model
            } else {
                Field::Composer
            };
        }
        self.focus.focus(window, cx);
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

    /// The decisions a key may make now. None for a key held down, or pressed just after a
    /// message was sent: that Enter was meant for the message, and a bounce of the key must
    /// not allow a command.
    fn offered_to_key(&self, event: &KeyDownEvent) -> Vec<Decision> {
        let just_sent = self
            .sent_at
            .is_some_and(|at| at.elapsed() < Duration::from_millis(700));
        if event.is_held || just_sent {
            Vec::new()
        } else {
            self.offered()
        }
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
        if self.handle_key(event, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.chat_id.is_none() || self.deleted() {
            return false;
        }
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            if self.media.viewer.take().is_some() {
                cx.notify();
                return true;
            }
            if self.menu.is_some() {
                self.close_menu(cx);
                return true;
            }
            // ⎋ while dictating takes out what was dictated, and nothing else.
            if self.dictation.phase().is_active() {
                self.cancel_dictation(cx);
                return true;
            }
            let empty = self.composer.text.trim().is_empty();
            return match composer::escape(empty, &self.offered_to_key(event)) {
                Some(decision) => {
                    if let Some((approval, _)) = self.pending_approval() {
                        let request_id = approval.request_id.clone();
                        self.approve(request_id, decision, cx);
                    }
                    true
                }
                None => false,
            };
        }
        match self.field {
            Field::Composer => {
                let handled = self.composer_key(event, cx);
                if handled {
                    self.keep_cursor_in_view();
                }
                handled
            }
            Field::Model => self.model_key(event, cx),
            Field::Answer(at) => self.answer_key(at, event, cx),
        }
    }

    fn composer_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let mods = event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        match key {
            "enter" | "return" => {
                let action = composer::enter(
                    mods.shift,
                    mods.alt,
                    self.composer.marked.is_some(),
                    self.composer.text.trim().is_empty(),
                    &self.offered_to_key(event),
                );
                match action {
                    composer::Enter::Ignore => return false,
                    composer::Enter::NewLine => self.composer.replace_lines(None, "\n"),
                    composer::Enter::Send => self.send_message(cx),
                    composer::Enter::Approve(decision) => {
                        if let Some((approval, _)) = self.pending_approval() {
                            let request_id = approval.request_id.clone();
                            self.approve(request_id, decision, cx);
                        }
                    }
                }
                true
            }
            "up" | "down" if !mods.platform && !mods.shift && !mods.alt => {
                let cursor = self.composer.cursor();
                if let Some(to) = composer::vertical(&self.composer.text, cursor, key == "up") {
                    place_cursor(&mut self.composer, to);
                }
                true
            }
            "home" | "end" if !mods.shift => {
                let cursor = self.composer.cursor();
                let to = if key == "home" {
                    composer::line_start(&self.composer.text, cursor)
                } else {
                    composer::line_end(&self.composer.text, cursor)
                };
                place_cursor(&mut self.composer, to);
                true
            }
            // What the mouse selected in the transcript, unless the box has a selection of its own.
            "c" if mods.platform && self.composer.selection.is_empty() => self.copy_selection(cx),
            // The same as the key binding, for where the system takes the binding first.
            "." if mods.platform => {
                self.interrupt(cx);
                true
            }
            "d" if mods.control && mods.alt && !mods.platform && dictate::mic_shown(cx) => {
                self.toggle_dictation(cx);
                true
            }
            "v" if mods.platform => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.composer.replace_lines(None, &text);
                }
                true
            }
            _ => self.composer.key(event, cx),
        }
    }

    fn model_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        match event.keystroke.key.as_str() {
            "enter" | "return" => {
                let model = self.model_input.text.trim().to_owned();
                if !model.is_empty() {
                    self.configure(Some(model), None, None, None, cx);
                }
                true
            }
            _ => self.model_input.key(event, cx),
        }
    }

    fn answer_key(&mut self, at: usize, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        match event.keystroke.key.as_str() {
            "enter" | "return" => {
                self.submit_answers(cx);
                true
            }
            _ => match self.answers.get_mut(at) {
                Some(input) => input.key(event, cx),
                None => false,
            },
        }
    }

    // -----------------------------------------------------------------------------------
    // Questions
    // -----------------------------------------------------------------------------------

    /// Pick option `option` of prompt `prompt` of the question that waits. A prompt that takes
    /// one answer is changed to it; one that takes several toggles it.
    fn pick(&mut self, prompt: usize, option: usize, cx: &mut Context<Self>) {
        let Some(question) = self.pending_question() else {
            return;
        };
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

    fn submit_answers(&mut self, cx: &mut Context<Self>) {
        let Some(question) = self.pending_question() else {
            return;
        };
        let request_id = question.request_id.clone();
        if self.answered.contains(&request_id) {
            return;
        }
        let draft = self.drafts.get(&request_id);
        let answers = panels::answers_of(question, draft, &self.answers);
        if answers.iter().any(Vec::is_empty) {
            self.notice = Some("Answer every question first.".to_owned());
            cx.notify();
            return;
        }
        self.answered.insert(request_id.clone());
        self.answers.clear();
        self.field = Field::Composer;
        self.command(ChatCommand::Answer {
            request_id,
            answers,
        });
        cx.notify();
    }

    /// Make room for a free-text answer to each prompt of the question that waits, and take
    /// the keys back from an input that is no longer there.
    fn ready_inputs(&mut self) {
        let wanted = self.pending_question().map_or(0, |q| q.questions.len());
        if self.answers.len() != wanted {
            self.answers.resize_with(wanted, Input::default);
        }
        let gone = match self.field {
            Field::Composer => false,
            Field::Model => self.menu != Some(Menu::Model),
            Field::Answer(at) => at >= wanted,
        };
        if gone {
            self.field = Field::Composer;
        }
    }

    // -----------------------------------------------------------------------------------
    // The active input
    // -----------------------------------------------------------------------------------

    fn input(&self) -> &Input {
        match self.field {
            Field::Composer => &self.composer,
            Field::Model => &self.model_input,
            Field::Answer(at) => self.answers.get(at).unwrap_or(&self.composer),
        }
    }

    fn input_mut(&mut self) -> &mut Input {
        match self.field {
            Field::Composer => &mut self.composer,
            Field::Model => &mut self.model_input,
            Field::Answer(at) => {
                if at < self.answers.len() {
                    &mut self.answers[at]
                } else {
                    &mut self.composer
                }
            }
        }
    }

    fn accepts_input(&self) -> bool {
        self.chat_id.is_some() && !self.deleted()
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        if self.field == Field::Composer {
            self.keep_cursor_in_view();
        }
        cx.notify();
    }

    /// Scroll the message box so that the cursor shows. Lines are not measured: the cursor's
    /// share of the text stands for its share of the height, which is exact at the ends.
    fn keep_cursor_in_view(&self) {
        let (cursor, length) = (self.composer.cursor(), self.composer.text.len());
        let max = self.composer_scroll.max_offset();
        if cursor >= length {
            self.composer_scroll.scroll_to_bottom();
        } else if max.y > px(0.0) {
            let mut offset = self.composer_scroll.offset();
            offset.y = -(max.y * (cursor as f32 / length as f32));
            self.composer_scroll.set_offset(offset);
        }
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

fn place_cursor(input: &mut Input, at: usize) {
    input.selection = at..at;
    input.reversed = false;
    input.marked = None;
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = Look::of(cx);
        let colors = look.colors;
        self.follow_text_size();
        self.ready_inputs();
        self.has_focus = self.focus.is_focused(window);
        let body = if self.chat_id.is_none() {
            self.render_starting(look, cx)
        } else if self.deleted() {
            self.render_deleted(look, cx)
        } else {
            self.render_chat(look, cx)
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
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|view, _, window, cx| {
                    if view.field != Field::Composer && view.menu.is_none() {
                        view.field = Field::Composer;
                    }
                    // A press on selectable text stops here before it gets this far.
                    if view.selection.take().is_some() {
                        cx.notify();
                    }
                    view.focus.focus(window, cx);
                }),
            )
            .child(body)
    }
}

impl EntityInputHandler for ChatView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let input = self.input();
        let start = utf16_to_byte(&input.text, range.start);
        let end = utf16_to_byte(&input.text, range.end);
        *actual = Some(
            input.text[..start].encode_utf16().count()..input.text[..end].encode_utf16().count(),
        );
        Some(input.text[start..end].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let input = self.input();
        Some(UTF16Selection {
            range: input.text[..input.selection.start].encode_utf16().count()
                ..input.text[..input.selection.end].encode_utf16().count(),
            reversed: input.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let input = self.input();
        input.marked.as_ref().map(|range| {
            input.text[..range.start].encode_utf16().count()
                ..input.text[..range.end].encode_utf16().count()
        })
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.input_mut().marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_in_active(range, text);
        self.edited(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = self.replace_in_active(range, text);
        let input = self.input_mut();
        // What was inserted is from where it went in to where the cursor is now.
        let end = input.selection.end;
        input.marked = (end > start).then_some(start..end);
        self.edited(cx);
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.input().text.encode_utf16().count())
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.input().text.encode_utf16().count())
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.accepts_input()
    }
}

impl ChatView {
    /// The message box takes line breaks; the model name and the answers are one line. Where
    /// the text went in, as a byte offset.
    fn replace_in_active(&mut self, range: Option<Range<usize>>, text: &str) -> usize {
        let multiline = self.field == Field::Composer;
        let input = self.input_mut();
        let start = match &range {
            Some(range) => utf16_to_byte(&input.text, range.start),
            None => input.marked.as_ref().unwrap_or(&input.selection).start,
        };
        if multiline {
            input.replace_lines(range, text);
        } else {
            input.replace(range, text);
        }
        start
    }
}
