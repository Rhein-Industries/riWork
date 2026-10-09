//! A desktop device's terminal streams (`pty.*`): a real pseudo-terminal and a
//! real process against a stand-in RiWork CLI whose `shell attach --exec` runs
//! a few lines of `sh`, first through the RPC layer and then end to end through
//! a real relay and the real connector binary.
//!
//! The stand-in plays the part of tmux: it draws something first (so the open
//! answers), then does what the shell id's `mode.ID` file says: echo through
//! `cat`, answer `size` with `stty size`, wait for a window change, exit, flood,
//! refuse in the CLI's words, or sit still.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use riwork_remote::{
    config::{DeviceKind, Pairing, Storage, private_read},
    connector::{connect_registered, receive_json, send_json},
    crypto::{
        Envelope, PairAccept, ServerHelloV2, Session, accept_pair, accept_server_hello_v2, b64,
        client_hello_v2, decode, pair_hello, random32,
    },
    pty::{self, PtySet},
    relay::{Relay, Routes},
    rpc::Rpc,
    viewport::Viewport,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    process::{Child, Command},
    time::{sleep, timeout},
};

fn uuid4() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn request(method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":uuid4(),"method":method,"params":params})
}
fn data(text: &str) -> String {
    b64(text.as_bytes())
}

/// The stand-in CLI. `capabilities --json` says whether it can attach in place
/// (`old-cli` makes it a build that cannot). `shell attach ID --exec ...` logs its
/// arguments and environment, writes its process id, and then acts out `mode.ID`.
fn stand_in_cli(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let cli = dir.join("fake-riwork");
    let script = r#"#!/bin/sh
D='@DIR@'
case "$1 $2" in
'capabilities --json')
  echo capabilities >> "$D/calls.log"
  if [ -e "$D/old-cli" ]; then printf '{"v":1,"verifies_shell":true}'
  else printf '{"v":1,"verifies_shell":true,"shell_attach_exec":true}'; fi;;
'shell attach')
  ID="$3"
  { for A in "$@"; do printf '%s\037' "$A"; done; printf '\n'; } >> "$D/attach.log"
  echo $$ > "$D/pid.$ID"
  env | sort > "$D/env.$ID"
  MODE=cat; if [ -e "$D/mode.$ID" ]; then MODE=$(cat "$D/mode.$ID"); fi
  # A tmux client starts by drawing with an escape sequence; the refusals and tmux's
  # own errors, and the one mode that plays plain text, do not.
  case "$MODE" in unknown|exited|broken|silent|tmux-terminal|tmux-session|plain|mute) ;; *) printf '\033[?25l';; esac
  case "$MODE" in
  cat) stty size; exec cat;;
  lines) stty size; while IFS= read -r L; do case "$L" in size) stty size;; quit) exit 0;; esac; done;;
  winch) trap 'echo WINCH; stty size' WINCH; echo ready; while :; do sleep 0.02; done;;
  raw) stty raw -echo; echo ready; exec cat;;
  idle) echo ready; exec sleep 600;;
  mute) exec sleep 600;;
  exit) printf 'bye\n'; exit 0;;
  flood) head -c 200000 /dev/zero | tr '\0' x; echo; echo END; exec cat;;
  ctty) if (exec 3<>/dev/tty) 2>/dev/null; then echo ctty-yes; else echo ctty-no; fi; exec cat;;
  unknown) echo "riwork: unknown shell $ID" >&2; exit 2;;
  exited) echo "riwork: shell $ID has exited" >&2; exit 2;;
  broken) echo "riwork: tmux: server exited unexpectedly" >&2; exit 2;;
  silent) exit 3;;
  tmux-terminal) echo "missing or unsuitable terminal: xterm-ghostty" >&2; exit 1;;
  tmux-session) echo "can't find session: $ID" >&2; exit 1;;
  plain) echo "plain text, no escape"; exec cat;;
  esac;;
esac
"#
    .replace("@DIR@", &dir.to_string_lossy());
    std::fs::write(&cli, script).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

/// What a process id in `dir/pid.SHELL` is doing: running, or gone (a zombie is gone).
fn running(pid: i32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}
fn pid_of(dir: &Path, shell: &str) -> i32 {
    std::fs::read_to_string(dir.join(format!("pid.{shell}")))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
async fn until(limit: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() > end {
            return false;
        }
        sleep(Duration::from_millis(25)).await;
    }
}
async fn gone(pid: i32, limit: Duration) -> bool {
    until(limit, || !running(pid)).await
}
/// The process id appears once the stand-in has started.
async fn pid_when_started(dir: &Path, shell: &str) -> i32 {
    let path = dir.join(format!("pid.{shell}"));
    assert!(
        until(Duration::from_secs(10), || path.exists()).await,
        "no process for {shell}"
    );
    // Written whole by `echo`, but let the line end.
    sleep(Duration::from_millis(20)).await;
    pid_of(dir, shell)
}

// ---- the RPC layer --------------------------------------------------------

struct Fixture {
    _home: tempfile::TempDir,
    stub: tempfile::TempDir,
    rpc: Rpc,
    device: String,
    set: Arc<PtySet>,
    viewport: tokio::sync::Mutex<Option<Viewport>>,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let stub = tempfile::tempdir().unwrap();
        let storage = Storage::at(home.path().into()).unwrap();
        let device = storage
            .pair_kind(
                "wss://example.com/v1/ws".into(),
                "other mac".into(),
                false,
                &home.path().join("mac.json"),
                None,
                2,
                600,
                DeviceKind::Desktop,
            )
            .unwrap()
            .device_id;
        let cli = stand_in_cli(stub.path());
        Self {
            rpc: Rpc::new(cli, storage),
            viewport: tokio::sync::Mutex::new(Some(Viewport::new("/none".into(), device.clone()))),
            _home: home,
            stub,
            device,
            set: PtySet::new(),
        }
    }
    fn mode(&self, shell: &str, mode: &str) {
        std::fs::write(self.stub.path().join(format!("mode.{shell}")), mode).unwrap();
    }
    fn dir(&self) -> &Path {
        self.stub.path()
    }
    /// A request as the connector would carry it out: the three that never wait
    /// are answered by the connection loop, the others run in a lane.
    async fn call(&self, method: &str, params: Value) -> Value {
        self.call_in(Some(&self.set), method, params).await
    }
    async fn call_in(&self, set: Option<&Arc<PtySet>>, method: &str, params: Value) -> Value {
        let request = request(method, params);
        if let Some(set) = set
            && matches!(method, "pty.write" | "pty.resize" | "pty.close")
        {
            return self
                .rpc
                .handle_pty_inline(&self.device, request, set)
                .unwrap();
        }
        self.rpc
            .handle_session_up_to(&self.device, request, &self.viewport, set, 131_072)
            .await
            .unwrap()
    }
    async fn open_as(&self, shell: &str, mode: &str) -> Value {
        self.mode(shell, mode);
        self.call(
            "pty.open",
            json!({"shell_id":shell,"columns":100,"rows":30,"term":"xterm-ghostty"}),
        )
        .await
    }
    /// Opens a stream onto a new shell acting out `mode`.
    async fn open(&self, mode: &str) -> Stream<'_> {
        let shell = uuid4();
        let opened = self.open_as(&shell, mode).await;
        assert_eq!(opened["ok"], true, "{opened}");
        assert_eq!(opened["result"]["shell_id"], shell);
        Stream {
            fixture: self,
            id: opened["result"]["stream"].as_str().unwrap().to_owned(),
            shell,
            seen: Vec::new(),
        }
    }
}
fn code(response: &Value) -> &str {
    response["error"]["code"].as_str().unwrap_or("")
}
fn message(response: &Value) -> &str {
    response["error"]["message"].as_str().unwrap_or("")
}

