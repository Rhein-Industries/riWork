//! What both drivers need from a provider child process: a private process
//! group, a line reader with a size cap, and a shutdown that escalates.
//!
//! The providers speak one JSON object per line, and single lines can be huge
//! (a 49-million-character diff has been seen), so `FrameReader` never buffers
//! more than its cap: it drops an oversized line as it streams past and says how
//! big it was. `Proc` owns the child and its input; the driver's reader thread
//! owns the child's output.

use super::driver::DriverConfig;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// The longest provider line a driver will read; longer ones are skipped.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// How long a provider gets to leave by itself once its input is closed.
pub const GRACE: Duration = Duration::from_secs(5);
/// How long it gets after SIGTERM before SIGKILL.
pub const TERM_WAIT: Duration = Duration::from_secs(2);
pub const WRITE_WAIT: Duration = Duration::from_secs(30);

/// Zero written bytes are a refusal; any prefix may have reached the provider.
#[derive(Debug)]
pub struct WriteFailure {
    pub written: usize,
    pub message: String,
}
impl std::fmt::Display for WriteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({} bytes written)", self.message, self.written)
    }
}
/// The end of a child's stderr kept to explain why it died.
const STDERR_TAIL_BYTES: usize = 4096;

/// One line of a provider's output.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// A line without its newline.
    Line(Vec<u8>),
    /// A line longer than the cap, dropped; this many bytes long.
    Oversized(u64),
}

/// Reads lines from a stream, keeping at most `cap` bytes of any one.
pub struct FrameReader<R: Read> {
    reader: BufReader<R>,
    cap: usize,
}

impl<R: Read> FrameReader<R> {
    pub fn new(reader: R, cap: usize) -> Self {
        Self {
            reader: BufReader::with_capacity(64 * 1024, reader),
            cap,
        }
    }

