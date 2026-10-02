//! `riwork-remote attach --desktop ID --shell UUID`: the child process of a Ghostty
//! surface that shows a shell of another Mac.
//!
//! Ghostty draws whatever its child writes, so this process is a pipe with a few
//! manners: it puts its terminal in raw mode (every key goes to the host as a byte,
//! Ctrl-C included), asks the client daemon for a PTY stream of the shell
//! (`client ensure` first), copies keys to the daemon and the daemon's output to the
//! terminal, and passes window size changes on. The output is what a local
//! `tmux attach` would write, so scrollback, mouse, the alternate screen and the
//! repaint after a reconnect all come from tmux and Ghostty, not from here.
//!
//! When the link drops the daemon says so (`S` offline). The screen then stays as it
//! was, with one dimmed line at the bottom, and keys are dropped (typing into a shell
//! that may not receive it, and having it arrive minutes later, is worse than losing
//! it). When the stream is back (`S` online) the terminal is reset first, because the
//! frozen frame may have left any mode on, and tmux repaints the rest.
use crate::{
    client::load_host,
    client_daemon::{
        AttachRequest, FRAME_DATA, FRAME_END, FRAME_RESIZE, FRAME_STATUS, daemon_attach, ensure,
        read_frame, write_frame,
    },
    config::Storage,
    crypto::uuid,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::fd::{AsRawFd, RawFd},
    path::Path,
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    sync::mpsc,
    time::sleep,
};

/// What the terminal gets before output resumes after an interruption: a soft
/// reset (`ESC [ ! p`), the alternate screen left, mouse reporting, focus events and
/// bracketed paste off, the cursor shown and attributes cleared. tmux sets up what
/// it needs again when it repaints.
pub const RESET: &[u8] = b"\x1b[!p\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1016l\x1b[?1004l\x1b[?2004l\x1b[?25h\x1b[0m";

/// The TERM to ask the host for: Ghostty's own when that is what this terminal is,
/// the common one otherwise.
pub fn pick_term(term: Option<&str>) -> &'static str {
    if term.is_some_and(|t| t.contains("ghostty")) {
        "xterm-ghostty"
    } else {
        "xterm-256color"
    }
}

/// A dimmed line on the last row, drawn without moving the cursor or touching
/// anything else on the screen.
pub fn notice_bytes(rows: u16, columns: u16, text: &str) -> Vec<u8> {
    let width = usize::from(columns).saturating_sub(1).max(1);
    let text: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(width)
        .collect();
    format!("\x1b7\x1b[{rows};1H\x1b[2K\x1b[2m{text}\x1b[0m\x1b8").into_bytes()
}
/// Erases what [`notice_bytes`] drew.
pub fn clear_notice_bytes(rows: u16) -> Vec<u8> {
    format!("\x1b7\x1b[{rows};1H\x1b[2K\x1b8").into_bytes()
}
/// The sentence printed when the daemon ends the stream. `\r\n` because raw mode
/// turns off the terminal's own newline handling.
pub fn end_message(reason: &str) -> String {
    let sentence = match reason {
        "exited" => "The session ended.".to_owned(),
        "closed" => "The session was closed.".to_owned(),
        "limit" => "The host cannot open more terminals for this Mac right now.".to_owned(),
        other => other.chars().filter(|c| !c.is_control()).collect(),
    };
    format!("\r\n[riwork] {sentence}\r\n")
}

fn env_number(name: &str) -> Option<u16> {
    std::env::var(name).ok()?.parse().ok().filter(|n| *n > 0)
}
/// The size of the terminal this process is attached to.
pub fn tty_size() -> (u16, u16) {
    for fd in [1, 0, 2] {
        let mut size: libc::winsize = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCGWINSZ writes one `winsize` through the pointer we pass.
        let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } == 0;
        if ok && size.ws_col > 0 && size.ws_row > 0 {
            return (size.ws_col.min(1000), size.ws_row.min(500));
        }
    }
    (
        env_number("COLUMNS").unwrap_or(80).min(1000),
        env_number("LINES").unwrap_or(24).min(500),
    )
}