struct Stream<'a> {
    fixture: &'a Fixture,
    id: String,
    shell: String,
    /// Everything read so far, whose length is the `seq` of the next read.
    seen: Vec<u8>,
}
impl Stream<'_> {
    async fn call(&self, method: &str, params: Value) -> Value {
        let mut params = params;
        params["stream"] = json!(self.id);
        self.fixture.call(method, params).await
    }
    /// One read; checks that its offset is the number of bytes read before it.
    async fn read(&mut self, wait_ms: u64) -> Value {
        let reply = self.call("pty.read", json!({"wait_ms":wait_ms})).await;
        assert_eq!(reply["ok"], true, "{reply}");
        let result = &reply["result"];
        assert_eq!(result["stream"], self.id);
        assert_eq!(result["seq"], self.seen.len() as u64, "{reply}");
        if let Some(text) = result["data"].as_str() {
            let bytes = URL_SAFE_NO_PAD.decode(text).unwrap();
            assert!(bytes.len() <= pty::MAX_CHUNK);
            self.seen.extend(bytes);
        }
        reply
    }
    /// Reads until what has been read contains `needle`.
    async fn read_until(&mut self, needle: &str) -> String {
        let end = Instant::now() + Duration::from_secs(15);
        while !String::from_utf8_lossy(&self.seen).contains(needle) {
            assert!(
                Instant::now() < end,
                "never saw {needle:?} in {:?}",
                String::from_utf8_lossy(&self.seen)
            );
            let reply = self.read(1000).await;
            assert!(reply["result"]["eof"].is_null(), "ended early: {reply}");
        }
        String::from_utf8_lossy(&self.seen).into_owned()
    }
    /// Reads to the end, and returns its reason.
    async fn read_to_end(&mut self) -> String {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                Instant::now() < end,
                "never ended: {:?}",
                String::from_utf8_lossy(&self.seen)
            );
            let reply = self.read(1000).await;
            if reply["result"]["eof"] == true {
                return reply["result"]["reason"].as_str().unwrap().to_owned();
            }
        }
    }
    /// Writes at the running offset, and keeps count.
    async fn type_at(&self, offset: &mut u64, text: &str) -> Value {
        let reply = self
            .call(
                "pty.write",
                json!({"seq":*offset,"data":data(text),"gap_ms":0}),
            )
            .await;
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["result"]["status"], "written");
        assert_eq!(reply["result"]["seq"], *offset);
        *offset += text.len() as u64;
        reply
    }
    fn pid(&self) -> i32 {
        pid_of(self.fixture.dir(), &self.shell)
    }
    fn seen_text(&self) -> String {
        String::from_utf8_lossy(&self.seen).into_owned()
    }
}

#[tokio::test]
async fn what_is_typed_comes_back_through_the_terminal_in_offsets_that_add_up() {
    let f = Fixture::new();
    let mut stream = f.open("cat").await;
    // The client started on a terminal of the size asked for.
    assert!(
        stream
            .read_until("30 100\r\n")
            .await
            .ends_with("30 100\r\n")
    );
    // Typed in two pieces, as the client's offsets say; the line discipline
    // echoes it and `cat` prints it again.
    let mut offset = 0;
    stream.type_at(&mut offset, "pin").await;
    stream.type_at(&mut offset, "g\n").await;
    assert_eq!(offset, 5);
    let seen = stream.read_until("ping\r\nping\r\n").await;
    assert!(seen.contains("30 100\r\n"), "{seen:?}");
    // Nothing more: an idle read says so, and where it stands.
    sleep(Duration::from_millis(100)).await;
    while stream.read(0).await["result"]["data"] != "" {}
    let idle = stream.read(50).await;
    assert_eq!(idle["result"]["data"], "");
    assert_eq!(idle["result"]["seq"], stream.seen.len() as u64);
    // Its terminal is a real one with the client as its controlling process.
    let tty = f.open("ctty").await;
    let mut tty = tty;
    tty.read_until("ctty-yes").await;
}