    /// The next line, or `None` at the end of the stream. A last line without a
    /// newline still counts.
    pub fn next_frame(&mut self) -> io::Result<Option<Frame>> {
        let mut line = Vec::new();
        // Bytes of an oversized line seen so far; `None` while it fits.
        let mut skipped: Option<u64> = None;
        loop {
            let buffer = match self.reader.fill_buf() {
                Ok(buffer) => buffer,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if buffer.is_empty() {
                return Ok(match skipped {
                    Some(bytes) => Some(Frame::Oversized(bytes)),
                    None if line.is_empty() => None,
                    None => Some(Frame::Line(trim_carriage_return(line))),
                });
            }
            let newline = buffer.iter().position(|&byte| byte == b'\n');
            let chunk = &buffer[..newline.unwrap_or(buffer.len())];
            match &mut skipped {
                Some(bytes) => *bytes += chunk.len() as u64,
                None if line.len() + chunk.len() > self.cap => {
                    skipped = Some((line.len() + chunk.len()) as u64);
                    line = Vec::new();
                }
                None => line.extend_from_slice(chunk),
            }
            let consumed = chunk.len() + usize::from(newline.is_some());
            self.reader.consume(consumed);
            if newline.is_some() {
                return Ok(Some(match skipped {
                    Some(bytes) => Frame::Oversized(bytes),
                    None => Frame::Line(trim_carriage_return(line)),
                }));
            }
        }
    }
}

fn trim_carriage_return(mut line: Vec<u8>) -> Vec<u8> {
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    line
}

/// The command for `config`: its program, `extra_args`, then the driver's own
/// arguments, in the chat's directory with its environment changes applied.
pub fn command(config: &DriverConfig, driver_args: &[&str]) -> Command {
    let mut command = Command::new(&config.program);
    command
        .args(&config.extra_args)
        .args(driver_args)
        .current_dir(&config.cwd);
    for (name, value) in &config.env {
        command.env(name, value);
    }
    for name in &config.env_remove {
        command.env_remove(name);
    }
    command
}

/// A running provider process: its input, the tail of its stderr and the
/// signals that stop it. It leads a process group of its own, so everything it
/// starts (shell commands, MCP servers) can be stopped with it.
pub struct Proc {
    // All signals and consuming waits use this same lifetime lock. An atomic
    // "reaped" flag alone cannot prevent a signal racing PID reuse.
    child: Mutex<OwnedChild>,
    pid: libc::pid_t,
    stdin: Mutex<Option<ChildStdin>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    /// The stderr drain has seen the end of the stream.
    stderr_done: Arc<AtomicBool>,
    writes_cancelled: AtomicBool,
}

struct OwnedChild {
    child: Child,
    /// Set before any consuming wait, never reset after reaping.
    retired: bool,
}

/// Group targets must be this child's positive, private group, never 0/-1,
/// the caller's group, or a group supplied by a caller. This is only a target
/// guard: the caller must also hold the lifetime lock and confirm an unreaped
/// child immediately before signalling.
fn owned_signal_target(
    pid: libc::pid_t,
    child_id: u32,
    retired: bool,
    group: Option<(libc::pid_t, libc::pid_t)>,
) -> Option<libc::pid_t> {
    if retired || pid <= 1 || u32::try_from(pid).ok() != Some(child_id) {
        return None;
    }
    match group {
        Some((actual, caller)) if actual == pid && actual != caller => Some(-pid),
        Some(_) => None,
        None => Some(pid),
    }
}

impl OwnedChild {
    /// Observe without reaping. Even an exited child pins its PID until our
    /// consuming wait, so group cleanup can safely precede that wait.
    fn exited(&mut self, pid: libc::pid_t) -> io::Result<bool> {
        if owned_signal_target(pid, self.child.id(), self.retired, None).is_none() {
            return Err(io::Error::from_raw_os_error(libc::ECHILD));
        }
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                let observed = unsafe { info.si_pid() };
                if observed == 0 {
                    return Ok(false);
                }
                if observed == pid {
                    return Ok(true);
                }
                // Fail closed if the OS did not confirm this exact child.
                self.retired = true;
                return Err(io::Error::from_raw_os_error(libc::ECHILD));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.retired = true;
            }
            return Err(error);
        }
    }

    /// Only after a non-consuming observation (or try_wait == None on the
    /// shutdown fallback), with the lifetime lock held through kill. No other
    /// Proc method can release this PID between confirmation and signalling.
    fn signal_pinned(&self, pid: libc::pid_t, signal: libc::c_int, group: bool) -> bool {
        if owned_signal_target(pid, self.child.id(), self.retired, None).is_none() {
            return false;
        }
        let group = group.then(|| unsafe { (libc::getpgid(pid), libc::getpgrp()) });
        let Some(target) = owned_signal_target(pid, self.child.id(), self.retired, group) else {
            return false;
        };
        unsafe { libc::kill(target, signal) == 0 }
    }

    fn reap_exited(&mut self, pid: libc::pid_t) -> Option<ExitStatus> {
        // WNOWAIT confirmed an exited, still-owned leader. Sweep only now,
        // before reaping; never send a signal using its cached exit status.
        self.signal_pinned(pid, libc::SIGKILL, true);
        self.retired = true;
        self.child.try_wait().ok().flatten()
    }

    fn try_wait(&mut self, pid: libc::pid_t) -> Option<ExitStatus> {
        if self.retired {
            return self.child.try_wait().ok().flatten();
        }
        match self.exited(pid) {
            Ok(true) => self.reap_exited(pid),
            Ok(false) => None,
            Err(_) if self.retired => self.child.try_wait().ok().flatten(),
            Err(_) => None,
        }
    }

    fn signal(&mut self, pid: libc::pid_t, signal: libc::c_int, group: bool) {
        match self.exited(pid) {
            Ok(false) => {
                self.signal_pinned(pid, signal, group);
            }
            Ok(true) => {
                self.reap_exited(pid);
            }
            Err(_) => {} // Unconfirmed/reaped ownership never permits a signal.
        }
    }

    fn kill_and_wait(&mut self, pid: libc::pid_t) -> Option<ExitStatus> {
        if self.retired {
            return self.child.wait().ok();
        }
        if self.exited(pid).is_err() {
            // If WNOWAIT failed, a consuming try_wait may still confirm a
            // running child. Retire before that wait; never sweep after Some.
            let unconfirmed = self.retired;
            self.retired = true;
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if !unconfirmed => self.retired = false,
                _ => return None,
            }
        }
        if !self.signal_pinned(pid, libc::SIGKILL, true) {
            // An owned child may have left its private group. Finish only the
            // confirmed, unreaped child; never target its new/shared group.
            self.signal_pinned(pid, libc::SIGKILL, false);
        }
        self.retired = true;
        self.child.wait().ok()
    }
}

