//! A chat's files on disk, under `RIWORK_HOME/chats/<id>/`:
//!
//! - `info.json` is the chat's `ChatInfo`, replaced atomically on every change.
//! - `events.jsonl` is its log: one `wire::Envelope` per line, append-only, the
//!   line number being the envelope's `seq` (1-based, gapless). A subscriber's
//!   replay is a copy of these lines, so the file format is the wire format.
//!
//! A crash can leave half a line at the end of the log. `ChatLog::open` cuts
//! it off, so the next event continues the numbering, and it checks that the
//! numbering is what the replay relies on: line N is event N.
//!
//! No file stays open between events: a host with hundreds of chats would run
//! out of descriptors (a macOS app's soft limit is 256).

use super::model::ChatInfo;
use super::wire::Envelope;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const INFO: &str = "info.json";
const EVENTS: &str = "events.jsonl";

/// Where all chats live.
pub fn chats_dir(home: &Path) -> PathBuf {
    home.join("chats")
}

/// The directory of chat `id`, which must be a canonical UUID: an id from the
/// wire never becomes a path component otherwise.
pub fn chat_dir(home: &Path, id: &str) -> Option<PathBuf> {
    Uuid::parse_str(id)
        .ok()
        .filter(|uuid| uuid.to_string() == id)
        .map(|_| chats_dir(home).join(id))
}

pub struct ChatLog {
    dir: PathBuf,
}

impl ChatLog {
    /// A new chat: its directory (owner-only), `info.json`, and an empty log.
    pub fn create(dir: &Path, info: &ChatInfo) -> Result<Self, String> {
        crate::paths::create_private_dir(dir)
            .map_err(|error| format!("Cannot create {}: {error}", dir.display()))?;
        open_events(dir)
            .map_err(|error| format!("Cannot create {}: {error}", dir.join(EVENTS).display()))?;
        let log = Self {
            dir: dir.to_owned(),
        };
        log.save_info(info)?;
        Ok(log)
    }

    /// An existing chat: its info, its log ready to append to, and the `seq`
    /// of the last event in it (0 for an empty log).
    pub fn open(dir: &Path) -> Result<(Self, ChatInfo, u64), String> {
        let text = fs::read_to_string(dir.join(INFO))
            .map_err(|error| format!("Cannot read {}: {error}", dir.join(INFO).display()))?;
        let info: ChatInfo = serde_json::from_str(&text)
            .map_err(|error| format!("Cannot parse {}: {error}", dir.join(INFO).display()))?;
        open_events(dir)
            .map_err(|error| format!("Cannot open {}: {error}", dir.join(EVENTS).display()))?;
        let path = dir.join(EVENTS);
        let last = repair_tail(&path)
            .and_then(|last| verify_numbering(&path, last))
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
        Ok((
            Self {
                dir: dir.to_owned(),
            },
            info,
            last,
        ))
    }

    /// Appends one complete line (ending in a newline) with a single write, so
    /// a reader never sees half of it while the host runs. With `sync`, what
    /// was appended so far survives a power loss.
    pub fn append(&self, line: &[u8], sync: bool) -> io::Result<()> {
        debug_assert!(line.ends_with(b"\n"));
        let mut events = open_events(&self.dir)?;
        events.write_all(line)?;
        if sync {
            events.sync_data()?;
        }
        Ok(())
    }

    /// Replaces `info.json`: the old file or the whole new one is there, never
    /// a partial write.
    pub fn save_info(&self, info: &ChatInfo) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(info).map_err(|error| error.to_string())?;
        let path = self.dir.join(INFO);
        let temporary = self.dir.join(format!(".info-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = private_options().create_new(true).open(&temporary)?;
            file.write_all(&data)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Cannot write {}: {error}", path.display()));
        }
        Ok(())
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn open_events(dir: &Path) -> io::Result<File> {
    private_options()
        .create(true)
        .append(true)
        .open(dir.join(EVENTS))
}

/// Writes the log's lines `since + 1 ..= last` to `out`, as stored. The caller
/// knows `last` from the moment it registered for live events, so lines that
/// are written afterwards are not part of the replay.
pub fn replay(dir: &Path, since: u64, last: u64, out: &mut impl Write) -> io::Result<()> {
    let truncated = || {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the event log is shorter than its numbering",
        )
    };
    let mut reader = BufReader::with_capacity(256 * 1024, File::open(dir.join(EVENTS))?);
    for _ in 0..since {
        if reader.skip_until(b'\n')? == 0 {
            return Err(truncated());
        }
    }
    let mut line = Vec::new();
    for _ in since..last {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Err(truncated());
        }
        out.write_all(&line)?;
    }
    out.flush()
}