#[tokio::test]
async fn a_resize_changes_the_size_the_client_sees_and_signals_it() {
    let f = Fixture::new();
    let mut stream = f.open("lines").await;
    stream.read_until("30 100\r\n").await;
    let mut offset = 0;
    stream.type_at(&mut offset, "size\n").await;
    stream.read_until("size\r\n30 100\r\n").await;
    let before = stream.seen.len();

    let resized = stream
        .call("pty.resize", json!({"columns":120,"rows":40}))
        .await;
    assert_eq!(
        resized["result"],
        json!({"stream":stream.id,"status":"resized"})
    );
    stream.type_at(&mut offset, "size\n").await;
    let seen = stream.read_until("40 120\r\n").await;
    assert!(seen[before..].contains("40 120"), "{seen:?}");

    // The extremes are accepted, one past them is not.
    for (columns, rows, ok) in [
        (1, 1, true),
        (1000, 500, true),
        (1001, 24, false),
        (80, 501, false),
        (0, 24, false),
    ] {
        let reply = stream
            .call("pty.resize", json!({"columns":columns,"rows":rows}))
            .await;
        assert_eq!(reply["ok"], ok, "{columns}x{rows}: {reply}");
    }
    // And the client ending ends the stream, with everything it said read first.
    stream.type_at(&mut offset, "quit\n").await;
    assert_eq!(stream.read_to_end().await, "exited");
    let end = stream.read(5000).await;
    assert_eq!(end["result"]["eof"], true, "the end is not used up: {end}");
    // Writing to or resizing an ended stream is refused; closing it is not.
    let late = stream
        .call(
            "pty.write",
            json!({"seq":offset,"data":data("x"),"gap_ms":0}),
        )
        .await;
    assert_eq!(code(&late), "not_found", "{late}");
    let late = stream
        .call("pty.resize", json!({"columns":80,"rows":24}))
        .await;
    assert_eq!(code(&late), "not_found", "{late}");
    assert_eq!(stream.call("pty.close", json!({})).await["ok"], true);
    assert_eq!(
        code(&stream.call("pty.close", json!({})).await),
        "not_found"
    );
    assert_eq!(
        code(&stream.call("pty.read", json!({"wait_ms":0})).await),
        "not_found"
    );
}

#[tokio::test]
async fn a_window_change_reaches_the_client_as_a_signal() {
    let f = Fixture::new();
    let mut stream = f.open("winch").await;
    stream.read_until("ready").await;
    stream
        .call("pty.resize", json!({"columns":132,"rows":43}))
        .await;
    let seen = stream.read_until("43 132").await;
    assert!(seen.contains("WINCH"), "{seen:?}");
}

#[tokio::test]
async fn a_client_that_exits_leaves_its_last_words_then_ends_the_stream() {
    let f = Fixture::new();
    let mut stream = f.open("exit").await;
    assert_eq!(stream.read_to_end().await, "exited");
    assert!(
        stream.seen_text().ends_with("bye\r\n"),
        "{:?}",
        stream.seen_text()
    );
    // The reason is stable, and the stream stays until closed.
    assert_eq!(stream.read(0).await["result"]["reason"], "exited");
    assert_eq!(f.set.len(), 1);
    stream.call("pty.close", json!({})).await;
    assert_eq!(f.set.len(), 0);
}

#[tokio::test]
async fn output_is_chunked_at_64_kib_and_two_parked_reads_split_it_without_overlap() {
    let f = Fixture::new();
    let mut stream = f.open("flood").await;
    // `read` checks every chunk against MAX_CHUNK and the offset before it.
    let mut chunks = 0;
    let end = Instant::now() + Duration::from_secs(20);
    while !String::from_utf8_lossy(&stream.seen).contains("END") {
        assert!(Instant::now() < end, "never saw END");
        let before = stream.seen.len();
        stream.read(1000).await;
        chunks += usize::from(stream.seen.len() > before);
    }
    assert!(stream.seen.len() >= 200_000);
    assert!(
        chunks >= 4,
        "{chunks} chunks of at most 64 KiB for {} bytes",
        stream.seen.len()
    );

    // Two reads parked together get different bytes, each at its own offset.
    let calm = f.open("cat").await;
    let base = {
        let mut calm = calm;
        calm.read_until("30 100\r\n").await;
        let base = calm.seen.len() as u64;
        let (a, b) = (
            calm.call("pty.read", json!({"wait_ms":3000})),
            calm.call("pty.read", json!({"wait_ms":3000})),
        );
        let typing = async {
            sleep(Duration::from_millis(100)).await;
            let mut offset = 0;
            calm.type_at(&mut offset, "ab\n").await;
            sleep(Duration::from_millis(300)).await;
            calm.type_at(&mut offset, "cd\n").await;
        };
        let (a, b, ()) = tokio::join!(a, b, typing);
        let mut got: Vec<(u64, Vec<u8>)> = [a, b]
            .iter()
            .map(|r| {
                assert_eq!(r["ok"], true, "{r}");
                (
                    r["result"]["seq"].as_u64().unwrap(),
                    URL_SAFE_NO_PAD
                        .decode(r["result"]["data"].as_str().unwrap())
                        .unwrap(),
                )
            })
            .collect();
        got.sort();
        assert!(got.iter().all(|(_, bytes)| !bytes.is_empty()), "{got:?}");
        // Back to back from where the stream stood: no byte twice, none lost.
        assert_eq!(got[0].0, base);
        assert_eq!(got[1].0, base + got[0].1.len() as u64);
        base
    };
    assert!(base > 0);
}

#[tokio::test]
async fn closing_kills_the_client_and_wakes_a_parked_read_with_closed() {
    let f = Fixture::new();
    let mut stream = f.open("idle").await;
    stream.read_until("ready").await;
    let pid = stream.pid();
    assert!(running(pid));
    let parked = {
        let (id, set) = (stream.id.clone(), f.set.clone());
        let rpc = &f.rpc;
        let (device, viewport) = (&f.device, &f.viewport);
        let read = rpc.handle_session_up_to(
            device,
            request("pty.read", json!({"stream":id,"wait_ms":25000})),
            viewport,
            Some(&set),
            131_072,
        );
        let closing = async {
            sleep(Duration::from_millis(150)).await;
            let started = Instant::now();
            let closed = stream.call("pty.close", json!({})).await;
            (closed, started)
        };
        let (reply, (closed, started)) = tokio::join!(read, closing);
        assert_eq!(closed["result"]["status"], "closed", "{closed}");
        let reply = reply.unwrap();
        assert_eq!(
            reply["result"]["eof"], true,
            "a read parked for 25 s returns when the stream is closed: {reply}"
        );
        assert_eq!(reply["result"]["reason"], "closed");
        assert!(started.elapsed() < Duration::from_secs(2));
        reply
    };
    assert_eq!(parked["result"]["stream"], stream.id);
    // The process is killed (and reaped) at once, and the stream is forgotten.
    assert!(
        gone(pid, Duration::from_secs(2)).await,
        "pid {pid} still running"
    );
    assert_eq!(f.set.len(), 0);
    assert_eq!(
        code(&stream.call("pty.close", json!({})).await),
        "not_found"
    );
}