/// The terminal in raw mode, put back by `Drop`.
struct RawMode {
    fd: RawFd,
    saved: libc::termios,
}
impl RawMode {
    /// `None` when stdin is not a terminal (a pipe in a test, say): then there is
    /// nothing to change.
    fn enable() -> Result<Option<Self>> {
        let fd = std::io::stdin().as_raw_fd();
        // SAFETY: isatty only inspects the descriptor.
        if unsafe { libc::isatty(fd) } != 1 {
            return Ok(None);
        }
        // SAFETY: tcgetattr fills the termios we give it; an all-zero one is a valid buffer.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return Err(std::io::Error::last_os_error()).context("read the terminal settings");
        }
        let mut raw = saved;
        // SAFETY: cfmakeraw edits the struct it is given.
        unsafe { libc::cfmakeraw(&mut raw) };
        // SAFETY: tcsetattr reads the struct it is given.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(std::io::Error::last_os_error()).context("set the terminal to raw mode");
        }
        Ok(Some(Self { fd, saved }))
    }
}
impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: tcsetattr reads the saved struct; a failure leaves the terminal as it is.
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
    }
}

/// The terminal's output, written by a thread of its own so that a write that blocks
/// (a held-up window) does not block the runtime's other work. The queue to the thread
/// is bounded, so a terminal that stays blocked eventually holds the bridge back, and
/// with it the daemon and the host: that is the backpressure a real terminal exerts.
struct Tty {
    tx: mpsc::Sender<Vec<u8>>,
    finished: std::sync::mpsc::Receiver<()>,
    /// Fires once when the terminal can no longer be written (it was closed).
    gone: mpsc::UnboundedReceiver<()>,
}
impl Tty {
    fn spawn() -> Self {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
        let (done, finished) = std::sync::mpsc::channel();
        let (gone_tx, gone) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let mut out = std::io::stdout();
            while let Some(bytes) = rx.blocking_recv() {
                if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
                    let _ = gone_tx.send(());
                    break;
                }
            }
            let _ = done.send(());
        });
        Self { tx, finished, gone }
    }
    async fn write(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(bytes).await;
    }
    /// Waits (briefly) for everything queued to reach the terminal.
    fn finish(self) {
        drop(self.tx);
        let _ = self.finished.recv_timeout(Duration::from_secs(2));
    }
}

/// Keys, read on a thread because a terminal read cannot be cancelled. The thread
/// ends with the process.
fn spawn_stdin() -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel(64);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.blocking_send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    rx
}

/// What the screen has been through, across attaches to a daemon.
#[derive(Default)]
struct Screen {
    /// The stream was interrupted since the terminal was last known clean.
    dirty: bool,
    notice_rows: Option<u16>,
}

enum Ended {
    /// The daemon ended the stream with this reason.
    Stream(String),
    /// The terminal or a signal ended the bridge.
    Exit(i32),
    /// The daemon connection broke without an end.
    DaemonLost,
}

/// Runs the bridge; the exit code is the process's.
pub async fn run(
    storage: &Storage,
    desktop: &str,
    shell: &str,
    ignore_size: bool,
    exe: &Path,
) -> Result<i32> {
    uuid(desktop).context("--desktop takes a host's full desktop id")?;
    uuid(shell).context("--shell takes a shell's full UUID")?;
    // Fail on an unknown host before the terminal is touched.
    load_host(storage, desktop)?;
    let raw = RawMode::enable()?;
    let result = attach_loop(storage, desktop, shell, ignore_size, exe).await;
    drop(raw);
    result
}

async fn attach_loop(
    storage: &Storage,
    desktop: &str,
    shell: &str,
    ignore_size: bool,
    exe: &Path,
) -> Result<i32> {
    let mut signals = Signals::new()?;
    let mut tty = Tty::spawn();
    let mut keys = spawn_stdin();
    let term = pick_term(std::env::var("TERM").ok().as_deref());
    let mut screen = Screen::default();
    let mut failures = 0u32;
    let outcome = loop {
        let (columns, rows) = tty_size();
        let request = AttachRequest {
            shell_id: shell.into(),
            columns,
            rows,
            term: term.into(),
            ignore_size,
        };
        let attempt = async {
            let socket = ensure(storage, desktop, exe).await?;
            daemon_attach(&socket, &request).await
        };
        // A signal ends the bridge at any point, not only while it is copying.
        let attempt = tokio::select! {
            attempt = attempt => attempt,
            code = signals.exits.wait() => break Ok(code),
        };
        let stream = match attempt {
            Ok(stream) => stream,
            Err(e) => {
                failures += 1;
                if failures >= 5 {
                    break Err(e.context("cannot reach the client daemon"));
                }
                show_notice(&mut tty, &mut screen, "Waiting for the client daemon…").await;
                tokio::select! {
                    () = sleep(Duration::from_secs(1)) => {}
                    code = signals.exits.wait() => break Ok(code),
                }
                continue;
            }
        };
        let ended = pump(
            stream,
            (columns, rows),
            &mut tty,
            &mut keys,
            &mut screen,
            &mut failures,
            &mut signals,
        )
        .await;
        match ended {
            Ended::Stream(reason) => {
                clear_screen_state(&mut tty, &mut screen).await;
                tty.write(end_message(&reason).into_bytes()).await;
                break Ok(0);
            }
            Ended::Exit(code) => break Ok(code),
            Ended::DaemonLost => {
                failures += 1;
                if failures >= 5 {
                    break Err(anyhow::anyhow!(
                        "the client daemon keeps closing the connection"
                    ));
                }
                screen.dirty = true;
                show_notice(&mut tty, &mut screen, "Reconnecting to the client daemon…").await;
                tokio::select! {
                    () = sleep(Duration::from_secs(1)) => {}
                    code = signals.exits.wait() => break Ok(code),
                }
            }
        }
    };
    if outcome.is_err() {
        clear_screen_state(&mut tty, &mut screen).await;
    }
    tty.finish();
    outcome
}

