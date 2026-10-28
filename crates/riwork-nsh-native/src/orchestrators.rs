//! `Orchestrators` of `riwork/native`: the global and project orchestrators as
//! they are, and the explicit Start (contracts §1.10; parity SHELL-053..058,
//! SHELL-217..220).
//!
//! Only `start` may make one, and only as `orchestrators::create` would: the
//! scope's live one is reused whatever Settings say now; creators of a scope
//! take turns in every process (`lock_scope`). `find` and `plan` start nothing.
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use nsh_core::prelude::*;
use riwork_core::{
    settings::{AUTOMATIC_ORCHESTRATORS, OrchestratorRuns},
    store::Store,
};
use riwork_engine::{
    chat::model::{ChatInfo, OrchestratorScope},
    orchestrators::{self, ChatHost, Created, Orchestrator},
    sessions::{SessionManager, orchestrator_context},
};
use serde::{Deserialize, Serialize};

use crate::chat::{dto::wire_name, view::state_name};

/// An orchestrator as a window shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorDto {
    /// `null` for the global orchestrator, else its project's id.
    pub project_id: Option<String>,
    /// `chat` or `terminal`.
    pub kind: String,
    #[prop(skip_none)]
    pub chat_id: Option<String>,
    #[prop(skip_none)]
    pub shell_id: Option<String>,
    /// A chat's provider (`codex`, `claude`, `muse`); a terminal runs Codex.
    #[prop(skip_none)]
    pub provider: Option<String>,
    /// A chat's state word; a terminal's `live` or `exited`.
    pub state: String,
    /// The fixed tab title, "G·ORCH · GLOBAL" or "P·ORCH · PROJECT".
    pub title: String,
    /// Made by this request.
    pub created: bool,
    /// What Automatic could not start first, and what runs instead.
    #[prop(skip_none)]
    pub fallback: Option<String>,
}

/// How a new orchestrator of a scope would run, for the Start button's words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorPlanDto {
    /// `automatic`, `terminal` or `chat`.
    pub mode: String,
    #[prop(skip_none)]
    pub provider: Option<String>,
    #[prop(skip_none)]
    pub model: Option<String>,
    /// Automatic's choices in order: `provider:model`.
    pub automatic: Vec<String>,
    /// Saved choices this build could not read, in words.
    pub notes: Vec<String>,
}

fn scope(project_id: Option<&str>) -> OrchestratorScope {
    orchestrators::scope_of(project_id)
}

pub fn dto(project_id: Option<&str>, made: Created) -> OrchestratorDto {
    let title = orchestrators::tab_title(&scope(project_id)).to_owned();
    let (kind, chat_id, shell_id, provider, state) = match made.orchestrator {
        Orchestrator::Chat(chat) => (
            "chat",
            Some(chat.id.clone()),
            None,
            Some(wire_name(&chat.provider)),
            state_name(&chat.state).0.to_owned(),
        ),
        Orchestrator::Terminal(shell) => (
            "terminal",
            None,
            Some(shell.id.clone()),
            Some("codex".to_owned()),
            if shell.alive { "live" } else { "exited" }.to_owned(),
        ),
    };
    OrchestratorDto {
        project_id: project_id.map(str::to_owned),
        kind: kind.into(),
        chat_id,
        shell_id,
        provider,
        state,
        title,
        created: made.created,
        fallback: made.fallback,
    }
}

/// Blocking adapter; the module always calls it on the blocking pool.
pub trait OrchestratorBackend: Send + Sync {
    fn find(&self, project_id: Option<&str>) -> Result<Option<Orchestrator>, String>;
    fn start(&self, project_id: Option<&str>) -> Result<Created, String>;
    fn plan(&self, project_id: Option<&str>) -> Result<orchestrators::Plan, String>;
}

/// The chat host as the native chat module reaches it: the running host, or
/// one this desktop executable may start (never a test runner).
fn ensure(_: &Path) -> Result<PathBuf, String> {
    crate::chat::service::backend().ensure()
}