#[tokio::test]
async fn the_end_of_a_session_kills_every_client_and_refuses_new_ones() {
    let f = Fixture::new();
    let mut streams = Vec::new();
    for _ in 0..3 {
        let mut stream = f.open("idle").await;
        stream.read_until("ready").await;
        streams.push(stream);
    }
    let pids: Vec<i32> = streams.iter().map(Stream::pid).collect();
    assert!(pids.iter().all(|pid| running(*pid)));
    let (id, set) = (streams[0].id.clone(), f.set.clone());
    let parked = tokio::spawn({
        let set = set.clone();
        async move { pty::read(&set, &json!({"stream":id,"wait_ms":25000})).await }
    });
    sleep(Duration::from_millis(100)).await;
    f.set.close_all();
    let reply = timeout(Duration::from_secs(2), parked)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reply["reason"], "closed");
    for pid in pids {
        assert!(
            gone(pid, Duration::from_secs(2)).await,
            "pid {pid} still running"
        );
    }
    assert_eq!(f.set.len(), 0);
    let late = f
        .call(
            "pty.open",
            json!({"shell_id":uuid4(),"columns":80,"rows":24,"term":"xterm-256color"}),
        )
        .await;
    assert_eq!(code(&late), "not_found", "{late}");
    assert!(message(&late).contains("session has ended"));
}

#[tokio::test]
async fn an_open_that_is_cut_short_leaves_no_client_behind() {
    let f = Fixture::new();
    let shell = uuid4();
    f.mode(&shell, "mute");
    // The client never draws, so the open waits; its session ends meanwhile.
    let open = {
        let params = json!({"shell_id":shell,"columns":80,"rows":24,"term":"xterm-256color"});
        let set = f.set.clone();
        let cli = f.rpc.cli.clone();
        tokio::spawn(async move { pty::open(&set, &cli, pty::open_spec(&params).unwrap()).await })
    };
    let pid = pid_when_started(f.dir(), &shell).await;
    assert!(running(pid));
    open.abort();
    assert!(open.await.unwrap_err().is_cancelled());
    assert!(
        gone(pid, Duration::from_secs(2)).await,
        "pid {pid} still running"
    );
    // Nor does it keep its place among the streams.
    assert_eq!(f.set.len(), 0);
    let mut streams = Vec::new();
    for _ in 0..pty::MAX_STREAMS {
        streams.push(f.open("idle").await);
    }
}