async fn show_notice(tty: &mut Tty, screen: &mut Screen, text: &str) {
    let (columns, rows) = tty_size();
    tty.write(notice_bytes(rows, columns, text)).await;
    screen.notice_rows = Some(rows);
}
/// Takes the notice away, and the modes the interrupted tmux may have left on, so a
/// message printed after them reads on a plain screen.
async fn clear_screen_state(tty: &mut Tty, screen: &mut Screen) {
    if screen.dirty || screen.notice_rows.is_some() {
        tty.write(RESET.to_vec()).await;
        screen.dirty = false;
        screen.notice_rows = None;
    }
}

/// The signals the bridge acts on.
struct Signals {
    winch: tokio::signal::unix::Signal,
    exits: Exits,
}
/// The ones that end it.
struct Exits {
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
    interrupt: tokio::signal::unix::Signal,
}
impl Signals {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            winch: signal(SignalKind::window_change())?,
            exits: Exits {
                terminate: signal(SignalKind::terminate())?,
                hangup: signal(SignalKind::hangup())?,
                interrupt: signal(SignalKind::interrupt())?,
            },
        })
    }
}
impl Exits {
    /// The exit code of whichever arrives first, as a shell reports a signal.
    async fn wait(&mut self) -> i32 {
        tokio::select! {
            _ = self.terminate.recv() => 143,
            _ = self.hangup.recv() => 129,
            _ = self.interrupt.recv() => 130,
        }
    }
}

/// Stops the task that reads the daemon's frames when the connection is done with.
struct StopOnDrop(tokio::task::JoinHandle<()>);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

type Frame = std::io::Result<Option<(u8, Vec<u8>)>>;

/// Copies between the terminal and one daemon connection until it ends. `size` is the
/// window size the attach was requested with: a change after that is still to be sent.
async fn pump(
    stream: tokio::net::UnixStream,
    mut size: (u16, u16),
    tty: &mut Tty,
    keys: &mut mpsc::Receiver<Vec<u8>>,
    screen: &mut Screen,
    failures: &mut u32,
    signals: &mut Signals,
) -> Ended {
    let (read, mut write) = stream.into_split();
    // A frame is read whole by a task of its own: a read abandoned half way by a
    // `select!` (a key, a signal) would lose the bytes it had taken and desync the stream.
    let (frames_tx, mut frames) = mpsc::channel::<Frame>(8);
    let _reader = StopOnDrop(tokio::spawn(async move {
        let mut read = BufReader::new(read);
        loop {
            let frame = read_frame(&mut read).await;
            let last = !matches!(frame, Ok(Some(_)));
            if frames_tx.send(frame).await.is_err() || last {
                break;
            }
        }
    }));
    let mut online = false;
    let mut keys_open = true;
    loop {
        tokio::select! {
            frame = frames.recv() => match frame.unwrap_or(Ok(None)) {
                Ok(Some((FRAME_DATA, data))) => tty.write(data).await,
                Ok(Some((FRAME_STATUS, payload))) => {
                    let status: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                    match status["state"].as_str() {
                        Some("online") => {
                            online = true;
                            *failures = 0;
                            if screen.dirty {
                                tty.write(RESET.to_vec()).await;
                                screen.dirty = false;
                            } else if let Some(rows) = screen.notice_rows {
                                tty.write(clear_notice_bytes(rows)).await;
                            }
                            screen.notice_rows = None;
                            // The window may have changed while no one was listening.
                            let now = tty_size();
                            if now != size {
                                size = now;
                                let _ = send_size(&mut write, now).await;
                            }
                        }
                        Some(state) => {
                            online = false;
                            // A screen is only stale once the stream has been down.
                            screen.dirty |= state == "offline";
                            let host = status["label"].as_str().unwrap_or("the host");
                            let text = if state == "connecting" {
                                format!("Connecting to {host}…")
                            } else {
                                format!("{host} is unreachable; waiting to reconnect…")
                            };
                            show_notice(tty, screen, &text).await;
                        }
                        None => {}
                    }
                }
                Ok(Some((FRAME_END, payload))) => {
                    let end: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                    return Ended::Stream(end["reason"].as_str().unwrap_or("closed").to_owned());
                }
                // A frame type from a newer daemon.
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return Ended::DaemonLost,
            },
            keys_in = keys.recv(), if keys_open => match keys_in {
                // Keys typed while the stream is down are dropped, never replayed.
                Some(bytes) if online => {
                    if write_frame(&mut write, FRAME_DATA, &bytes).await.is_err() {
                        return Ended::DaemonLost;
                    }
                }
                Some(_) => {}
                // Input ended: a pipe, not a terminal. Output goes on until the end.
                None => keys_open = false,
            },
            _ = signals.winch.recv() => {
                let now = tty_size();
                if now != size {
                    size = now;
                    if send_size(&mut write, now).await.is_err() {
                        return Ended::DaemonLost;
                    }
                }
            },
            Some(()) = tty.gone.recv() => return Ended::Exit(0),
            code = signals.exits.wait() => return Ended::Exit(code),
        }
    }
}

