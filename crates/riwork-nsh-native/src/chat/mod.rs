//! `Chat` and `ChatHandle` of `riwork/native` (contracts §1.6, Phase 4a).
//!
//! The bridge follows the EXISTING chat host over its socket with the engine
//! client: one follower and one ordered deliverer thread per open handle, and a
//! worker thread that folds events, projects with the engine, and pushes
//! coalesced reset/patch deliveries. Rows are headers; bodies are paged by the
//! List's viewport; streaming text arrives as append/splice ops. Nothing polls:
//! a quiet chat blocks on its socket and its worker on its inbox.

pub mod activity_summary;
pub mod drafts;
pub mod dto;
pub mod feed;
pub mod links;
pub mod markdown;
pub mod service;
pub mod toolbar;
pub mod view;
pub mod worker;

use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
};

use nsh_core::prelude::*;
use riwork_engine::chat::{
    client::Client,
    model::{ApprovalMode, ChatCommand, NewChat, Provider},
    projection::DisplayMode,
};

use crate::Subscription;
use dto::{
    AnswerRequest, ChatDelivery, ChatSummaryDto, ConfigureRequest, DraftDto, NewChatRequest,
    OpenOptions, ReceiptDto, SavedSubmissionDto, SubmitRequest, from_wire, parse_decision,
    wire_name,
};
use feed::{Backoff, Delivery, Feed};
use view::{ChatView, system_clock};
use worker::{Subscriber, WorkerMsg};

pub use service::{ChatBackend, allow_host_start, install_backend};

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| format!("broken: native worker failed: {error}"))?
}

fn with_code(error: String, code: &str) -> String {
    const CODES: [&str; 7] = [
        "refused:",
        "broken:",
        "not_found:",
        "invalid:",
        "conflict:",
        "unavailable:",
        "io:",
    ];
    if CODES.iter().any(|known| error.starts_with(known)) {
        error
    } else {
        format!("{code}: {error}")
    }
}

fn summary(info: riwork_engine::chat::model::ChatInfo) -> ChatSummaryDto {
    let title = if info.title.trim().is_empty() {
        format!("{} chat", view::provider_name(info.provider))
    } else {
        info.title.clone()
    };
    ChatSummaryDto {
        id: info.id,
        title,
        provider: wire_name(&info.provider),
        project_id: info.project_id,
        worktree_id: info.worktree_id,
        cwd: info.cwd.to_string_lossy().into_owned(),
        state: view::state_name(&info.state).0.into(),
        model: info.model,
    }
}

fn mode_of(name: &str) -> Result<DisplayMode, String> {
    match name {
        "normal" => Ok(DisplayMode::Normal),
        "verbose" => Ok(DisplayMode::Verbose),
        other => Err(format!("invalid: unknown display mode {other:?}")),
    }
}

/// The chat host's chats and their lifecycle; `open` follows one.
#[module]
pub struct Chat;

#[module]
impl Chat {
    pub async fn list() -> Result<Vec<ChatSummaryDto>, String> {
        blocking(|| {
            let socket = service::backend().existing()?;
            let chats = Client::connect(&socket)
                .and_then(|mut client| client.list())
                .map_err(|error| format!("unavailable: {error}"))?;
            Ok(chats.into_iter().map(summary).collect())
        })
        .await
    }

    /// Ask the host for a new chat; the host registers its shared tab entry.
    pub async fn create(request: NewChatRequest) -> Result<ChatSummaryDto, String> {
        let provider: Provider = from_wire(&request.provider)?;
        let approval_mode: ApprovalMode = match request.approval_mode.as_deref() {
            Some(mode) => from_wire(mode)?,
            None => ApprovalMode::default(),
        };
        for id in [&request.project_id, &request.worktree_id]
            .into_iter()
            .flatten()
        {
            crate::service::validate_uuid(id)?;
        }
        if request.cwd.trim().is_empty() {
            return Err("invalid: the chat's working directory is empty".into());
        }
        let new = NewChat {
            parent_id: None,
            provider,
            project_id: request.project_id,
            worktree_id: request.worktree_id,
            cwd: PathBuf::from(request.cwd),
            codex_account_id: None,
            title: request.title,
            approval_mode,
            model: request.model,
            effort: request.effort,
            orchestrator: None,
            fast: request.fast,
            contributor_mode: Default::default(),
            contributor_opt_out: false,
            claude_billing: None,
        };
        blocking(move || {
            let socket = service::backend().ensure()?;
            let info = Client::connect(&socket)
                .and_then(|mut client| client.create(new))
                .map_err(|error| with_code(error, "refused"))?;
            Ok(summary(info))
        })
        .await
    }