#[tokio::test]
async fn an_attach_that_cannot_start_is_told_in_the_codes_of_the_other_methods() {
    let f = Fixture::new();
    let params =
        |shell: &str| json!({"shell_id":shell,"columns":80,"rows":24,"term":"xterm-256color"});
    let cases = [
        ("unknown", "not_found", "existing shell ID not found"),
        ("exited", "not_found", "selected shell is not alive"),
        ("broken", "cli_error", "tmux: server exited unexpectedly"),
        (
            "silent",
            "cli_error",
            "the terminal ended before it drew anything",
        ),
        // tmux's own words, printed by the client it became.
        (
            "tmux-terminal",
            "cli_error",
            "missing or unsuitable terminal: xterm-ghostty",
        ),
        ("tmux-session", "not_found", "selected shell is not alive"),
    ];
    for (mode, expected_code, expected_message) in cases {
        let shell = uuid4();
        f.mode(&shell, mode);
        let started = Instant::now();
        let reply = f.call("pty.open", params(&shell)).await;
        assert_eq!(code(&reply), expected_code, "{mode}: {reply}");
        assert_eq!(message(&reply), expected_message, "{mode}");
        assert!(started.elapsed() < Duration::from_secs(10), "{mode}");
        // No stream, no place held, no process left.
        assert_eq!(f.set.len(), 0);
        let pid = pid_of(f.dir(), &shell);
        assert!(
            gone(pid, Duration::from_secs(2)).await,
            "{mode}: pid {pid} still running"
        );
    }
    // A shell id that is not a canonical UUID never reaches the CLI.
    let before = std::fs::read_to_string(f.dir().join("attach.log"))
        .unwrap()
        .lines()
        .count();
    let reply = f.call("pty.open", params("not-a-uuid")).await;
    assert_eq!(code(&reply), "invalid_request");
    let after = std::fs::read_to_string(f.dir().join("attach.log"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(before, after);

    // A CLI from before `shell attach --exec` is told apart before anything starts.
    let fresh = Fixture::new();
    std::fs::write(fresh.dir().join("old-cli"), "").unwrap();
    let reply = fresh.call("pty.open", params(&uuid4())).await;
    assert_eq!(code(&reply), "cli_error", "{reply}");
    assert!(message(&reply).contains("update RiWork"), "{reply}");
    assert!(
        !fresh.dir().join("attach.log").exists(),
        "nothing was started"
    );
}

#[tokio::test]
async fn a_client_that_starts_with_plain_text_and_keeps_running_is_a_screen_after_a_moment() {
    let f = Fixture::new();
    let started = Instant::now();
    let mut stream = f.open("plain").await;
    // It is given a moment to turn out to be an error message, and does not.
    assert!(started.elapsed() >= Duration::from_millis(300));
    stream.read_until("plain text, no escape\r\n").await;
    stream.call("pty.close", json!({})).await;
}

#[tokio::test]
async fn at_most_eight_streams_are_open_at_once_and_a_closed_one_makes_room() {
    let f = Fixture::new();
    let mut streams = Vec::new();
    for _ in 0..pty::MAX_STREAMS {
        streams.push(f.open("idle").await);
    }
    let shell = uuid4();
    let refused = f.open_as(&shell, "idle").await;
    assert_eq!(code(&refused), "pty_limit", "{refused}");
    assert!(message(&refused).contains('8'), "{refused}");
    assert!(
        !f.dir().join(format!("pid.{shell}")).exists(),
        "a refused open starts nothing"
    );
    // Only the capabilities question was asked, once: a yes is remembered.
    let asked = std::fs::read_to_string(f.dir().join("calls.log")).unwrap();
    assert_eq!(asked.lines().count(), 1, "{asked}");

    let freed = streams.remove(3);
    freed.call("pty.close", json!({})).await;
    let again = f.open_as(&uuid4(), "idle").await;
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(f.set.len(), pty::MAX_STREAMS);
}

#[tokio::test]
#[ignore = "slow: real pty client; wall-clock bounds on the 150 ms Return delay"]
async fn a_return_after_text_waits_the_gap_the_client_asks_up_to_150_ms() {
    let f = Fixture::new();
    let mut stream = f.open("raw").await;
    stream.read_until("ready").await;
    let mut offset = 0;
    stream.type_at(&mut offset, "x").await;
    stream.read_until("x").await;

    // A Return with a gap of a whole second is held back by 150 ms, not 1000.
    let before = stream.seen.len();
    let sent = Instant::now();
    let reply = stream
        .call(
            "pty.write",
            json!({"seq":offset,"data":data("\r"),"gap_ms":1000}),
        )
        .await;
    assert_eq!(reply["ok"], true, "{reply}");
    offset += 1;
    assert!(
        sent.elapsed() < Duration::from_millis(100),
        "the answer does not wait for the gap"
    );
    stream.read_until("x\r").await;
    let waited = sent.elapsed();
    assert!(waited >= Duration::from_millis(140), "{waited:?}");
    assert!(waited < Duration::from_millis(900), "{waited:?}");
    assert!(stream.seen.len() > before);

    // Only a chunk that *starts* with a Return waits.
    let sent = Instant::now();
    stream
        .call(
            "pty.write",
            json!({"seq":offset,"data":data("a\r"),"gap_ms":1000}),
        )
        .await;
    stream.read_until("a\r").await;
    assert!(
        sent.elapsed() < Duration::from_millis(900),
        "{:?}",
        sent.elapsed()
    );
}

#[tokio::test]
async fn the_client_gets_the_argv_and_the_environment_it_is_allowed() {
    let f = Fixture::new();
    let shell = uuid4();
    f.mode(&shell, "cat");
    let opened = f
        .call(
            "pty.open",
            json!({"shell_id":shell,"columns":90,"rows":25,"term":"xterm-ghostty","ignore_size":true}),
        )
        .await;
    assert_eq!(opened["ok"], true, "{opened}");
    let logged = std::fs::read_to_string(f.dir().join("attach.log")).unwrap();
    assert_eq!(
        logged,
        format!("shell\u{1f}attach\u{1f}{shell}\u{1f}--exec\u{1f}--ignore-size\u{1f}\n")
    );
    let environment = std::fs::read_to_string(f.dir().join(format!("env.{shell}"))).unwrap();
    let names: Vec<&str> = environment
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .collect();
    for wanted in ["PATH", "TERM", "COLORTERM", "LANG"] {
        assert!(names.contains(&wanted), "{wanted} missing from {names:?}");
    }
    assert!(
        environment.contains("TERM=xterm-ghostty\n"),
        "{environment}"
    );
    assert!(
        environment.contains("COLORTERM=truecolor\n"),
        "{environment}"
    );
    // Nothing of the test run's own environment follows into the client.
    for secret in [
        "CARGO_PKG_NAME",
        "CARGO_MANIFEST_DIR",
        "TMUX",
        "TMUX_TMPDIR",
    ] {
        assert!(!names.contains(&secret), "{secret} in {names:?}");
    }
    // The size it starts with is the one asked for (the client prints it first).
    let mut stream = Stream {
        fixture: &f,
        id: opened["result"]["stream"].as_str().unwrap().to_owned(),
        shell,
        seen: Vec::new(),
    };
    stream.read_until("25 90\r\n").await;
}

// ---- the command line ------------------------------------------------------

fn remote(home: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_riwork-remote"))
        .env("RIWORK_HOME", home)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn pairing_a_desktop_needs_protocol_2_and_devices_show_the_kind() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let pair = |name: &str, extra: &[&str]| {
        let out = dir.path().join(format!("{name}.json"));
        let mut args = vec![
            "pair",
            "--relay",
            "wss://relay.example.com/v1/ws",
            "--name",
            name,
            "--out",
            out.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        remote(&home, &args)
    };
    let refused = pair("v1-mac", &["--kind", "desktop"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("--kind desktop requires --protocol 2"),
        "{stderr}"
    );
    assert!(!dir.path().join("v1-mac.json").exists());
    let refused = pair("v1-mac", &["--kind", "desktop", "--protocol", "1"]);
    assert!(!refused.status.success());
    let unknown = pair("tablet", &["--kind", "tablet", "--protocol", "2"]);
    assert!(!unknown.status.success());

    assert!(pair("My iPhone", &[]).status.success());
    assert!(
        pair("phone-v2", &["--protocol", "2", "--kind", "mobile"])
            .status
            .success()
    );
    assert!(
        pair("Studio Mac", &["--protocol", "2", "--kind", "desktop"])
            .status
            .success()
    );

    let listed = remote(&home, &["devices"]);
    assert!(listed.status.success());
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    let kinds: Vec<(&str, &str, u64)> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["device_name"].as_str().unwrap(),
                d["kind"].as_str().unwrap(),
                d["protocol"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("My iPhone", "mobile", 1),
            ("phone-v2", "mobile", 2),
            ("Studio Mac", "desktop", 2)
        ]
    );
    // Only the desktop's record carries a kind; the others are written as ever.
    let saved = std::fs::read_to_string(home.join("remote/devices.json")).unwrap();
    assert_eq!(saved.matches("\"kind\"").count(), 1, "{saved}");
}

// ---- end to end ------------------------------------------------------------

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A relay, the real connector binary with the stand-in CLI, and one paired device.
struct Rig {
    dir: tempfile::TempDir,
    home: PathBuf,
    storage: Storage,
    pairing: Pairing,
    invite_secret: [u8; 32],
    connector: Child,
    relay: tokio::task::JoinHandle<()>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.connector.start_kill();
        self.relay.abort();
    }
}
impl Rig {
    async fn new(kind: DeviceKind) -> Self {
        Self::with_cli(kind, None).await
    }
    /// `cli`: a real RiWork CLI instead of the stand-in.
    async fn with_cli(kind: DeviceKind, cli: Option<PathBuf>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
        let storage = Storage::at(home.clone()).unwrap();
        let routes_path = dir.path().join("routes.json");
        let pairing = storage
            .pair_kind(
                url,
                "other mac".into(),
                true,
                &dir.path().join("pair.json"),
                Some(&routes_path),
                2,
                600,
                kind,
            )
            .unwrap();
        let invite_secret = decode::<32>(pairing.invite_secret.as_deref().unwrap()).unwrap();
        let routes: Routes = private_read(&routes_path, 1_048_576).unwrap();
        let relay = Relay::new(routes, 16).unwrap();
        let relay = tokio::spawn(async move {
            axum::serve(listener, relay.router()).await.unwrap();
        });
        let cli = cli.unwrap_or_else(|| stand_in_cli(dir.path()));
        let connector = Command::new(env!("CARGO_BIN_EXE_riwork-remote"))
            .env("RIWORK_HOME", &home)
            .args(["start", "--riwork"])
            .arg(&cli)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            dir,
            home,
            storage,
            pairing,
            invite_secret,
            connector,
            relay,
        }
    }
    fn mode(&self, shell: &str, mode: &str) {
        std::fs::write(self.dir.path().join(format!("mode.{shell}")), mode).unwrap();
    }
    async fn connect(&self) -> Socket {
        let p = &self.pairing;
        let (mut ws, online) = loop {
            match connect_registered(&p.relay_url, &p.route_id, "mobile", &p.relay_token).await {
                Ok(ready) => break ready,
                Err(_) => sleep(Duration::from_millis(20)).await,
            }
        };
        if !online {
            let peer = receive_json(&mut ws).await.unwrap();
            assert_eq!(peer["online"], true);
        }
        ws
    }
    /// The first session: redeems the invite, then the session handshake.
    async fn first_session(&self) -> (Client, [u8; 32]) {
        let p = &self.pairing;
        let mut ws = self.connect().await;
        let nonce = random32();
        let hello = pair_hello(
            &p.identity(),
            &p.relay_url,
            p.invite_id.as_deref().unwrap(),
            p.expires_at.unwrap(),
            &self.invite_secret,
            nonce,
        )
        .unwrap();
        send_json(&mut ws, &hello).await.unwrap();
        let accept: PairAccept =
            serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        let (finish, root, _) = accept_pair(
            &p.identity(),
            &p.relay_url,
            p.invite_id.as_deref().unwrap(),
            p.expires_at.unwrap(),
            &self.invite_secret,
            &nonce,
            &accept,
        )
        .unwrap();
        send_json(&mut ws, &finish).await.unwrap();
        (self.handshake(ws, &root).await, root)
    }
    async fn resume(&self, root: &[u8; 32]) -> Client {
        let ws = self.connect().await;
        self.handshake(ws, root).await
    }
    async fn handshake(&self, mut ws: Socket, root: &[u8; 32]) -> Client {
        let p = &self.pairing;
        let private = random32();
        let (hello, _) = client_hello_v2(&p.identity(), root, private).unwrap();
        send_json(&mut ws, &hello).await.unwrap();
        let server: ServerHelloV2 =
            serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        let (finish, mut session) =
            accept_server_hello_v2(&p.identity(), root, private, &server).unwrap();
        send_json(&mut ws, &finish).await.unwrap();
        let ready: Envelope = serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        let ready: Value = serde_json::from_slice(&session.open("d2c", &ready).unwrap()).unwrap();
        assert_eq!(ready["type"], "ready");
        Client {
            ws,
            session,
            ready,
            early: Vec::new(),
        }
    }
}

struct Client {
    ws: Socket,
    session: Session,
    ready: Value,
    /// Answers that arrived while another was awaited.
    early: Vec<Value>,
}
impl Client {
    async fn send(&mut self, method: &str, params: Value) -> String {
        let request = request(method, params);
        let id = request["id"].as_str().unwrap().to_owned();
        let sealed = self
            .session
            .seal("c2d", &serde_json::to_vec(&request).unwrap())
            .unwrap();
        send_json(&mut self.ws, &sealed).await.unwrap();
        id
    }
    /// The answer to request `id`, whatever else arrives first.
    async fn answer(&mut self, id: &str) -> Value {
        if let Some(at) = self.early.iter().position(|a| a["id"] == id) {
            return self.early.remove(at);
        }
        loop {
            let frame = timeout(Duration::from_secs(20), receive_json(&mut self.ws))
                .await
                .expect("an answer")
                .unwrap();
            let envelope: Envelope = serde_json::from_value(frame).unwrap();
            let answer: Value =
                serde_json::from_slice(&self.session.open("d2c", &envelope).unwrap()).unwrap();
            if answer["id"] == id {
                return answer;
            }
            self.early.push(answer);
        }
    }
    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        self.answer(&id).await
    }
}