pub struct EngineOrchestrators {
    home: PathBuf,
    sessions: OnceLock<SessionManager>,
}

impl EngineOrchestrators {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            sessions: OnceLock::new(),
        }
    }

    fn sessions(&self) -> Result<&SessionManager, String> {
        if let Some(sessions) = self.sessions.get() {
            return Ok(sessions);
        }
        let sessions = SessionManager::at(self.home.clone())?;
        Ok(self.sessions.get_or_init(|| sessions))
    }
}

impl OrchestratorBackend for EngineOrchestrators {
    fn find(&self, project_id: Option<&str>) -> Result<Option<Orchestrator>, String> {
        let host = ChatHost {
            home: &self.home,
            ensure: &ensure,
        };
        orchestrators::find(self.sessions()?, &host, &scope(project_id))
    }

    fn start(&self, project_id: Option<&str>) -> Result<Created, String> {
        let host = ChatHost {
            home: &self.home,
            ensure: &ensure,
        };
        let plan = orchestrators::plan(&self.home, project_id)?;
        let root = project_id
            .map(|id| {
                Store::open(self.home.clone())?
                    .snapshot()?
                    .project(id)
                    .map(|project| project.root.clone())
            })
            .transpose()?;
        let cwd = root
            .clone()
            .unwrap_or_else(|| orchestrator_context(&self.home, None));
        orchestrators::create(
            self.sessions()?,
            &host,
            &scope(project_id),
            root,
            cwd,
            None,
            &plan.runs,
        )
    }

    fn plan(&self, project_id: Option<&str>) -> Result<orchestrators::Plan, String> {
        orchestrators::plan(&self.home, project_id)
    }
}

static BACKEND: OnceLock<Arc<dyn OrchestratorBackend>> = OnceLock::new();

/// Test seam: one backend per process, installed before first use.
pub fn install_backend(backend: Arc<dyn OrchestratorBackend>) -> Result<(), String> {
    BACKEND
        .set(backend)
        .map_err(|_| "conflict: the orchestrator backend is already installed".into())
}

fn backend() -> Result<Arc<dyn OrchestratorBackend>, String> {
    if let Some(backend) = BACKEND.get() {
        return Ok(backend.clone());
    }
    let home = crate::service::process_home()?;
    #[cfg(any(debug_assertions, feature = "test-support"))]
    if riwork_nsh_workspace::session_io::is_fixture(&home) {
        return Ok(BACKEND
            .get_or_init(|| Arc::new(fixture::FixtureOrchestrators { home }))
            .clone());
    }
    Ok(BACKEND
        .get_or_init(|| Arc::new(EngineOrchestrators::new(home)))
        .clone())
}

fn validate(project_id: &Option<String>) -> Result<(), String> {
    project_id
        .as_deref()
        .map(crate::service::validate_uuid)
        .transpose()
        .map(|_| ())
}

#[module]
pub struct Orchestrators;

#[module]
impl Orchestrators {
    /// The scope's orchestrator as it is (`null`: there is none). Starts nothing.
    pub async fn find(project_id: Option<String>) -> Result<Option<OrchestratorDto>, String> {
        validate(&project_id)?;
        crate::service::blocking(move || {
            let found = backend()?.find(project_id.as_deref())?;
            Ok(found.map(|orchestrator| {
                dto(
                    project_id.as_deref(),
                    Created {
                        orchestrator,
                        created: false,
                        fallback: None,
                    },
                )
            }))
        })
        .await
    }

    /// Explicit Start only (a button, ⌘⇧O, ⌘⌥O): the scope's live orchestrator,
    /// else one made as the project or Settings say.
    pub async fn start(project_id: Option<String>) -> Result<OrchestratorDto, String> {
        validate(&project_id)?;
        crate::service::blocking(move || {
            let made = backend()?.start(project_id.as_deref())?;
            Ok(dto(project_id.as_deref(), made))
        })
        .await
    }