/// Every event of the log, for a chat that has to be examined whole.
pub fn read_envelopes(dir: &Path) -> io::Result<Vec<Envelope>> {
    let reader = BufReader::new(File::open(dir.join(EVENTS))?);
    let mut envelopes = Vec::new();
    for line in reader.lines() {
        match serde_json::from_str::<Envelope>(&line?) {
            Ok(envelope) if envelope.seq == envelopes.len() as u64 + 1 => envelopes.push(envelope),
            _ => break,
        }
    }
    Ok(envelopes)
}

/// Cuts a half-written last line off the log and returns the `seq` of the last
/// whole one. A last line that does not parse is cut off too.
fn repair_tail(path: &Path) -> io::Result<u64> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    loop {
        let length = file.metadata()?.len();
        if length == 0 {
            return Ok(0);
        }
        let mut last_byte = [0];
        file.seek(SeekFrom::Start(length - 1))?;
        file.read_exact(&mut last_byte)?;
        if last_byte[0] != b'\n' {
            let whole = line_start(&mut file, length)?;
            file.set_len(whole)?;
            continue;
        }
        let start = line_start(&mut file, length - 1)?;
        file.seek(SeekFrom::Start(start))?;
        let mut line = Vec::new();
        BufReader::new(&mut file).read_until(b'\n', &mut line)?;
        match serde_json::from_slice::<Envelope>(&line) {
            Ok(envelope) => return Ok(envelope.seq),
            Err(_) => file.set_len(start)?,
        }
    }
}

/// Checks that line N of the log is event N, as far as `last` (the number of
/// the last line's event) says: the replay finds events by counting lines. A
/// log where that does not hold (edited, or pieced together from another)
/// keeps its longest gapless beginning, and the rest is cut off. Returns the
/// number of events that are left.
fn verify_numbering(path: &Path, last: u64) -> io::Result<u64> {
    let mut reader = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut lines = 0u64;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        lines += buffer.iter().filter(|byte| **byte == b'\n').count() as u64;
        let length = buffer.len();
        reader.consume(length);
    }
    if lines == last {
        return Ok(last);
    }
    let mut reader = BufReader::new(File::open(path)?);
    let (mut kept_lines, mut kept_bytes) = (0u64, 0u64);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        match serde_json::from_slice::<Envelope>(&line) {
            Ok(envelope) if envelope.seq == kept_lines + 1 && line.ends_with(b"\n") => {
                kept_lines += 1;
                kept_bytes += line.len() as u64;
            }
            _ => break,
        }
    }
    OpenOptions::new()
        .write(true)
        .open(path)?
        .set_len(kept_bytes)?;
    eprintln!(
        "riwork chat: {} was numbered irregularly; kept its first {kept_lines} events",
        path.display()
    );
    Ok(kept_lines)
}

