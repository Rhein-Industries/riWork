//! Waking a waiting `shell output` when its pane may have changed, instead of
//! capturing the pane every 80 ms to find out.
//!
//! A tmux control-mode client attached to the shell's own session
//! (`tmux -C attach-session -f read-only,ignore-size`) gets a line for every
//! write the pane's program makes (`%output`), and for layout and mode changes.
//! It is only an alarm: the waiting call still captures and hashes the pane
//! itself, so what it answers is what it always answered. The client never
//! sends a command, never sizes a window (`ignore-size`) and cannot change
//! anything (`read-only`).
//!
//! A quiet pane then costs nothing: no process starts until something is
//! written. A change in a pane that tmux announces nothing about (such as
//! `clear-history`) is still found by a slow safety capture, and a client that
//! cannot attach (an old tmux, a session that just ended) leaves the caller on
//! the 80 ms loop it always had.
use std::{
    io::{self, BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    thread,
    time::{Duration, Instant},
};

/// How long a change keeps being collected before the pane is captured, so that
/// a screen redrawn by several writes is read once, whole.
pub(super) const SETTLE: Duration = Duration::from_millis(5);
/// The least spacing of captures after a wake-up found nothing new (a program
/// that repaints an unchanged screen); it doubles up to the polling interval,
/// so the worst case is the old 80 ms loop and never more.
pub(super) const FIRST_GAP: Duration = Duration::from_millis(10);
/// The longest silence between two captures while watching. Changes tmux does
/// not announce are found this late.
pub(super) const SAFETY_POLL: Duration = Duration::from_secs(4);
/// The shortest wait that is worth starting a client for.
pub(super) const MIN_WATCHED_WAIT: Duration = Duration::from_millis(250);
/// How long a client may take to attach before the caller polls instead.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(1);
/// Bytes of each line that are looked at; the rest of a long `%output` is
/// read and dropped.
const LINE_HEAD: usize = 32;

/// What one line of a control client means to a waiting caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Notice {
    /// Attached: from here on nothing the pane does can be missed.
    Attached,
    /// The pane, its size or its mode may have changed.
    Change,
    /// The client is leaving (the session ended).
    Closed,
    /// Replies to commands (there are none) and news about other sessions.
    Ignore,
}

/// The meaning of a control-mode line, from its first bytes.
pub(super) fn notice(head: &[u8]) -> Notice {
    let word = head
        .iter()
        .position(|byte| matches!(byte, b' ' | b'\n' | b'\r'))
        .map_or(head, |end| &head[..end]);
    match word {
        b"%session-changed" => Notice::Attached,
        b"%exit" => Notice::Closed,
        // Replies to commands (none are sent) and news that says nothing about
        // what the pane shows. A window is renamed after its foreground
        // program every time that changes, which a shell script that sleeps in
        // a loop does many times a second.
        b"%begin"
        | b"%end"
        | b"%error"
        | b"%window-renamed"
        | b"%unlinked-window-renamed"
        | b"%sessions-changed"
        | b"%session-renamed"
        | b"%session-window-changed"
        | b"%client-session-changed"
        | b"%client-detached"
        | b"%paste-buffer-changed"
        | b"%paste-buffer-deleted"
        | b"%subscription-changed" => Notice::Ignore,
        // `%output`, `%extended-output`, `%layout-change`, `%pane-mode-changed`,
        // `%window-*`, and whatever a newer tmux adds: when in doubt, look.
        _ => Notice::Change,
    }
}

/// The head of the next line (at most `LINE_HEAD` bytes, newline included if
/// it fits), and whether there was one. The rest of the line is consumed.
fn next_line_head(reader: &mut impl BufRead, head: &mut Vec<u8>) -> io::Result<bool> {
    head.clear();
    let mut any = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(any);
        }
        any = true;
        let (used, end) = match available.iter().position(|byte| *byte == b'\n') {
            Some(newline) => (newline + 1, true),
            None => (available.len(), false),
        };
        let room = LINE_HEAD.saturating_sub(head.len());
        head.extend_from_slice(&available[..used.min(room)]);
        reader.consume(used);
        if end {
            return Ok(true);
        }
    }
}

/// Why a wait ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Woke {
    /// The pane may have changed.
    Change,
    /// Nothing happened for as long as was asked.
    Silence,
    /// The client is gone; only polling is left.
    Lost,
}