    /// How a new orchestrator of the scope would run. Reads Settings and the store.
    pub async fn plan(project_id: Option<String>) -> Result<OrchestratorPlanDto, String> {
        validate(&project_id)?;
        crate::service::blocking(move || {
            let plan = backend()?.plan(project_id.as_deref())?;
            let (mode, provider, model) = match &plan.runs {
                OrchestratorRuns::Automatic => ("automatic", None, None),
                OrchestratorRuns::Terminal => ("terminal", None, None),
                OrchestratorRuns::Chat { provider, model } => {
                    ("chat", Some(wire_name(provider)), model.clone())
                }
            };
            Ok(OrchestratorPlanDto {
                mode: mode.into(),
                provider,
                model,
                automatic: AUTOMATIC_ORCHESTRATORS
                    .iter()
                    .map(|(provider, model)| format!("{}:{model}", wire_name(provider)))
                    .collect(),
                notes: plan.notes,
            })
        })
        .await
    }
}

/// The scope's chat orchestrator among `chats`.
pub fn chat_of<'a>(chats: &'a [ChatInfo], project_id: Option<&str>) -> Option<&'a ChatInfo> {
    let wanted = scope(project_id);
    chats
        .iter()
        .find(|chat| chat.orchestrator.as_ref() == Some(&wanted))
}

#[cfg(any(debug_assertions, feature = "test-support"))]
pub mod fixture {
    //! Fixture homes have no tmux: an orchestrator is a chat of the in-process
    //! fixture host, made with the scope, title and approval mode
    //! `orchestrators::create` gives one. The recording is the `turn` stream.
    use super::*;
    use riwork_engine::chat::{
        client::Client,
        model::{NewChat, OrchestratorScope, Provider},
    };

    pub struct FixtureOrchestrators {
        pub home: PathBuf,
    }

    impl FixtureOrchestrators {
        /// A running fixture host's chats; none when no host runs.
        fn chats(&self) -> Result<Vec<ChatInfo>, String> {
            let socket = crate::chat::service::backend().existing()?;
            Ok(Client::connect(&socket)
                .and_then(|mut client| client.list())
                .unwrap_or_default())
        }
    }

    impl OrchestratorBackend for FixtureOrchestrators {
        fn find(&self, project_id: Option<&str>) -> Result<Option<Orchestrator>, String> {
            Ok(chat_of(&self.chats()?, project_id)
                .cloned()
                .map(Orchestrator::Chat))
        }

        fn start(&self, project_id: Option<&str>) -> Result<Created, String> {
            if let Some(orchestrator) = self.find(project_id)? {
                return Ok(Created {
                    orchestrator,
                    created: false,
                    fallback: None,
                });
            }
            let plan = orchestrators::plan(&self.home, project_id)?;
            let provider = match plan.runs {
                OrchestratorRuns::Terminal => {
                    return Err(
                        "unavailable: fixture homes have no tmux; choose a chat orchestrator"
                            .into(),
                    );
                }
                // The fixture host replays Codex recordings only.
                _ => Provider::Codex,
            };
            let wanted: OrchestratorScope = scope(project_id);
            let cwd = crate::chat::service::fixture_directory("turn")?;
            let new = NewChat {
                parent_id: None,
                provider,
                project_id: project_id.map(str::to_owned),
                worktree_id: None,
                cwd,
                codex_account_id: None,
                title: Some(orchestrators::chat_title(&wanted).to_owned()),
                // The recordings were made in the default mode; the real
                // backend's `create` gives the scope's own mode.
                approval_mode: Default::default(),
                model: None,
                effort: None,
                orchestrator: Some(wanted),
                fast: false,
                contributor_mode: Default::default(),
                contributor_opt_out: false,
                claude_billing: None,
            };
            let socket = crate::chat::service::backend().ensure()?;
            let chat = Client::connect(&socket)
                .and_then(|mut client| client.create(new))
                .map_err(|error| format!("refused: {error}"))?;
            Ok(Created {
                orchestrator: Orchestrator::Chat(chat),
                created: true,
                fallback: None,
            })
        }

        fn plan(&self, project_id: Option<&str>) -> Result<orchestrators::Plan, String> {
            orchestrators::plan(&self.home, project_id)
        }
    }
}
