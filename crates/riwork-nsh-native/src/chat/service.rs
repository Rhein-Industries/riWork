//! How the bridge reaches this home's chat host (`riwork chat serve`).
//!
//! Following history only resolves the socket; explicit work (create, send)
//! ensures the host. Only the RiWork desktop executable may start a host: its
//! `nsh_host` calls [`allow_host_start`], because `riwork chat ensure` runs the
//! current executable, which in a test runner is the test harness. Tests install
//! their own backend; debug/test-support builds may opt into an in-process
//! fixture host on an isolated `/tmp` home that replays recorded provider
//! streams (`RIWORK_NSH_CHAT_FIXTURE=1`).

use std::{
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use riwork_engine::chat::client::socket_path;

pub trait ChatBackend: Send + Sync {
    fn home(&self) -> Result<PathBuf, String>;
    /// The socket of a host that may be running; never starts one.
    fn existing(&self) -> Result<PathBuf, String> {
        self.home().map(|home| socket_path(&home))
    }
    /// The socket of a running host, starting one when this process may.
    fn ensure(&self) -> Result<PathBuf, String>;
}

static BACKEND: OnceLock<Arc<dyn ChatBackend>> = OnceLock::new();
static HOST_START: AtomicBool = AtomicBool::new(false);

/// Called once by the desktop host: this executable answers `riwork chat ensure`.
pub fn allow_host_start() {
    HOST_START.store(true, Ordering::SeqCst);
}

/// Test seam: one backend per process, like `install_process_service`.
pub fn install_backend(backend: Arc<dyn ChatBackend>) -> Result<(), String> {
    BACKEND
        .set(backend)
        .map_err(|_| "conflict: the chat backend is already installed".into())
}

pub fn backend() -> Arc<dyn ChatBackend> {
    BACKEND.get_or_init(|| Arc::new(ProcessChatBackend)).clone()
}

/// The production backend: this process's RIWORK_HOME.
pub struct ProcessChatBackend;

impl ChatBackend for ProcessChatBackend {
    fn home(&self) -> Result<PathBuf, String> {
        crate::service::process_home()
    }

    fn ensure(&self) -> Result<PathBuf, String> {
        let home = self.home()?;
        #[cfg(any(debug_assertions, feature = "test-support"))]
        if let Some(socket) = fixture::socket(&home)? {
            return Ok(socket);
        }
        let socket = socket_path(&home);
        if UnixStream::connect(&socket).is_ok() {
            return Ok(socket);
        }
        if !HOST_START.load(Ordering::SeqCst) {
            return Err("unavailable: the chat host is not running".into());
        }
        riwork_engine::chat::host::ensure_host(&home)
            .map_err(|error| format!("unavailable: {error}"))
    }
}

/// The isolated fixture directory a test chat runs in, when fixture mode is on.
pub fn fixture_directory(name: &str) -> Result<PathBuf, String> {
    #[cfg(any(debug_assertions, feature = "test-support"))]
    {
        let home = backend().home()?;
        if fixture::socket(&home)?.is_some() {
            return fixture::directory(&home, name);
        }
    }
    let _ = name;
    Err("unavailable: chat fixtures are off".into())
}

#[cfg(any(debug_assertions, feature = "test-support"))]
pub mod fixture {
    //! An in-process chat host whose Codex driver is the real one, talking to
    //! `fake_provider.py` replaying the engine's recorded `testdata/codex`
    //! streams. A chat's working directory `<home>/work/<fixture>` names its
    //! recording. Never runs outside an isolated `/tmp` home.

    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::{Mutex, OnceLock},
    };

    use riwork_engine::chat::{
        driver::DriverConfig,
        host::{Host, Options, Providers},
        model::{ChatInfo, Provider},
    };

    const FAKE_PROVIDER: &str =
        include_str!("../../../riwork-engine/src/chat/testdata/fake_provider.py");
    pub const RECORDINGS: [(&str, &str); 6] = [
        (
            "turn",
            include_str!("../../../riwork-engine/src/chat/testdata/codex/turn.ndjson"),
        ),
        (
            "approval_accept",
            include_str!("../../../riwork-engine/src/chat/testdata/codex/approval_accept.ndjson"),
        ),
        (
            "approval_decline",
            include_str!("../../../riwork-engine/src/chat/testdata/codex/approval_decline.ndjson"),
        ),
        (
            "interrupt",
            include_str!("../../../riwork-engine/src/chat/testdata/codex/interrupt.ndjson"),
        ),
        (
            "question",
            include_str!("../../../riwork-engine/src/chat/testdata/codex/question.ndjson"),
        ),
        (
            "steer",
            include_str!("../../../riwork-engine/src/chat/testdata/codex/steer.ndjson"),
        ),
    ];

    static HOST: OnceLock<Mutex<Option<(PathBuf, Host)>>> = OnceLock::new();
    static PROVIDERS: OnceLock<PathBuf> = OnceLock::new();

    fn enabled(home: &Path) -> bool {
        std::env::var_os("RIWORK_NSH_CHAT_FIXTURE").as_deref() == Some(std::ffi::OsStr::new("1"))
            && isolated(home)
    }

    /// Only a home under /tmp (the W1 fixture rule).
    pub fn isolated(home: &Path) -> bool {
        let home = home.canonicalize().unwrap_or_else(|_| home.to_owned());
        home.starts_with("/tmp") || home.starts_with("/private/tmp")
    }

    pub fn directory(home: &Path, name: &str) -> Result<PathBuf, String> {
        if !RECORDINGS.iter().any(|(known, _)| *known == name) {
            return Err(format!("not_found: no recorded provider stream {name:?}"));
        }
        let dir = home.join("work").join(name);
        fs::create_dir_all(&dir).map_err(|error| format!("io: {error}"))?;
        Ok(dir)
    }

    /// The fixture host's socket, starting it on first use; `None` when off.
    pub fn socket(home: &Path) -> Result<Option<PathBuf>, String> {
        if !enabled(home) {
            return Ok(None);
        }
        let slot = HOST.get_or_init(|| Mutex::new(None));
        let mut slot = slot.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((socket, _)) = slot.as_ref() {
            return Ok(Some(socket.clone()));
        }
        let providers = write_providers(home)?;
        let _ = PROVIDERS.set(providers);
        let host = Host::start(
            home,
            Providers {
                codex: riwork_engine::chat::codex::start,
                claude: riwork_engine::chat::codex::start,
                muse: riwork_engine::chat::codex::start,
                account: no_account,
                config: replay_config,
            },
            Options::default(),
        )
        .map_err(|error| format!("unavailable: fixture chat host: {error:?}"))?;
        let socket = riwork_engine::chat::client::socket_path(home);
        *slot = Some((socket.clone(), host));
        Ok(Some(socket))
    }

    /// One wrapper program per recording: `fake_provider.py RECORD FIXTURE -- ARGS`.
    pub fn write_providers(home: &Path) -> Result<PathBuf, String> {
        let dir = home.join("fixture-providers");
        fs::create_dir_all(&dir).map_err(|error| format!("io: {error}"))?;
        let script = dir.join("fake_provider.py");
        fs::write(&script, FAKE_PROVIDER).map_err(|error| format!("io: {error}"))?;
        for (name, text) in RECORDINGS {
            let fixture = dir.join(format!("{name}.ndjson"));
            fs::write(&fixture, text).map_err(|error| format!("io: {error}"))?;
            let program = dir.join(name);
            fs::write(
                &program,
                format!(
                    "#!/bin/sh\nexec /usr/bin/python3 '{}' '{}' '{}' -- \"$@\"\n",
                    script.display(),
                    dir.join(format!("{name}.record.jsonl")).display(),
                    fixture.display()
                ),
            )
            .map_err(|error| format!("io: {error}"))?;
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755))
                .map_err(|error| format!("io: {error}"))?;
        }
        Ok(dir)
    }

    pub fn set_providers(dir: PathBuf) {
        let _ = PROVIDERS.set(dir);
    }

    pub fn no_account(
        _home: &Path,
        _provider: Provider,
        _project: Option<&str>,
        _requested: Option<&str>,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }

    /// The real Codex driver, started on the recording its directory names.
    pub fn replay_config(
        _home: &Path,
        info: &ChatInfo,
        resume: Option<String>,
    ) -> Result<DriverConfig, String> {
        let providers = PROVIDERS.get().ok_or("no fixture providers")?;
        let name = info
            .cwd
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| RECORDINGS.iter().any(|(known, _)| known == name))
            .unwrap_or("turn");
        Ok(DriverConfig {
            provider: info.provider,
            program: providers.join(name),
            cwd: info.cwd.clone(),
            approval_mode: info.approval_mode,
            model: info.model.clone(),
            effort: info.effort.clone(),
            fast: info.fast,
            contributor_mode: info.contributor_mode,
            trust_workspace: false,
            resume,
            resume_has_history: false,
            reporter: Default::default(),
            outstanding_notices: Default::default(),
            extra_args: Vec::new(),
            instructions: None,
            env: Vec::new(),
            env_remove: Vec::new(),
        })
    }
}