    pub async fn delete(chat_id: String) -> Result<(), String> {
        crate::service::validate_uuid(&chat_id)?;
        blocking(move || {
            let socket = service::backend().ensure()?;
            Client::connect(&socket)
                .and_then(|mut client| client.delete(&chat_id))
                .map_err(|error| with_code(error, "refused"))?;
            Ok(())
        })
        .await
    }

    /// Follow a chat from its first event. Starts its threads; the work they do
    /// (sockets, folding, projection) never runs on the engine thread.
    pub fn open(chat_id: String, options: Option<OpenOptions>) -> Result<ChatHandle, String> {
        crate::service::validate_uuid(&chat_id)?;
        let options = options.unwrap_or_default();
        let mode = options.mode.as_deref().map(mode_of).transpose()?;
        Ok(ChatHandle::start(
            service::backend(),
            chat_id,
            options.history,
            mode,
        ))
    }

    /// Debug/test fixtures only: the isolated working directory whose name picks
    /// a recorded provider stream. Rejects unless fixture mode is on.
    pub async fn fixture_directory(name: String) -> Result<String, String> {
        blocking(move || {
            service::fixture_directory(&name).map(|dir| dir.to_string_lossy().into_owned())
        })
        .await
    }
}

/// One open chat. Its subscription delivers a `reset` then `patch`es; its
/// commands reach the host in order. `close()` (or disposal) ends its threads.
#[class]
pub struct ChatHandle {
    chat_id: String,
    inbox: Option<mpsc::Sender<WorkerMsg>>,
    commands: Option<mpsc::Sender<Delivery>>,
    backend: Arc<dyn ChatBackend>,
}

/// What an async handle call needs, taken out of the JS instance so nothing
/// holds it across an await (and Rust tests can drive a handle directly).
#[derive(Clone)]
pub struct Parts {
    pub chat_id: String,
    pub commands: Option<mpsc::Sender<Delivery>>,
    pub backend: Arc<dyn ChatBackend>,
}

impl ChatHandle {
    pub fn start(
        backend: Arc<dyn ChatBackend>,
        chat_id: String,
        history: bool,
        mode: Option<DisplayMode>,
    ) -> Self {
        let (inbox, receiver) = mpsc::channel();
        let handle_backend = backend.clone();
        let delivery: feed::Ensure = {
            let backend = backend.clone();
            Arc::new(move || backend.ensure())
        };
        let follow: feed::Ensure = if history {
            let backend = backend.clone();
            Arc::new(move || backend.existing())
        } else {
            delivery.clone()
        };
        let feed = Feed::start(
            delivery,
            follow,
            chat_id.clone(),
            0,
            worker::sink(inbox.clone()),
            Backoff::DEFAULT,
        );
        let commands = feed.commands();
        let id = chat_id.clone();
        let view = ChatView::new(chat_id.clone(), mode.unwrap_or_default(), system_clock());
        let init: Box<dyn FnOnce(&mut ChatView) + Send> = Box::new(move |view| {
            let Ok(home) = backend.home() else {
                return;
            };
            if let Err(error) = drafts::init(&home) {
                eprintln!("riwork: chat drafts: {error}");
            }
            if mode.is_none()
                && let Ok(settings) =
                    riwork_core::settings::SettingsStore::open(home).and_then(|store| store.load())
            {
                view.set_mode(match settings.chat_display_for(Some(&id)) {
                    riwork_core::settings::DisplayMode::Normal => DisplayMode::Normal,
                    riwork_core::settings::DisplayMode::Verbose => DisplayMode::Verbose,
                });
            }
        });
        let mut feed = Some(feed);
        let deleted = chat_id.clone();
        worker::spawn(
            view,
            receiver,
            init,
            // A deleted chat's draft is forgotten for the rest of the run (CHATC-064).
            Arc::new(move |link| {
                if link == feed::Link::Deleted {
                    drafts::forget(&deleted);
                }
            }),
            Box::new(move || {
                if let Some(mut feed) = feed.take() {
                    feed.stop();
                }
            }),
        );
        Self {
            chat_id,
            inbox: Some(inbox),
            commands,
            backend: handle_backend,
        }
    }

