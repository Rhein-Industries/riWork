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
    child: Mutex<Child>,
    pid: libc::pid_t,
    stdin: Mutex<Option<ChildStdin>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    /// The stderr drain has seen the end of the stream.
    stderr_done: Arc<AtomicBool>,
    /// The process has been reaped and its group swept: its pid may be reused,
    /// so the group is never signalled again.
    swept: AtomicBool,
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
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stderr_tail,
            stderr_done,
            swept: AtomicBool::new(false),
        };
        Ok((Arc::new(proc), stdout))
    }

    pub fn id(&self) -> u32 {
        self.pid as u32
    }

    /// Write one line to the provider. Fails once its input is closed or gone.
    pub fn send_line(&self, line: &str) -> Result<(), String> {
        let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        let input = stdin.as_mut().ok_or("the provider's input is closed")?;
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        input
            .write_all(&bytes)
            .and_then(|()| input.flush())
            .map_err(|error| format!("cannot write to the provider: {error}"))
    }

    pub fn send(&self, value: &serde_json::Value) -> Result<(), String> {
        self.send_line(&value.to_string())
    }

    /// Close the provider's input, which tells it to finish.
    pub fn close_stdin(&self) {
        self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    /// The exit status once the process has ended, or `None` while it runs.
    pub fn try_wait(&self) -> Option<ExitStatus> {
        let status = {
            let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
            child.try_wait().ok().flatten()
        };
        if status.is_some() && !self.swept.swap(true, Ordering::AcqRel) {
            // Whatever the process left running in its group goes with it, now,
            // while the group id still means this process.
            unsafe { libc::kill(-self.pid, libc::SIGKILL) };
        }
        status
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
        if self.try_wait().is_none() {
            unsafe { libc::kill(self.pid, signal) };
        }
    }

    /// Signal the whole process group (not once the process is gone).
    pub fn signal_group(&self, signal: libc::c_int) {
        if !self.swept.load(Ordering::Acquire) {
            // The process leads its group (`process_group(0)`), so the id is the pid.
            unsafe { libc::kill(-self.pid, signal) };
        }
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
        // A writer stuck on a provider that stopped reading holds the lock:
        // then the input stays open and the signals do the work.
        if let Ok(mut stdin) = self.stdin.try_lock() {
            stdin.take();
        }
        if self.wait_exit(grace).is_none() {
            self.signal_group(libc::SIGTERM);
            if self.wait_exit(term_wait).is_none() {
                self.signal_group(libc::SIGKILL);
                let _ = self.child.lock().unwrap_or_else(|e| e.into_inner()).wait();
            }
        }
        // Reaping sweeps the group; make sure it has been.
        let _ = self.try_wait();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if self.try_wait().is_none() {
            unsafe { libc::kill(-self.pid, libc::SIGKILL) };
            let _ = self
                .child
                .get_mut()
                .unwrap_or_else(|e| e.into_inner())
                .wait();
        }
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
    fn an_oversized_line_is_skipped_and_measured_and_reading_goes_on() {
        let mut input = b"before\n".to_vec();
        input.extend(std::iter::repeat_n(b'x', 1000));
        input.extend_from_slice(b"\nafter\n");
        assert_eq!(
            frames(&input, 16),
            vec![line("before"), Frame::Oversized(1000), line("after")]
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

    fn script(body: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(body);
        command
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
    fn an_exit_is_explained_with_the_end_of_stderr() {
        let (proc, _stdout) = Proc::spawn(script("echo boom >&2; exit 7")).unwrap();
        let message = proc.describe_exit(Duration::from_secs(5));
        assert!(message.contains("exit status: 7"), "{message}");
        assert!(message.contains("boom"), "{message}");
    }

    #[test]
    fn a_missing_program_is_reported_by_name() {
        let error = Proc::spawn(Command::new("/nonexistent/riwork-provider"))
            .err()
            .unwrap();
        assert!(error.contains("/nonexistent/riwork-provider"), "{error}");
    }
}