/// The offset just after the last newline that lies before `end`; 0 if none.
fn line_start(file: &mut File, end: u64) -> io::Result<u64> {
    const CHUNK: u64 = 64 * 1024;
    let mut position = end;
    while position > 0 {
        let from = position.saturating_sub(CHUNK);
        let mut buffer = vec![0; (position - from) as usize];
        file.seek(SeekFrom::Start(from))?;
        file.read_exact(&mut buffer)?;
        if let Some(index) = buffer.iter().rposition(|byte| *byte == b'\n') {
            return Ok(from + index as u64 + 1);
        }
        position = from;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ApprovalMode, ChatEvent, ChatState, Provider};

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rwlog-{}", &Uuid::new_v4().to_string()[..8]));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn info(dir: &Path) -> ChatInfo {
        ChatInfo {
            id: Uuid::new_v4().to_string(),
            provider: Provider::Codex,
            project_id: None,
            worktree_id: None,
            cwd: dir.to_owned(),
            title: "Codex chat".into(),
            created_at_unix: 1,
            provider_thread_id: None,
            model: None,
            effort: None,
            approval_mode: ApprovalMode::Supervised,
            codex_account_id: None,
            state: ChatState::Starting,
        }
    }

    fn line(seq: u64) -> String {
        let envelope = Envelope {
            chat_id: "c".into(),
            seq,
            event: ChatEvent::State {
                state: ChatState::Idle,
            },
        };
        format!("{}\n", serde_json::to_string(&envelope).unwrap())
    }

    #[test]
    fn a_chat_is_written_and_read_back_with_private_modes() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let dir = root.join("chat");
        let info = info(&root);
        let log = ChatLog::create(&dir, &info).unwrap();
        log.append(line(1).as_bytes(), false).unwrap();
        log.append(line(2).as_bytes(), true).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(INFO)), 0o600);
        assert_eq!(mode(&dir.join(EVENTS)), 0o600);
        drop(log);
        let (_, read, last) = ChatLog::open(&dir).unwrap();
        assert_eq!((read, last), (info, 2));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saving_info_leaves_no_temporary_files_and_replaces_the_old_one() {
        let root = scratch();
        let dir = root.join("chat");
        let mut info = info(&root);
        let log = ChatLog::create(&dir, &info).unwrap();
        info.title = "Renamed".into();
        info.state = ChatState::Idle;
        log.save_info(&info).unwrap();
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, [EVENTS, INFO]);
        assert_eq!(ChatLog::open(&dir).unwrap().1, info);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_half_written_last_line_is_cut_off_and_numbering_continues() {
        let root = scratch();
        let dir = root.join("chat");
        let info = info(&root);
        let log = ChatLog::create(&dir, &info).unwrap();
        for seq in 1..=3 {
            log.append(line(seq).as_bytes(), false).unwrap();
        }
        let whole = fs::read(dir.join(EVENTS)).unwrap();
        // Half of a fourth line, as a crash in the middle of a write leaves it.
        let mut cut = whole.clone();
        cut.extend_from_slice(&line(4).as_bytes()[..20]);
        fs::write(dir.join(EVENTS), &cut).unwrap();
        let (log, _, last) = ChatLog::open(&dir).unwrap();
        assert_eq!(last, 3);
        assert_eq!(fs::read(dir.join(EVENTS)).unwrap(), whole);
        log.append(line(4).as_bytes(), false).unwrap();
        assert_eq!(read_envelopes(&dir).unwrap().len(), 4);
        // A whole line that is not an event is cut off too.
        let mut garbled = fs::read(dir.join(EVENTS)).unwrap();
        garbled.extend_from_slice(b"{\"oops\":true}\n");
        fs::write(dir.join(EVENTS), &garbled).unwrap();
        assert_eq!(ChatLog::open(&dir).unwrap().2, 4);
        // No newline anywhere: nothing to keep.
        fs::write(dir.join(EVENTS), b"{\"chat_id\"").unwrap();
        assert_eq!(ChatLog::open(&dir).unwrap().2, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_log_whose_numbering_is_irregular_keeps_its_gapless_beginning() {
        let root = scratch();
        let dir = root.join("chat");
        let log = ChatLog::create(&dir, &info(&root)).unwrap();
        for seq in [1, 2, 4, 5] {
            log.append(line(seq).as_bytes(), false).unwrap();
        }
        let (log, _, last) = ChatLog::open(&dir).unwrap();
        assert_eq!(last, 2);
        assert_eq!(read_envelopes(&dir).unwrap().len(), 2);
        log.append(line(3).as_bytes(), false).unwrap();
        assert_eq!(ChatLog::open(&dir).unwrap().2, 3);
        // A last event with an absurd number is not taken at its word.
        log.append(line(u64::MAX).as_bytes(), false).unwrap();
        assert_eq!(ChatLog::open(&dir).unwrap().2, 3);
        // Neither is a last line that is not even text.
        let mut bytes = fs::read(dir.join(EVENTS)).unwrap();
        bytes.extend_from_slice(b"\xff\xfe\n");
        fs::write(dir.join(EVENTS), bytes).unwrap();
        assert_eq!(ChatLog::open(&dir).unwrap().2, 3);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn appending_holds_no_file_open() {
        let root = scratch();
        let dir = root.join("chat");
        let log = ChatLog::create(&dir, &info(&root)).unwrap();
        // Removing the directory under a log is seen at the next append.
        fs::remove_dir_all(&dir).unwrap();
        assert!(log.append(line(1).as_bytes(), false).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_replay_copies_the_requested_lines_as_stored() {
        let root = scratch();
        let dir = root.join("chat");
        let log = ChatLog::create(&dir, &info(&root)).unwrap();
        for seq in 1..=5 {
            log.append(line(seq).as_bytes(), false).unwrap();
        }
        let mut out = Vec::new();
        replay(&dir, 2, 4, &mut out).unwrap();
        assert_eq!(out, [line(3), line(4)].concat().as_bytes());
        let mut nothing = Vec::new();
        replay(&dir, 5, 5, &mut nothing).unwrap();
        assert!(nothing.is_empty());
        assert!(replay(&dir, 0, 6, &mut Vec::new()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_canonical_uuids_name_a_chat_directory() {
        let home = Path::new("/home");
        let id = Uuid::new_v4().to_string();
        assert_eq!(chat_dir(home, &id), Some(home.join("chats").join(&id)));
        for bad in ["", "..", "../x", &id.to_uppercase(), &id[..8], "a/b"] {
            assert_eq!(chat_dir(home, bad), None, "{bad:?}");
        }
    }
}