    /// Rust-side access for the bridge tests and the JS methods below.
    pub fn post(&self, message: WorkerMsg) {
        if let Some(inbox) = &self.inbox {
            let _ = inbox.send(message);
        }
    }

    pub fn parts(&self) -> Parts {
        Parts {
            chat_id: self.chat_id.clone(),
            commands: self.commands.clone(),
            backend: self.backend.clone(),
        }
    }

    /// A Rust subscriber (tests): the same deliveries a JS listener gets.
    pub fn subscribe_channel(&self) -> async_channel::Receiver<ChatDelivery> {
        let (subscriber, receiver) = Subscriber::pair();
        self.post(WorkerMsg::Subscribe(subscriber));
        receiver
    }
}

fn receipt(result: Result<(), String>) -> ReceiptDto {
    match result {
        Ok(()) => ReceiptDto {
            status: "submitted".into(),
            error: None,
            key: None,
            clear: true,
        },
        Err(error) if error.starts_with("refused:") => ReceiptDto {
            status: "refused".into(),
            error: Some(error.trim_start_matches("refused:").trim().to_owned()),
            key: None,
            clear: false,
        },
        Err(error) => ReceiptDto {
            status: "uncertain".into(),
            error: Some(error.trim_start_matches("broken:").trim().to_owned()),
            key: None,
            clear: false,
        },
    }
}

impl Parts {
    /// Queue a command behind earlier ones and wait for its exchange, off the
    /// engine thread. `once`: a single exchange that is never resent.
    pub async fn exchange(&self, command: ChatCommand, once: bool) -> Result<(), String> {
        let commands = self.commands.clone().ok_or("refused: the chat is closed")?;
        let (receipt, answer) = tokio::sync::oneshot::channel();
        let delivery = if once {
            Delivery::Submission { command, receipt }
        } else {
            Delivery::Command {
                command,
                receipt: Some(receipt),
            }
        };
        commands
            .send(delivery)
            .map_err(|_| "refused: the chat delivery queue is closed".to_owned())?;
        answer
            .await
            .map_err(|_| "broken: the chat delivery thread ended".to_owned())?
    }

    /// Send the composer's text exactly once (it steers a running turn). Its
    /// recovery snapshot is on disk before the provider can see it; the receipt
    /// says whether the composer may clear (CHATC-032..035, CHATC-059..062).
    pub async fn submit(&self, request: SubmitRequest) -> ReceiptDto {
        let text = request.text.trim().to_owned();
        if text.is_empty() {
            return receipt(Err("refused: the draft is empty".into()));
        }
        let key = uuid::Uuid::new_v4().to_string();
        let owner = (request.editor, request.generation);
        let snapshot = drafts::Contents {
            text: request.text,
            ..Default::default()
        };
        let admitted = {
            let (chat_id, key, backend) = (self.chat_id.clone(), key.clone(), self.backend.clone());
            blocking(move || {
                drafts::init(&backend.home()?)?;
                let barrier = drafts::begin_submission(
                    &chat_id,
                    owner,
                    drafts::SavedSubmission {
                        key: key.clone(),
                        snapshot,
                        status: drafts::SubmissionStatus::Pending,
                    },
                )
                .map_err(|error| format!("refused: {error}"))?;
                let durable = matches!(
                    barrier.recv_timeout(std::time::Duration::from_secs(10)),
                    Ok(Ok(()))
                );
                if !durable {
                    let error = "Could not save the submission recovery snapshot; nothing was sent";
                    drafts::settle_submission(
                        &chat_id,
                        &key,
                        drafts::SubmissionStatus::Refused(error.into()),
                    );
                    return Err(format!("refused: {error}"));
                }
                if !drafts::pending_submission(&chat_id, &key) {
                    return Err("refused: The saved submission was dismissed or the chat became unavailable; nothing was sent".into());
                }
                Ok(())
            })
            .await
        };
        let result = match admitted {
            Ok(()) => self.exchange(ChatCommand::Send { text }, true).await,
            Err(error) => Err(error),
        };
        let status = match &result {
            Ok(()) => drafts::SubmissionStatus::Submitted,
            Err(error) if error.starts_with("refused:") => drafts::SubmissionStatus::Refused(
                error.trim_start_matches("refused:").trim().into(),
            ),
            Err(error) => drafts::SubmissionStatus::Uncertain(
                error.trim_start_matches("broken:").trim().into(),
            ),
        };
        let (chat_id, settled) = (self.chat_id.clone(), key.clone());
        let _ = blocking(move || {
            drafts::settle_submission(&chat_id, &settled, status);
            Ok(())
        })
        .await;
        let mut receipt = receipt(result);
        receipt.key = Some(key);
        receipt
    }