async fn send_size(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    size: (u16, u16),
) -> std::io::Result<()> {
    let payload = serde_json::to_vec(&json!({"columns":size.0,"rows":size.1}))?;
    write_frame(write, FRAME_RESIZE, &payload).await?;
    write.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghostty_asks_for_its_own_terminfo_and_everything_else_for_the_common_one() {
        assert_eq!(pick_term(Some("xterm-ghostty")), "xterm-ghostty");
        assert_eq!(pick_term(Some("ghostty")), "xterm-ghostty");
        assert_eq!(pick_term(Some("xterm-256color")), "xterm-256color");
        assert_eq!(pick_term(Some("screen")), "xterm-256color");
        assert_eq!(pick_term(None), "xterm-256color");
    }

    #[test]
    fn the_reset_leaves_every_mode_tmux_may_have_turned_on() {
        let text = std::str::from_utf8(RESET).unwrap();
        assert!(text.starts_with("\x1b[!p"), "soft reset first");
        for mode in ["1049", "1000", "1002", "1003", "1006", "1004", "2004"] {
            assert!(text.contains(&format!("\x1b[?{mode}l")), "{mode}");
        }
        assert!(text.contains("\x1b[?25h"), "cursor shown");
    }

    #[test]
    fn the_notice_is_one_dim_line_on_the_last_row_that_keeps_the_cursor() {
        let bytes = notice_bytes(24, 80, "Mac Studio is unreachable");
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text.starts_with("\x1b7") && text.ends_with("\x1b8"),
            "cursor saved and restored"
        );
        assert!(text.contains("\x1b[24;1H") && text.contains("\x1b[2m"));
        assert!(text.contains("Mac Studio is unreachable"));
        // Never wider than the window, and never a control character from a label.
        let narrow = String::from_utf8(notice_bytes(10, 12, "abcdefghijklmnopqrstuvwxyz")).unwrap();
        assert!(narrow.contains("abcdefghijk") && !narrow.contains("abcdefghijkl"));
        let hostile = String::from_utf8(notice_bytes(5, 80, "a\x1b[2Jb\nc")).unwrap();
        assert!(hostile.contains("a[2Jbc") && !hostile.contains("\x1b[2J"));
        assert!(
            String::from_utf8(clear_notice_bytes(24))
                .unwrap()
                .contains("\x1b[24;1H\x1b[2K")
        );
    }

    #[test]
    fn the_end_reasons_read_as_sentences() {
        assert_eq!(end_message("exited"), "\r\n[riwork] The session ended.\r\n");
        assert!(end_message("closed").contains("closed"));
        assert!(end_message("limit").contains("more terminals"));
        assert_eq!(
            end_message("not_found: no such shell"),
            "\r\n[riwork] not_found: no such shell\r\n"
        );
        assert!(!end_message("a\x1bb").contains('\x1b'));
    }

    #[test]
    fn the_size_comes_from_the_environment_when_there_is_no_terminal() {
        // The test harness has no terminal on fds 0-2 under `cargo test`; whichever
        // happens, the answer is a usable size.
        let (columns, rows) = tty_size();
        assert!((1..=1000).contains(&columns) && (1..=500).contains(&rows));
    }
}