/// A control-mode client watching one session.
pub(super) struct PaneWatch {
    child: Child,
    // Held so the client sees its input stay open: when this process dies, in
    // any way, the pipe closes and the client leaves with it.
    _input: std::process::ChildStdin,
    notices: Receiver<Notice>,
    /// The last wait ended on a change, so the capture after it has not been
    /// judged yet.
    woken: bool,
    /// Least spacing of captures; grows while wake-ups keep finding nothing.
    gap: Duration,
}

impl PaneWatch {
    /// Attach `command` (a tmux client set up for the right server) to
    /// `session` and wait for it to be attached. `None` when it cannot be.
    pub(super) fn start(mut command: Command, session: &str) -> Option<Self> {
        let mut child = command
            .args(["-C", "attach-session", "-t"])
            .arg(format!("={session}"))
            .args(["-f", "read-only,ignore-size"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        let (sender, notices) = mpsc::sync_channel(8);
        thread::spawn(move || read_notices(output, &sender));
        let watch = Self {
            child,
            _input: input,
            notices,
            woken: false,
            gap: Duration::ZERO,
        };
        let deadline = Instant::now() + ATTACH_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match watch.notices.recv_timeout(left) {
                Ok(Notice::Attached) => return Some(watch),
                // Anything else before that is not an attached client.
                Ok(Notice::Closed) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    /// Wait up to `longest` for the pane to change, then, if it did, let the
    /// burst settle and keep to the spacing between captures. Never longer
    /// than `longest` in all.
    ///
    /// Called right after a capture. Calling it again after a change means that
    /// capture found nothing new, which widens the spacing.
    pub(super) fn wait(&mut self, longest: Duration) -> Woke {
        let entered = Instant::now();
        let deadline = entered + longest;
        if std::mem::take(&mut self.woken) {
            self.gap = (self.gap.max(FIRST_GAP) * 2).min(super::OUTPUT_POLL);
        }
        match self.notices.recv_timeout(longest) {
            Ok(Notice::Change) | Ok(Notice::Attached) => {}
            Ok(Notice::Ignore) => return Woke::Silence,
            Ok(Notice::Closed) | Err(RecvTimeoutError::Disconnected) => return Woke::Lost,
            Err(RecvTimeoutError::Timeout) => return Woke::Silence,
        }
        self.woken = true;
        let ready = (Instant::now() + SETTLE).max(entered + self.gap);
        let until = ready.min(deadline);
        thread::sleep(until.saturating_duration_since(Instant::now()));
        // What came in meanwhile is covered by the capture about to be made.
        while let Ok(notice) = self.notices.try_recv() {
            if notice == Notice::Closed {
                return Woke::Lost;
            }
        }
        Woke::Change
    }
}

impl Drop for PaneWatch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_notices(output: impl io::Read, sender: &SyncSender<Notice>) {
    let mut reader = BufReader::with_capacity(64 * 1024, output);
    let mut head = Vec::with_capacity(LINE_HEAD);
    while matches!(next_line_head(&mut reader, &mut head), Ok(true)) {
        match notice(&head) {
            Notice::Ignore => {}
            // A full channel already holds news for the waiter.
            other => {
                let _ = sender.try_send(other);
            }
        }
    }
    // The waiter sees the channel close once the queue has been read.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_lines_mean_what_a_waiting_caller_needs() {
        for (line, meaning) in [
            ("%output %0 hello\\015\\012\n", Notice::Change),
            ("%extended-output %0 12 : x\n", Notice::Change),
            ("%layout-change @0 b25d,80x24,0,0,0\n", Notice::Change),
            ("%pane-mode-changed %0\n", Notice::Change),
            ("%window-pane-changed @0 %0\n", Notice::Change),
            ("%something-from-a-newer-tmux 1 2\n", Notice::Change),
            ("%session-changed $0 name\n", Notice::Attached),
            ("%exit\n", Notice::Closed),
            ("%exit server exited\n", Notice::Closed),
            ("%begin 1790859582 573 0\n", Notice::Ignore),
            ("%end 1790859582 573 0\n", Notice::Ignore),
            ("%error 1790859582 573 0\n", Notice::Ignore),
            ("%sessions-changed\n", Notice::Ignore),
            ("%client-session-changed client-1 $2 name\n", Notice::Ignore),
            ("%session-renamed $0 other\n", Notice::Ignore),
            ("%window-renamed @0 sleep\n", Notice::Ignore),
            ("%unlinked-window-renamed @1 x\n", Notice::Ignore),
            ("%session-window-changed $0 @0\n", Notice::Ignore),
            // Text of any other kind is not a reason to sleep through it.
            ("anything else\n", Notice::Change),
            ("%outputs-look-alike\n", Notice::Change),
        ] {
            assert_eq!(notice(line.as_bytes()), meaning, "{line:?}");
        }
    }

    #[test]
    fn only_the_head_of_a_long_line_is_kept_and_the_rest_is_consumed() {
        let long = format!("%output %0 {}\nshort\n", "x".repeat(100_000));
        let mut reader = BufReader::with_capacity(16, long.as_bytes());
        let mut head = Vec::new();
        assert!(next_line_head(&mut reader, &mut head).unwrap());
        assert_eq!(head.len(), LINE_HEAD);
        assert!(head.starts_with(b"%output %0 xxxx"));
        assert!(next_line_head(&mut reader, &mut head).unwrap());
        assert_eq!(head, b"short\n");
        assert!(!next_line_head(&mut reader, &mut head).unwrap());
        // A final line without a newline is still a line.
        let mut reader = BufReader::new(&b"%exit"[..]);
        assert!(next_line_head(&mut reader, &mut head).unwrap());
        assert_eq!(notice(&head), Notice::Closed);
    }

    /// A watch fed by hand, with no tmux behind it.
    fn watch(sender: &mut Option<SyncSender<Notice>>) -> PaneWatch {
        let (tx, notices) = mpsc::sync_channel(8);
        *sender = Some(tx);
        let mut child = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        PaneWatch {
            _input: child.stdin.take().unwrap(),
            child,
            notices,
            woken: false,
            gap: Duration::ZERO,
        }
    }

    #[test]
    fn a_wait_ends_at_a_change_settles_and_never_overruns_its_time() {
        let mut sender = None;
        let mut watch = watch(&mut sender);
        let tx = sender.take().unwrap();
        // Silence lasts exactly as long as asked.
        let started = Instant::now();
        assert_eq!(watch.wait(Duration::from_millis(60)), Woke::Silence);
        assert!(started.elapsed() >= Duration::from_millis(60));
        assert!(started.elapsed() < Duration::from_millis(500));
        // A change that is already waiting is answered after the settling time.
        tx.send(Notice::Change).unwrap();
        let started = Instant::now();
        assert_eq!(watch.wait(Duration::from_secs(10)), Woke::Change);
        assert!(started.elapsed() >= SETTLE);
        assert!(started.elapsed() < Duration::from_secs(1));
        // Nothing found by that capture: the next wake-up keeps a longer gap.
        tx.send(Notice::Change).unwrap();
        let started = Instant::now();
        assert_eq!(watch.wait(Duration::from_secs(10)), Woke::Change);
        assert!(
            started.elapsed() >= FIRST_GAP * 2,
            "{:?}",
            started.elapsed()
        );
        // The gap never passes the polling interval, whatever the streak.
        for _ in 0..8 {
            tx.send(Notice::Change).unwrap();
            assert_eq!(watch.wait(Duration::from_secs(10)), Woke::Change);
        }
        assert_eq!(watch.gap, crate::sessions::OUTPUT_POLL);
        // The time asked for caps the settling and the gap.
        tx.send(Notice::Change).unwrap();
        let started = Instant::now();
        assert_eq!(watch.wait(Duration::from_millis(20)), Woke::Change);
        assert!(started.elapsed() < Duration::from_millis(500));
        // A burst is collected into one wake-up.
        for _ in 0..5 {
            tx.send(Notice::Change).unwrap();
        }
        assert_eq!(watch.wait(Duration::from_secs(10)), Woke::Change);
        assert!(watch.notices.try_recv().is_err());
        // Ending the client, in the queue or by closing it, is lost.
        tx.send(Notice::Closed).unwrap();
        assert_eq!(watch.wait(Duration::from_secs(10)), Woke::Lost);
        drop(tx);
        assert_eq!(watch.wait(Duration::from_secs(10)), Woke::Lost);
    }
}