    /// The saved draft and unresolved submissions (CHATC-052). A submission
    /// that was still pending comes back Uncertain.
    pub async fn restore_draft(&self) -> Result<Option<DraftDto>, String> {
        let (chat_id, backend) = (self.chat_id.clone(), self.backend.clone());
        blocking(move || {
            drafts::init(&backend.home()?)?;
            Ok(
                drafts::recover(&chat_id).map(|(record, warnings)| DraftDto {
                    text: record.draft.text,
                    submissions: record
                        .submissions
                        .into_iter()
                        .map(|saved| {
                            let (status, error) = match saved.status {
                                drafts::SubmissionStatus::Pending => ("pending", None),
                                drafts::SubmissionStatus::Submitted => ("submitted", None),
                                drafts::SubmissionStatus::Refused(error) => {
                                    ("refused", Some(error))
                                }
                                drafts::SubmissionStatus::Uncertain(error) => {
                                    ("uncertain", Some(error))
                                }
                            };
                            SavedSubmissionDto {
                                key: saved.key,
                                text: saved.snapshot.text,
                                status: status.into(),
                                error,
                            }
                        })
                        .collect(),
                    warnings,
                }),
            )
        })
        .await
    }

    pub async fn configure(&self, request: ConfigureRequest) -> Result<(), String> {
        let approval_mode = request
            .approval_mode
            .as_deref()
            .map(from_wire::<ApprovalMode>)
            .transpose()?;
        self.exchange(
            ChatCommand::Configure {
                model: request.model,
                effort: request.effort,
                approval_mode,
                fast: request.fast,
                contributor_mode: None,
                contributor_opt_out: None,
                claude_billing: None,
            },
            false,
        )
        .await
    }
}

#[class]
impl ChatHandle {
    /// The chat id this handle follows.
    pub fn chat_id(&self) -> String {
        self.chat_id.clone()
    }

    /// First a `reset`, then coalesced `patch`es (at most one per 16 ms), until
    /// closed or the rendering owner goes. A second subscription replaces the first.
    pub fn subscribe(
        this: Instance<Self>,
        listener: Callback<ChatDelivery>,
        cx: CallCx<'_>,
    ) -> Subscription {
        let receiver = this.with(|handle| handle.subscribe_channel());
        Subscription::start(cx, async move {
            while let Ok(delivery) = receiver.recv().await {
                listener.post(delivery);
                // One delivery per engine turn; a quiet chat awaits the channel.
                next_frame().await;
            }
        })
    }

    /// The List's visible rows; bodies near them are materialized.
    pub fn set_viewport(&mut self, first: u32, last: u32) {
        self.post(WorkerMsg::Viewport(first as usize, last as usize));
    }

    /// Open or close one disclosure row (manual override, CHATT-027).
    pub fn toggle(&mut self, row_id: String) {
        self.post(WorkerMsg::Toggle(row_id));
    }

    /// Expand or collapse the rows present now (Normal: activity rows only).
    pub fn expand_all(&mut self, open: bool) {
        self.post(WorkerMsg::ExpandAll(open));
    }

    pub fn set_mode(&mut self, mode: String) -> Result<(), String> {
        self.post(WorkerMsg::Mode(mode_of(&mode)?));
        Ok(())
    }