impl Proc {
    /// Start `command` with piped input and output in a new process group.
    /// The caller reads the returned output; stderr is drained here.
    pub fn spawn(mut command: Command) -> Result<(Arc<Proc>, ChildStdout), String> {
        let program = command.get_program().to_string_lossy().into_owned();
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot start {program}: {error}"))?;
        let stdout = child.stdout.take().ok_or("provider has no output")?;
        let stdin = child.stdin.take();
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        let stderr_done = Arc::new(AtomicBool::new(false));
        if let Some(mut stderr) = child.stderr.take() {
            let tail = Arc::clone(&stderr_tail);
            let done = Arc::clone(&stderr_done);
            thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                loop {
                    match stderr.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                            tail.extend_from_slice(&chunk[..count]);
                            let excess = tail.len().saturating_sub(STDERR_TAIL_BYTES);
                            tail.drain(..excess);
                        }
                    }
                }
                done.store(true, Ordering::Release);
            });
        } else {
            stderr_done.store(true, Ordering::Release);
        }
        let proc = Proc {
            pid: child.id() as libc::pid_t,
            child: Mutex::new(OwnedChild {
                child,
                retired: false,
            }),
            stdin: Mutex::new(stdin),
            stderr_tail,
            stderr_done,
            writes_cancelled: AtomicBool::new(false),
        };
        // Do this after constructing the owner: a failure drops/reaps only this child.
        if let Some(input) = proc
            .stdin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let fd = input.as_raw_fd();
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(format!(
                    "cannot make provider input nonblocking: {}",
                    io::Error::last_os_error()
                ));
            }
        }
        Ok((Arc::new(proc), stdout))
    }

    pub fn id(&self) -> u32 {
        self.pid as u32
    }

    /// Write one line to the provider. Fails once its input is closed or gone.
    pub fn send_line(&self, line: &str) -> Result<(), String> {
        self.send_line_until(line, Instant::now() + WRITE_WAIT)
            .map_err(|e| {
                if e.written == 0 {
                    e.to_string()
                } else {
                    format!(
                        "{} {e}; inspect the transcript before resending",
                        super::attachments::UNKNOWN_SUBMISSION
                    )
                }
            })
    }

    /// Covers mutex acquisition and pipe backpressure. No detached writer can outlive
    /// the deadline. A partial JSON line poisons the input: never append another frame.
    pub fn send_line_until(&self, line: &str, deadline: Instant) -> Result<(), WriteFailure> {
        let failure = |written, message: &str| WriteFailure {
            written,
            message: message.into(),
        };
        let mut stdin = loop {
            if self.writes_cancelled() {
                return Err(failure(0, "provider input was cancelled"));
            }
            if Instant::now() >= deadline {
                return Err(failure(0, "provider write deadline expired"));
            }
            match self.stdin.try_lock() {
                Ok(input) => break input,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(2)),
            }
        };
        let input = stdin
            .as_mut()
            .ok_or_else(|| failure(0, "the provider's input is closed"))?;
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        let mut written = 0;
        let result = loop {
            if self.writes_cancelled() {
                break Err(failure(written, "provider input was cancelled"));
            }
            if Instant::now() >= deadline {
                break Err(failure(written, "provider write deadline expired"));
            }
            match input.write(&bytes[written..]) {
                Ok(0) => break Err(failure(written, "provider input closed during write")),
                Ok(count) => {
                    written += count;
                    if written == bytes.len() {
                        break Ok(());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let mut fd = libc::pollfd {
                        fd: input.as_raw_fd(),
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    let timeout = remaining.as_millis().min(20) as libc::c_int;
                    let ready = unsafe { libc::poll(&mut fd, 1, timeout) };
                    if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                    {
                        break Err(failure(written, "cannot poll provider input"));
                    }
                }
                Err(error) => {
                    break Err(WriteFailure {
                        written,
                        message: format!("cannot write to provider: {error}"),
                    });
                }
            }
        };
        if result.is_err() && written > 0 {
            self.cancel_writes();
            stdin.take();
        }
        result
    }

    /// Accessible without the driver or stdin mutex, including during backpressure.
    /// Cancellation is terminal for this owned child, never a resend request.
    pub fn cancel_writes(&self) {
        self.writes_cancelled.store(true, Ordering::Release);
    }
    pub fn writes_cancelled(&self) -> bool {
        self.writes_cancelled.load(Ordering::Acquire)
    }

    pub fn send(&self, value: &serde_json::Value) -> Result<(), String> {
        self.send_line(&value.to_string())
    }

    /// Close the provider's input, which tells it to finish.
    pub fn close_stdin(&self) {
        self.cancel_writes();
        self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    /// The exit status once the process has ended, or `None` while it runs.
    pub fn try_wait(&self) -> Option<ExitStatus> {
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_wait(self.pid)
    }

    /// Wait up to `timeout` for the process to end.
    pub fn wait_exit(&self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Signal the process itself, not what it started.
    pub fn signal(&self, signal: libc::c_int) {
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .signal(self.pid, signal, false);
    }

    /// Signal the whole process group (not once the process is gone).
    pub fn signal_group(&self, signal: libc::c_int) {
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .signal(self.pid, signal, true);
    }

    /// Why the process ended, for a message to the user: its status (waiting up
    /// to `wait` for it) and the end of what it wrote to stderr.
    pub fn describe_exit(&self, wait: Duration) -> String {
        let status = self.wait_exit(wait);
        // Let the stderr drain catch up with an exit it just saw; something the
        // process left running may hold the stream open, so only briefly.
        let deadline = Instant::now() + Duration::from_millis(500);
        while status.is_some()
            && !self.stderr_done.load(Ordering::Acquire)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        let mut text = match status {
            Some(status) => format!("exited ({status})"),
            None => "closed its output".to_owned(),
        };
        let tail = self.stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
        let tail = String::from_utf8_lossy(&tail);
        let tail = tail.trim();
        if !tail.is_empty() {
            text.push_str(": ");
            text.push_str(tail);
        }
        text
    }

    /// Stop the process and everything it started: close its input and give it
    /// `GRACE` to leave, then SIGTERM, then SIGKILL, and reap it.
    pub fn stop(&self) {
        self.stop_after(GRACE, TERM_WAIT);
    }

    pub fn stop_after(&self, grace: Duration, term_wait: Duration) {
        self.cancel_writes();
        // A nonblocking writer releases the lock after observing cancellation.
        // Keep the existing owned-child shutdown escalation policy.
        if let Ok(mut stdin) = self.stdin.try_lock() {
            stdin.take();
        }
        if self.wait_exit(grace).is_none() {
            self.signal_group(libc::SIGTERM);
            if self.wait_exit(term_wait).is_none() {
                self.child
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .kill_and_wait(self.pid);
            }
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.child
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .kill_and_wait(self.pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frames(input: &[u8], cap: usize) -> Vec<Frame> {
        let mut reader = FrameReader::new(Cursor::new(input.to_vec()), cap);
        let mut frames = Vec::new();
        while let Some(frame) = reader.next_frame().unwrap() {
            frames.push(frame);
        }
        frames
    }

    fn line(text: &str) -> Frame {
        Frame::Line(text.as_bytes().to_vec())
    }

    #[test]
    fn lines_are_split_and_carriage_returns_dropped() {
        assert_eq!(
            frames(b"{\"a\":1}\r\n\nsecond\nlast", 100),
            vec![line("{\"a\":1}"), line(""), line("second"), line("last")]
        );
    }

    #[test]
    fn an_oversized_line_is_skipped_across_reads_and_at_the_end_of_input() {
        // Longer than the reader's buffer, with no newline at the end.
        let input = vec![b'y'; 200_000];
        assert_eq!(frames(&input, 1024), vec![Frame::Oversized(200_000)]);
        // Exactly at the cap still fits.
        assert_eq!(frames(b"abcd\n", 4), vec![line("abcd")]);
        assert_eq!(frames(b"abcde\n", 4), vec![Frame::Oversized(5)]);
    }

    #[test]
    fn owned_signal_targets_reject_broadcast_shared_mismatched_and_retired_ids() {
        // Pure policy assertions: these numbers are data, never OS targets.
        for pid in [libc::pid_t::MIN, -1, 0, 1] {
            assert_eq!(owned_signal_target(pid, 42, false, None), None);
            assert_eq!(owned_signal_target(pid, 42, false, Some((pid, 7))), None);
        }
        assert_eq!(owned_signal_target(42, 43, false, None), None);
        assert_eq!(owned_signal_target(42, 42, false, Some((43, 7))), None);
        assert_eq!(owned_signal_target(42, 42, false, Some((-1, 7))), None);
        assert_eq!(owned_signal_target(42, 42, false, Some((42, 42))), None);
        assert_eq!(owned_signal_target(42, 42, true, None), None);
        assert_eq!(owned_signal_target(42, 42, true, Some((42, 7))), None);
        assert_eq!(owned_signal_target(42, 42, false, None), Some(42));
        assert_eq!(owned_signal_target(42, 42, false, Some((42, 7))), Some(-42));
    }

    #[test]
    fn owned_fake_signalling_and_reaping_share_the_lifetime_lock() {
        use crate::chat::{model::Provider, testkit::Fake};
        use std::sync::{Barrier, mpsc};
        let fake = Fake::new(&["{\"type\":\"hang\"}\n"]);
        let (proc, _stdout) = Proc::spawn(command(&fake.config(Provider::Codex), &[])).unwrap();
        let held = proc.child.lock().unwrap();
        let ready = Arc::new(Barrier::new(3));
        let (signal_sent, signal_done) = mpsc::channel();
        let signalling = proc.clone();
        let signal_ready = ready.clone();
        let signaller = thread::spawn(move || {
            signal_ready.wait();
            signalling.signal_group(libc::SIGTERM);
            signal_sent.send(()).unwrap();
        });
        let (reap_sent, reap_done) = mpsc::channel();
        let reaping = proc.clone();
        let reap_ready = ready.clone();
        let reaper = thread::spawn(move || {
            reap_ready.wait();
            reap_sent
                .send(reaping.wait_exit(Duration::from_secs(2)))
                .unwrap();
        });
        ready.wait();
        // Both operations must wait for ownership, while writer cancellation
        // remains accessible independently of this lock and of stdin.
        assert!(matches!(
            signal_done.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(matches!(
            reap_done.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        proc.cancel_writes();
        assert!(proc.writes_cancelled());
        drop(held);
        signal_done.recv_timeout(Duration::from_secs(2)).unwrap();
        let status = reap_done
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .expect("only the fixed owned fake must exit");
        signaller.join().unwrap();
        reaper.join().unwrap();
        assert!(proc.child.lock().unwrap().retired);
        // Repeated callers receive the cached status and cannot
        // restore signal eligibility once the child has been consumed.
        proc.signal_group(libc::SIGKILL);
        proc.signal(libc::SIGINT);
        assert_eq!(proc.try_wait(), Some(status));
        let owned = proc.child.lock().unwrap();
        assert_eq!(
            owned_signal_target(proc.pid, owned.child.id(), owned.retired, None),
            None
        );
    }

    fn script(body: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(body);
        command
    }

    #[test]
    fn attachment_pipe_backpressure_has_a_real_deadline_and_poisoned_prefix() {
        use crate::chat::{model::Provider, testkit::Fake};
        let fake = Fake::new(&["{\"type\":\"hang\"}\n"]);
        let (proc, _stdout) = Proc::spawn(command(&fake.config(Provider::Codex), &[])).unwrap();
        let started = Instant::now();
        let error = proc
            .send_line_until(&"x".repeat(12 << 20), started + Duration::from_millis(200))
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            error.written > 0,
            "a pipe prefix is ambiguous, never a known refusal"
        );
        assert!(proc.writes_cancelled());
        let late = proc
            .send_line_until(
                "never append after a partial JSON frame",
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(late.written, 0);
        proc.stop_after(Duration::ZERO, Duration::from_millis(100));
    }

    #[test]
    fn attachment_pipe_cancellation_does_not_need_the_stdin_mutex() {
        use crate::chat::{model::Provider, testkit::Fake};
        let fake = Fake::new(&["{\"type\":\"hang\"}\n"]);
        let (proc, _stdout) = Proc::spawn(command(&fake.config(Provider::Codex), &[])).unwrap();
        let writing = proc.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let writer = thread::spawn(move || {
            let result = writing.send_line_until(
                &"x".repeat(12 << 20),
                Instant::now() + Duration::from_secs(10),
            );
            sent.send(result).unwrap();
        });
        // Wait for the owned writer to hold stdin, without depending on pipe capacity.
        let deadline = Instant::now() + Duration::from_secs(2);
        while proc.stdin.try_lock().is_ok() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        proc.cancel_writes();
        let error = received
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err();
        assert!(error.message.contains("cancelled"));
        writer.join().unwrap();
        proc.stop_after(Duration::ZERO, Duration::from_millis(100));
    }

    #[test]
    fn attachment_write_deadline_covers_waiting_for_the_writer_mutex() {
        use crate::chat::{model::Provider, testkit::Fake};
        let fake = Fake::new(&["{\"type\":\"hang\"}\n"]);
        let (proc, _stdout) = Proc::spawn(command(&fake.config(Provider::Codex), &[])).unwrap();
        let held = proc.stdin.lock().unwrap();
        let error = proc
            .send_line_until("no bytes", Instant::now() + Duration::from_millis(30))
            .unwrap_err();
        assert_eq!(error.written, 0);
        assert!(
            !proc.writes_cancelled(),
            "a zero-byte deadline does not corrupt the pipe"
        );
        drop(held);
        proc.stop_after(Duration::ZERO, Duration::from_millis(100));
    }

    #[test]
    fn a_child_leads_its_own_process_group_and_stdin_reaches_it() {
        let (proc, stdout) = Proc::spawn(script("read line; echo \"got $line\"")).unwrap();
        assert_eq!(
            unsafe { libc::getpgid(proc.id() as libc::pid_t) },
            proc.id() as libc::pid_t
        );
        proc.send_line("hello").unwrap();
        let mut reader = FrameReader::new(stdout, 100);
        assert_eq!(reader.next_frame().unwrap(), Some(line("got hello")));
        assert_eq!(reader.next_frame().unwrap(), None);
        assert!(proc.wait_exit(Duration::from_secs(5)).unwrap().success());
    }

    #[test]
    fn stopping_closes_input_first_and_lets_the_child_leave_by_itself() {
        let (proc, _stdout) = Proc::spawn(script("cat >/dev/null; exit 3")).unwrap();
        let started = Instant::now();
        proc.stop_after(Duration::from_secs(10), Duration::from_secs(10));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(proc.try_wait().unwrap().code(), Some(3));
        assert!(proc.send_line("late").is_err());
    }

    #[test]
    fn stopping_escalates_and_takes_what_the_child_started_with_it() {
        // Ignores SIGTERM and leaves a grandchild behind in the group.
        let (proc, stdout) = Proc::spawn(script(
            "trap '' TERM; sleep 60 & echo $!; while :; do sleep 1; done",
        ))
        .unwrap();
        let mut reader = FrameReader::new(stdout, 100);
        let Some(Frame::Line(pid)) = reader.next_frame().unwrap() else {
            panic!("no grandchild pid");
        };
        let grandchild: libc::pid_t = String::from_utf8(pid).unwrap().trim().parse().unwrap();
        proc.stop_after(Duration::from_millis(50), Duration::from_millis(100));
        assert!(proc.try_wait().is_some());
        let gone = (0..100).any(|_| {
            thread::sleep(Duration::from_millis(20));
            unsafe { libc::kill(grandchild, 0) != 0 }
        });
        assert!(gone, "the grandchild outlived the group");
    }

    #[test]
    fn a_missing_program_is_reported_by_name() {
        let error = Proc::spawn(Command::new("/nonexistent/riwork-provider"))
            .err()
            .unwrap();
        assert!(error.contains("/nonexistent/riwork-provider"), "{error}");
    }
}