#[tokio::test]
async fn a_desktop_is_told_streams_exist_opens_one_and_drives_it_over_the_encrypted_link() {
    let rig = Rig::new(DeviceKind::Desktop).await;
    let (mut client, _) = rig.first_session().await;
    assert_eq!(
        client.ready["features"]["pty"],
        json!({"max_streams":8,"max_reads":12,"max_write":32768,"max_chunk":65536})
    );
    // The earlier extensions are still announced beside it.
    assert!(client.ready["features"]["deflate"].is_object());

    let shell = uuid4();
    rig.mode(&shell, "lines");
    let opened = client
        .call(
            "pty.open",
            json!({"shell_id":shell,"columns":100,"rows":30,"term":"xterm-256color"}),
        )
        .await;
    assert_eq!(opened["ok"], true, "{opened}");
    assert!(opened["server_ms"].is_u64());
    let stream = opened["result"]["stream"].as_str().unwrap().to_owned();
    // The client uses the connector's RiWork state, not the default one.
    let environment = std::fs::read_to_string(rig.dir.path().join(format!("env.{shell}"))).unwrap();
    assert!(
        environment.contains(&format!("RIWORK_HOME={}\n", rig.home.display())),
        "{environment}"
    );

    // Park a read, and send three requests at once: they are answered in the
    // order they were sent, by the connection loop, while the read is still parked.
    let parked = client
        .send("pty.read", json!({"stream":stream,"wait_ms":25000}))
        .await;
    let first = client.answer(&parked).await;
    assert_eq!(first["result"]["seq"], 0);
    let text = String::from_utf8(
        URL_SAFE_NO_PAD
            .decode(first["result"]["data"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert!(text.contains("30 100"), "{text:?}");
    let mut seen = text.len() as u64;

    let parked = client
        .send("pty.read", json!({"stream":stream,"wait_ms":25000}))
        .await;
    let write = client
        .send(
            "pty.write",
            json!({"stream":stream,"seq":0,"data":data("size\n"),"gap_ms":0}),
        )
        .await;
    let resize = client
        .send(
            "pty.resize",
            json!({"stream":stream,"columns":120,"rows":40}),
        )
        .await;
    let write_again = client
        .send(
            "pty.write",
            json!({"stream":stream,"seq":5,"data":data("size\n"),"gap_ms":0}),
        )
        .await;
    let bad = client
        .send(
            "pty.write",
            json!({"stream":stream,"seq":99,"data":data("size\n"),"gap_ms":0}),
        )
        .await;
    // In arrival order: the frames of the replies, not only their contents.
    let mut order = Vec::new();
    while order.len() < 4 {
        let frame = receive_json(&mut client.ws).await.unwrap();
        let envelope: Envelope = serde_json::from_value(frame).unwrap();
        let answer: Value =
            serde_json::from_slice(&client.session.open("d2c", &envelope).unwrap()).unwrap();
        order.push(answer["id"].as_str().unwrap().to_owned());
        client.early.push(answer);
        // The parked read may be answered meanwhile; it is not one of the four.
        if order.last() == Some(&parked) {
            order.pop();
        }
    }
    assert_eq!(
        order,
        [
            write.clone(),
            resize.clone(),
            write_again.clone(),
            bad.clone()
        ]
    );
    assert_eq!(client.answer(&write).await["result"]["status"], "written");
    assert_eq!(client.answer(&resize).await["result"]["status"], "resized");
    assert_eq!(client.answer(&write_again).await["result"]["seq"], 5);
    let rejected = client.answer(&bad).await;
    assert_eq!(rejected["error"]["code"], "invalid_request", "{rejected}");

    // The output of the two `size` commands: before the resize and after it.
    let mut output = String::new();
    let mut parked_id = Some(parked);
    while !output.contains("40 120") {
        let id = match parked_id.take() {
            Some(id) => id,
            None => {
                client
                    .send("pty.read", json!({"stream":stream,"wait_ms":25000}))
                    .await
            }
        };
        let answer = client.answer(&id).await;
        let result = &answer["result"];
        assert_eq!(result["seq"], seen, "{answer}");
        let bytes = URL_SAFE_NO_PAD
            .decode(result["data"].as_str().unwrap_or(""))
            .unwrap();
        seen += bytes.len() as u64;
        output.push_str(&String::from_utf8_lossy(&bytes));
    }
    // (Whether the first `size` was answered before or after the resize depends on
    // how fast the stand-in is; the second one is certainly after it.)
    assert_eq!(output.matches("size\r\n").count(), 2, "{output:?}");

    let closed = client.call("pty.close", json!({"stream":stream})).await;
    assert_eq!(closed["result"]["status"], "closed");
    let pid = pid_of(rig.dir.path(), &shell);
    assert!(gone(pid, Duration::from_secs(2)).await);
    let late = client
        .call("pty.read", json!({"stream":stream,"wait_ms":0}))
        .await;
    assert_eq!(late["error"]["code"], "not_found");
}

#[tokio::test]
async fn a_phone_is_not_told_about_streams_and_cannot_open_one() {
    let rig = Rig::new(DeviceKind::Mobile).await;
    let (mut client, _) = rig.first_session().await;
    assert!(
        client.ready["features"].get("pty").is_none(),
        "{}",
        client.ready
    );
    assert!(client.ready["features"]["deflate"].is_object());
    let shell = uuid4();
    let stream = uuid4();
    for (method, params) in [
        (
            "pty.open",
            json!({"shell_id":shell,"columns":80,"rows":24,"term":"xterm-256color"}),
        ),
        ("pty.read", json!({"stream":stream,"wait_ms":0})),
        (
            "pty.write",
            json!({"stream":stream,"seq":0,"data":data("x"),"gap_ms":0}),
        ),
        (
            "pty.resize",
            json!({"stream":stream,"columns":80,"rows":24}),
        ),
        ("pty.close", json!({"stream":stream})),
    ] {
        let answer = client.call(method, params).await;
        assert_eq!(answer["ok"], false, "{method}: {answer}");
        assert_eq!(answer["error"]["code"], "invalid_request", "{method}");
        assert_eq!(
            answer["error"]["message"], "unsupported RPC method",
            "{method}"
        );
    }
    // The session is fine, and nothing was started.
    let configured = client.call("link.configure", json!({})).await;
    assert_eq!(configured["ok"], true, "{configured}");
    assert!(!rig.dir.path().join("attach.log").exists());
}

/// Revokes the device while it holds a stream; `parked`: with a read waiting on it.
async fn revoked_while_holding_a_stream(parked: bool) {
    let rig = Rig::new(DeviceKind::Desktop).await;
    let (mut client, _) = rig.first_session().await;
    let shell = uuid4();
    rig.mode(&shell, "idle");
    let opened = client
        .call(
            "pty.open",
            json!({"shell_id":shell,"columns":80,"rows":24,"term":"xterm-256color"}),
        )
        .await;
    let stream = opened["result"]["stream"].as_str().unwrap().to_owned();
    let ready = client
        .call("pty.read", json!({"stream":stream,"wait_ms":5000}))
        .await;
    assert_eq!(ready["ok"], true, "{ready}");
    if parked {
        // As the other Mac's daemon keeps one.
        client
            .send("pty.read", json!({"stream":stream,"wait_ms":25000}))
            .await;
    }
    let pid = pid_of(rig.dir.path(), &shell);
    assert!(running(pid));

    let revoked = Instant::now();
    rig.storage.revoke(&rig.pairing.device_id).unwrap();
    assert!(
        gone(pid, Duration::from_secs(5)).await,
        "the client is still running"
    );
    let took = revoked.elapsed();
    assert!(took < Duration::from_millis(1500), "{took:?}");
}

#[tokio::test]
#[ignore = "slow: real connector and client processes; asserts revocation ends them within 1.5 s"]
async fn revoking_the_device_ends_its_streams_within_a_second() {
    revoked_while_holding_a_stream(true).await;
}

#[tokio::test]
async fn a_new_session_starts_without_the_streams_of_the_old_one() {
    let rig = Rig::new(DeviceKind::Desktop).await;
    let (mut client, root) = rig.first_session().await;
    let shell = uuid4();
    rig.mode(&shell, "idle");
    let opened = client
        .call(
            "pty.open",
            json!({"shell_id":shell,"columns":80,"rows":24,"term":"xterm-256color"}),
        )
        .await;
    let stream = opened["result"]["stream"].as_str().unwrap().to_owned();
    let pid = pid_of(rig.dir.path(), &shell);
    assert!(running(pid));
    // The other Mac goes away: the relay tells the connector its peer is gone.
    let _ = client.ws.close(None).await;
    drop(client);
    assert!(
        gone(pid, Duration::from_secs(5)).await,
        "the client outlived the session that opened it"
    );
    let mut again = rig.resume(&root).await;
    assert!(again.ready["features"]["pty"].is_object());
    let answer = again
        .call("pty.read", json!({"stream":stream,"wait_ms":0}))
        .await;
    assert_eq!(answer["error"]["code"], "not_found", "{answer}");
}

#[tokio::test]
async fn when_the_connector_dies_the_terminals_hang_up_and_their_clients_end() {
    let mut rig = Rig::new(DeviceKind::Desktop).await;
    let (mut client, _) = rig.first_session().await;
    let shell = uuid4();
    rig.mode(&shell, "idle");
    let opened = client
        .call(
            "pty.open",
            json!({"shell_id":shell,"columns":80,"rows":24,"term":"xterm-256color"}),
        )
        .await;
    assert_eq!(opened["ok"], true, "{opened}");
    let pid = pid_of(rig.dir.path(), &shell);
    assert!(running(pid));
    // No chance to clean up: the master closes with the process.
    rig.connector.start_kill().unwrap();
    assert!(
        gone(pid, Duration::from_secs(5)).await,
        "the client is still running after its connector was killed"
    );
}

/// The real CLI and a real tmux in a throwaway `RIWORK_HOME`: the stand-in cannot
/// show that the connector and the CLI agree on the state, the arguments and the terminal.
/// `RIWORK_TEST_CLI=/absolute/path/to/riwork cargo test --test pty -- --ignored`
#[tokio::test]
#[ignore = "requires RIWORK_TEST_CLI (an absolute RiWork CLI) and tmux; creates only isolated temporary data"]
async fn the_real_cli_streams_a_real_tmux_shell_and_leaves_it_running() {
    let cli = PathBuf::from(std::env::var_os("RIWORK_TEST_CLI").expect("set RIWORK_TEST_CLI"));
    assert!(cli.is_absolute() && cli.is_file());
    let rig = Rig::with_cli(DeviceKind::Desktop, Some(cli.clone())).await;
    let riwork = |args: &[&str]| {
        let output = std::process::Command::new(&cli)
            .env("RIWORK_HOME", &rig.home)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap_or(Value::Null)
    };
    let project_dir = rig.dir.path().join("app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&project_dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(status.status.success());
    };
    git(&["init", "--initial-branch=main", "--template="]);
    git(&["commit", "--allow-empty", "-m", "fixture"]);
    let project = riwork(&["project", "add", project_dir.to_str().unwrap(), "--json"]);
    let project = project["id"].as_str().unwrap().to_owned();
    let shell = riwork(&[
        "shell",
        "create",
        "--project",
        &project,
        "--command",
        "exec sh",
        "--json",
    ]);
    let shell = shell["id"].as_str().unwrap().to_owned();

    let (mut client, _) = rig.first_session().await;
    assert!(client.ready["features"]["pty"].is_object());
    let opened = client
        .call(
            "pty.open",
            json!({"shell_id":shell,"columns":100,"rows":30,"term":"xterm-ghostty"}),
        )
        .await;
    assert_eq!(opened["ok"], true, "{opened}");
    let stream = opened["result"]["stream"].as_str().unwrap().to_owned();

    // Type into it as the other Mac would; read what tmux draws until it shows the answers.
    let mut seen = Vec::<u8>::new();
    let mut written = 0u64;
    let mut typed = |text: &str| {
        let seq = written;
        written += text.len() as u64;
        (seq, data(text))
    };
    let drawn = |client_seen: &mut Vec<u8>, answer: &Value| {
        let result = &answer["result"];
        assert_eq!(result["seq"], client_seen.len() as u64, "{answer}");
        client_seen.extend(
            URL_SAFE_NO_PAD
                .decode(result["data"].as_str().unwrap_or(""))
                .unwrap(),
        );
    };
    // `RIWORK_HOME` is whatever the real CLI gave the session; the arithmetic proves a live shell.
    let (seq, text) = typed("echo MARK$((20+22)) $RIWORK_HOME\r");
    let wrote = client
        .call(
            "pty.write",
            json!({"stream":stream,"seq":seq,"data":text,"gap_ms":0}),
        )
        .await;
    assert_eq!(wrote["ok"], true, "{wrote}");
    let end = Instant::now() + Duration::from_secs(20);
    while !String::from_utf8_lossy(&seen).contains("MARK42") {
        assert!(
            Instant::now() < end,
            "never saw the answer: {:?}",
            String::from_utf8_lossy(&seen)
        );
        let answer = client
            .call("pty.read", json!({"stream":stream,"wait_ms":1000}))
            .await;
        assert_eq!(answer["ok"], true, "{answer}");
        assert!(answer["result"]["eof"].is_null(), "{answer}");
        drawn(&mut seen, &answer);
    }

    // A resize reaches the shell: tmux sizes the window to its only client.
    let resized = client
        .call(
            "pty.resize",
            json!({"stream":stream,"columns":120,"rows":40}),
        )
        .await;
    assert_eq!(resized["ok"], true, "{resized}");
    let (seq, text) = typed("stty size\r");
    client
        .call(
            "pty.write",
            json!({"stream":stream,"seq":seq,"data":text,"gap_ms":0}),
        )
        .await;
    let end = Instant::now() + Duration::from_secs(20);
    while !String::from_utf8_lossy(&seen).contains("40 120") {
        assert!(
            Instant::now() < end,
            "no resize: {:?}",
            String::from_utf8_lossy(&seen)
        );
        let answer = client
            .call("pty.read", json!({"stream":stream,"wait_ms":1000}))
            .await;
        drawn(&mut seen, &answer);
    }

    // Closing hangs the client up and nothing else.
    let closed = client.call("pty.close", json!({"stream":stream})).await;
    assert_eq!(closed["result"]["status"], "closed");
    sleep(Duration::from_millis(500)).await;
    let listed = riwork(&["shell", "list", "--all", "--json"]);
    let alive = listed
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == shell.as_str() && s["alive"] == true);
    assert!(alive, "the shell must outlive its stream: {listed}");
    let unknown = client
        .call(
            "pty.open",
            json!({"shell_id":uuid4(),"columns":80,"rows":24,"term":"xterm-256color"}),
        )
        .await;
    assert_eq!(unknown["error"]["code"], "not_found", "{unknown}");
    riwork(&["shell", "close", &shell]);
}