    /// Send the composer's text exactly once; it steers a running turn.
    pub async fn submit(
        this: Instance<Self>,
        request: SubmitRequest,
    ) -> Result<ReceiptDto, String> {
        let parts = this.with(|handle| handle.parts());
        Ok(parts.submit(request).await)
    }

    /// ⌘. and Stop: interrupt the running turn (CHATT-020).
    pub async fn interrupt(this: Instance<Self>) -> Result<(), String> {
        let parts = this.with(|handle| handle.parts());
        parts.exchange(ChatCommand::Interrupt, false).await
    }

    /// End the agent's process; the next message resumes it (CHATC-039).
    pub async fn stop(this: Instance<Self>) -> Result<(), String> {
        let parts = this.with(|handle| handle.parts());
        parts.exchange(ChatCommand::Stop, false).await
    }

    /// Answer a pending approval with one of its offered decisions.
    pub async fn approve(
        this: Instance<Self>,
        request_id: String,
        decision: String,
    ) -> Result<(), String> {
        let decision = parse_decision(&decision)?;
        let parts = this.with(|handle| handle.parts());
        parts
            .exchange(
                ChatCommand::Approve {
                    request_id,
                    decision,
                },
                false,
            )
            .await
    }

    /// Answers to a pending question, exchanged exactly once (CHATC-041).
    pub async fn answer(
        this: Instance<Self>,
        request: AnswerRequest,
    ) -> Result<ReceiptDto, String> {
        let parts = this.with(|handle| handle.parts());
        let result = parts
            .exchange(
                ChatCommand::Answer {
                    request_id: request.request_id,
                    answers: request.answers,
                },
                true,
            )
            .await;
        Ok(receipt(result))
    }

    /// Model, effort, Fast or approval mode (CHATC-151..167).
    pub async fn configure(this: Instance<Self>, request: ConfigureRequest) -> Result<(), String> {
        let parts = this.with(|handle| handle.parts());
        parts.configure(request).await
    }

    /// Move this conversation to another provider (refused while a turn runs).
    pub async fn switch_provider(
        this: Instance<Self>,
        provider: String,
        model: Option<String>,
    ) -> Result<(), String> {
        let provider: Provider = from_wire(&provider)?;
        let parts = this.with(|handle| handle.parts());
        parts
            .exchange(
                ChatCommand::Switch {
                    provider,
                    model,
                    effort: None,
                    fast: None,
                },
                false,
            )
            .await
    }

    /// The saved draft and unresolved submissions, for an empty composer.
    pub async fn restore_draft(this: Instance<Self>) -> Result<Option<DraftDto>, String> {
        let parts = this.with(|handle| handle.parts());
        parts.restore_draft().await
    }

    /// Every composer change saves the whole draft at once; the native writer
    /// coalesces to the newest per chat (CHATC-050). Attachments and references
    /// saved with the draft by another build are kept.
    pub fn save_draft(&mut self, text: String, editor: u64, generation: u64) {
        self.post(save_draft_work(
            self.chat_id.clone(),
            text,
            (editor, generation),
        ));
    }

    /// Forget a refused or uncertain saved submission (its card's Dismiss).
    pub fn dismiss_submission(&mut self, key: String) {
        let chat_id = self.chat_id.clone();
        self.post(WorkerMsg::Run(Box::new(move |_| {
            drafts::dismiss_submission(&chat_id, &key);
        })));
    }

    /// Stop following. Commands already queued are still delivered.
    pub fn close(&mut self) {
        self.post(WorkerMsg::Close);
        self.inbox = None;
        self.commands = None;
    }
}

/// A draft save as worker work, in order with the handle's other requests.
pub fn save_draft_work(chat_id: String, text: String, owner: drafts::Owner) -> WorkerMsg {
    WorkerMsg::Run(Box::new(move |_| {
        let (contents, unready) = drafts::with(|store| {
            store
                .record(&chat_id)
                .map(|record| (record.draft.clone(), record.unready_attachments))
        })
        .flatten()
        .unwrap_or_default();
        let contents = drafts::Contents { text, ..contents };
        drafts::remember_complete(&chat_id, contents, owner, unready);
    }))
}

impl Drop for ChatHandle {
    fn drop(&mut self) {
        if let Some(inbox) = self.inbox.take() {
            let _ = inbox.send(WorkerMsg::Close);
        }
    }
}
