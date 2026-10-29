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

    /// The handshake every recording starts with (initialize, thread/start).
    const HANDSHAKE: &str = include_str!("fixture_handshake.ndjson");
    /// Calls in the synthesized large turn (the D-PERF-1 fixture size).
    pub const LARGE_TURN_CALLS: usize = 2200;

    /// One turn of `LARGE_TURN_CALLS` read-only commands and a short answer,
    /// in the recorded Codex app-server frame format, for "read everything".
    pub fn large_turn() -> String {
        use serde_json::json;
        let mut reply = vec![
            json!({"id": "$id", "result": {"turn": {"id": "turn-1", "items": [], "itemsView": "notLoaded", "status": "inProgress", "error": null}}}),
            json!({"method": "turn/started", "params": {"threadId": "thread-1", "turn": {"id": "turn-1", "items": [], "itemsView": "notLoaded", "status": "inProgress", "error": null}}}),
        ];
        let output = "bounded output line\n".repeat(20);
        for n in 0..LARGE_TURN_CALLS {
            let id = format!("exec-{n}");
            let item = |status: &str, output: Option<&str>, exit: Option<i32>| {
                json!({"type": "commandExecution", "id": id, "command": "cat src/main.rs", "cwd": "/work",
                    "processId": null, "source": "agent", "status": status,
                    "commandActions": [{"type": "read", "command": "cat src/main.rs", "name": "main.rs", "path": "src/main.rs"}],
                    "aggregatedOutput": output, "exitCode": exit, "durationMs": null})
            };
            reply.push(json!({"method": "item/started", "params": {"item": item("inProgress", None, None), "threadId": "thread-1", "turnId": "turn-1", "startedAtMs": 1}}));
            reply.push(json!({"method": "item/completed", "params": {"item": item("completed", Some(&output), Some(0)), "threadId": "thread-1", "turnId": "turn-1", "completedAtMs": 2}}));
        }
        for (method, text) in [
            ("item/started", ""),
            ("item/completed", "Read 2,200 files."),
        ] {
            reply.push(json!({"method": method, "params": {"item": {"type": "agentMessage", "id": "msg-1", "text": text, "phase": "final_answer", "memoryCitation": null, "delivery": null, "questions": null}, "threadId": "thread-1", "turnId": "turn-1", "startedAtMs": 1}}));
        }
        reply.push(json!({"method": "turn/completed", "params": {"threadId": "thread-1", "turn": {"id": "turn-1", "items": [], "itemsView": "notLoaded", "status": "completed", "error": null}}}));
        let turn = json!({"type": "expect", "frame": {"id": "<any>", "method": "turn/start", "params": {"threadId": "thread-1", "input": [{"type": "text", "text": "read everything"}]}}, "reply": reply});
        format!("{HANDSHAKE}{turn}\n")
    }

    /// Synthetic Cua-shaped MCP image results, used by bridge and TSX tests.
    pub fn image_turn() -> String {
        use base64::Engine as _;
        use serde_json::json;
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(12, 8, image::Rgba([200, 40, 10, 255]))
            .write_to(&mut png, image::ImageFormat::Png).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        let item = json!({"type":"mcpToolCall", "id":"cua-shot", "server":"cua-driver",
            "tool":"get_window_state", "status":"completed", "arguments":{},
            "result":{"content":[
                {"type":"image", "mimeType":"image/png", "data":encoded},
                {"type":"image", "mimeType":"image/png", "data":"not-base64"},
                {"type":"localImage", "path":"missing.png"},
                {"type":"localImage", "path":"/unregistered/shot.png"}
            ]}, "error":null});
        let turn = json!({"type":"expect", "frame":{"id":"<any>", "method":"turn/start",
            "params":{"threadId":"thread-1", "input":[{"type":"text", "text":"show screenshots"}]}},
            "reply":[
                {"id":"$id", "result":{"turn":{"id":"turn-1", "items":[], "itemsView":"notLoaded", "status":"inProgress", "error":null}}},
                {"method":"turn/started", "params":{"threadId":"thread-1", "turn":{"id":"turn-1", "items":[], "itemsView":"notLoaded", "status":"inProgress", "error":null}}},
                {"method":"item/completed", "params":{"item":item, "threadId":"thread-1", "turnId":"turn-1"}},
                {"method":"turn/completed", "params":{"threadId":"thread-1", "turn":{"id":"turn-1", "items":[], "itemsView":"notLoaded", "status":"completed", "error":null}}}
            ]});
        format!("{HANDSHAKE}{turn}\n")
    }

    /// Answer deltas `streaming_turn` sends, 100 ms apart.
    pub const STREAM_CHUNKS: usize = 40;

    /// A turn whose answer streams for seconds below a screenshot: the
    /// activity row's clock ticks while the answer's Markdown grows, as in
    /// a live turn. Every eighth chunk ends a paragraph (`chunk 8.`).
    pub fn streaming_turn() -> String {
        use base64::Engine as _;
        use serde_json::json;
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(12, 8, image::Rgba([200, 40, 10, 255]))
            .write_to(&mut png, image::ImageFormat::Png).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        let shot = json!({"type":"mcpToolCall", "id":"cua-shot", "server":"cua-driver",
            "tool":"get_window_state", "status":"completed", "arguments":{},
            "result":{"content":[{"type":"image", "mimeType":"image/png", "data":encoded}]}, "error":null});
        let message = |text: &str| json!({"type":"agentMessage", "id":"msg-1", "text":text, "phase":"final_answer",
            "memoryCitation":null, "delivery":null, "questions":null});
        let started = json!({"id":"turn-1", "items":[], "itemsView":"notLoaded", "status":"inProgress", "error":null});
        let turn = json!({"type":"expect", "frame":{"id":"<any>", "method":"turn/start",
            "params":{"threadId":"thread-1", "input":[{"type":"text", "text":"stream an answer"}]}},
            "reply":[
                {"id":"$id", "result":{"turn":started}},
                {"method":"turn/started", "params":{"threadId":"thread-1", "turn":started}},
                {"method":"item/completed", "params":{"item":shot, "threadId":"thread-1", "turnId":"turn-1"}},
                {"method":"item/started", "params":{"item":message(""), "threadId":"thread-1", "turnId":"turn-1", "startedAtMs":1}}
            ]});
        let mut out = format!("{HANDSHAKE}{turn}\n");
        let mut text = String::new();
        let mut step = |value: serde_json::Value| { out.push_str(&value.to_string()); out.push('\n'); };
        for n in 1..=STREAM_CHUNKS {
            let delta = if n % 8 == 0 { format!("chunk {n}.\n\n") } else { format!("chunk {n} ") };
            text.push_str(&delta);
            step(json!({"type":"sleep", "ms":100}));
            step(json!({"type":"emit", "frame":{"method":"item/agentMessage/delta",
                "params":{"threadId":"thread-1", "turnId":"turn-1", "itemId":"msg-1", "delta":delta}}}));
        }
        step(json!({"type":"emit", "frame":{"method":"item/completed",
            "params":{"item":message(text.trim_end()), "threadId":"thread-1", "turnId":"turn-1", "completedAtMs":2}}}));
        step(json!({"type":"emit", "frame":{"method":"turn/completed",
            "params":{"threadId":"thread-1", "turn":{"id":"turn-1", "items":[], "itemsView":"notLoaded", "status":"completed", "error":null}}}}));
        out
    }

    /// Provider warnings a `*_refresh` recording sends while its request waits:
    /// panel deliveries that leave the approvals and questions alone.
    pub const REFRESHES: usize = 8;
    /// The derived recordings: the question, and the approval with a long reason.
    const REFRESHED: [&str; 2] = ["question_refresh", "approval_refresh"];
    const LONG_REASON: &str =
        "May I write the file?\nIt is the build output.\nNothing else changes.\nIt can be undone.";

    /// `base` with [`REFRESHES`] provider warnings 300 ms apart, sent while the
    /// request its turn/start reply opens waits for the user. `reason`, when
    /// given, replaces a command approval's reason.
    pub fn refreshed(base: &str, reason: Option<&str>) -> String {
        use serde_json::{Value, json};
        let mut out = String::new();
        for line in base.lines() {
            let step = serde_json::from_str::<Value>(line).ok();
            let opens = step
                .as_ref()
                .is_some_and(|step| step["frame"]["method"] == "turn/start");
            match (step, reason) {
                (Some(mut step), Some(reason)) if opens => {
                    for frame in step["reply"].as_array_mut().into_iter().flatten() {
                        if frame["method"] == "item/commandExecution/requestApproval" {
                            frame["params"]["reason"] = json!(reason);
                        }
                    }
                    out.push_str(&step.to_string());
                }
                _ => out.push_str(line),
            }
            out.push('\n');
            if opens {
                for n in 1..=REFRESHES {
                    let message = format!("Still working ({n} of {REFRESHES})");
                    for step in [
                        json!({"type": "sleep", "ms": 300}),
                        json!({"type": "emit", "frame": {"method": "warning", "params": {"threadId": "thread-1", "message": message}}}),
                    ] {
                        out.push_str(&step.to_string());
                        out.push('\n');
                    }
                }
            }
        }
        out
    }

    fn recorded(name: &str) -> &'static str {
        RECORDINGS
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, text)| *text)
            .unwrap_or_default()
    }

    /// Every recording a fixture chat may name: the engine's, the large turn,
    /// and the refreshed request recordings.
    pub fn recordings() -> Vec<(&'static str, std::borrow::Cow<'static, str>)> {
        RECORDINGS
            .iter()
            .map(|(name, text)| (*name, std::borrow::Cow::Borrowed(*text)))
            .chain([
                ("large_turn", std::borrow::Cow::Owned(large_turn())),
                ("image_turn", std::borrow::Cow::Owned(image_turn())),
                ("streaming_turn", std::borrow::Cow::Owned(streaming_turn())),
                (
                    REFRESHED[0],
                    std::borrow::Cow::Owned(refreshed(recorded("question"), None)),
                ),
                (
                    REFRESHED[1],
                    std::borrow::Cow::Owned(refreshed(
                        recorded("approval_accept"),
                        Some(LONG_REASON),
                    )),
                ),
            ])
            .collect()
    }

    fn known(name: &str) -> bool {
        name == "large_turn"
            || name == "image_turn"
            || name == "streaming_turn"
            || REFRESHED.contains(&name)
            || RECORDINGS.iter().any(|(known, _)| *known == name)
    }

    pub fn directory(home: &Path, name: &str) -> Result<PathBuf, String> {
        if !known(name) {
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
        for (name, text) in recordings() {
            let fixture = dir.join(format!("{name}.ndjson"));
            fs::write(&fixture, text.as_bytes()).map_err(|error| format!("io: {error}"))?;
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
            .filter(|name| known(name))
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
