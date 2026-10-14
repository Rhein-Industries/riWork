//! What a chat's message box suggests while a `/` command or an `@` file is typed, and what
//! each suggestion shows and puts in before anything is sent. One catalog for every composer:
//! the desktop reads it here, and the phone gets the same entries from `riwork chat
//! suggestions --json` (the connector's `chat.suggestions`) and applies the same rules, which
//! `ios/Tests/Fixtures/composer-suggestions.json` pins for both.
//!
//! - **When the list opens** (`trigger`): `/` as the first thing in the draft (leading blank
//!   space allowed), because a provider reads a slash command only at the start of a message;
//!   `@` at the start of the draft or after blank space. The token runs from the sigil to the
//!   next blank space, the query from after the sigil to the caret. A sigil inside a word
//!   (`a/b`, `me@host`) opens nothing, nor does a token that is not a whole command name
//!   (`/usr/bin`).
//! - **Slash commands** (`COMMANDS`): only what RiWork actually does with the text. RiWork has
//!   no slash commands of its own today: a typed `/…` goes to the provider as written. The
//!   entries are the provider commands that verifiably work through RiWork's drivers.
//! - **`@` files** (`list_files`): the chat folder's files and folders, from one bounded walk
//!   that honours `.gitignore` (`FILE_BOUND` entries, `SCAN_BOUND` directory entries looked at,
//!   `WALK_BUDGET`), which a composer caches (`FileCache`) and only filters while typing.
//!   Accepting one puts in its path relative to the chat's folder, which every provider's
//!   agent can open from where it runs; nothing is attached. Chats and shells are not offered:
//!   no provider and no RiWork pipeline resolves a reference to one from a message's text.
//! - **Accepting** (`accept`) replaces the token with the entry's text and a space. It never
//!   sends: that stays the person's own ⏎ or Send.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use super::model::Provider;

/// The most files and folders one listing holds (the file explorer's cap for one folder).
pub const FILE_BOUND: usize = 5_000;
/// The most directory entries one listing looks at, ignored ones included.
pub const SCAN_BOUND: usize = 50_000;
/// How long one listing may walk before it stops with what it has.
pub const WALK_BUDGET: Duration = Duration::from_secs(2);
/// How deep under the chat's folder a listing goes.
pub const MAX_DEPTH: usize = 24;
/// The most suggestions the list shows at once.
pub const SHOWN: usize = 50;
/// How long a composer keeps a folder's listing before it walks the folder again.
pub const FILE_TTL: Duration = Duration::from_secs(30);
/// What a file's preview reads of it at most, and how many of its lines it shows.
pub const PEEK_BYTES: usize = 4 * 1024;
pub const PEEK_LINES: usize = 6;
/// A previewed line longer than this is cut.
const PEEK_LINE_CHARS: usize = 160;

// ---- When the list opens -------------------------------------------------------------------

/// What was typed to open the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sigil {
    /// `/`: a command.
    Slash,
    /// `@`: a file or folder of the chat's folder.
    Mention,
}

impl Sigil {
    pub fn char(self) -> char {
        match self {
            Self::Slash => '/',
            Self::Mention => '@',
        }
    }
}

/// The token the caret is in that opens the list, in byte offsets of the draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub sigil: Sigil,
    /// Where the sigil is.
    pub start: usize,
    /// Where the token ends: the next blank space after the caret, or the draft's end. Accepting
    /// replaces `start..end`.
    pub end: usize,
    /// What is typed between the sigil and the caret.
    pub query: String,
}

/// The list that `text` with the caret at byte `caret` opens, if any (see the module's
/// rules). A caret off a character boundary opens nothing.
pub fn trigger(text: &str, caret: usize) -> Option<Trigger> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let start = text[..caret]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(at, c)| at + c.len_utf8());
    let end = text[caret..]
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map_or(text.len(), |(at, _)| caret + at);
    let sigil = match text[start..].chars().next()? {
        '/' if text[..start].trim().is_empty() => Sigil::Slash,
        '@' => Sigil::Mention,
        _ => return None,
    };
    if start == caret {
        // The caret is before the sigil: the token is not being typed.
        return None;
    }
    let query = text[start + 1..caret].to_owned();
    // A command name is one word: a path typed at the start of a message is just text.
    if sigil == Sigil::Slash && text[start + 1..end].contains('/') {
        return None;
    }
    Some(Trigger {
        sigil,
        start,
        end,
        query,
    })
}

/// `text` with the trigger's token replaced by `insert`, and the caret after it. A space
/// follows unless one already does, so the next word starts apart.
pub fn accept(text: &str, trigger: &Trigger, insert: &str) -> (String, usize) {
    let rest = &text[trigger.end..];
    let mut out = String::with_capacity(text.len() + insert.len() + 1);
    out.push_str(&text[..trigger.start]);
    out.push_str(insert);
    let caret = if rest.starts_with(' ') {
        out.len() + 1
    } else {
        out.push(' ');
        out.len()
    };
    out.push_str(rest);
    (out, caret)
}

// ---- Slash commands ------------------------------------------------------------------------

/// Who acts on a command once the message is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Handler {
    /// The text goes to the provider as typed and the provider runs the command.
    Provider,
    /// RiWork acts on it before anything reaches the provider.
    Riwork,
}

/// A slash command as a composer shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// Without the slash.
    pub name: String,
    /// What it is, in one line.
    pub summary: String,
    /// What may follow the name, as the list shows it; none takes nothing.
    pub argument: Option<String>,
    /// What sending a message that starts with it does.
    pub sends: String,
    pub handler: Handler,
}

impl Command {
    /// What accepting it puts in the draft.
    pub fn insertion(&self) -> String {
        format!("/{}", self.name)
    }
}

/// One entry of `COMMANDS`.
pub struct Registered {
    pub name: &'static str,
    pub summary: &'static str,
    pub argument: Option<&'static str>,
    pub sends: &'static str,
    pub handler: Handler,
    /// The providers whose chats offer it.
    pub providers: &'static [Provider],
}

/// Every slash command a composer offers, in the order the list shows them. **This is where a
/// command is registered**: a RiWork command (`/memo`, say) is a `Handler::Riwork` entry here
/// for every provider whose chats it serves, and the code that acts on it before the message
/// reaches the provider lives with the send path. Desktop and phone pick it up from here.
pub const COMMANDS: &[Registered] = &[Registered {
    name: "compact",
    summary: "Summarize the conversation so far to free up context",
    // Claude takes focus instructions after it, but RiWork never sends any: none is claimed.
    argument: None,
    sends: "Goes to Claude as typed. Claude compacts the chat's context, as the Compact \
            button does, and the transcript marks where.",
    handler: Handler::Provider,
    // Codex and Muse read a typed `/compact` as words for the model: their compaction is
    // the Compact button's own request.
    providers: &[Provider::Claude],
}];

/// The commands a chat of `provider` offers, in `COMMANDS` order.
pub fn commands(provider: Provider) -> Vec<Command> {
    COMMANDS
        .iter()
        .filter(|command| command.providers.contains(&provider))
        .map(|command| Command {
            name: command.name.to_owned(),
            summary: command.summary.to_owned(),
            argument: command.argument.map(str::to_owned),
            sends: command.sends.to_owned(),
            handler: command.handler,
        })
        .collect()
}

/// The commands that match `query`, best first: a name that starts with it, then one that
/// holds it, then one that holds its letters in order; ties in `commands` order. Case is
/// ignored.
pub fn rank_commands<'a>(query: &str, commands: &'a [Command]) -> Vec<&'a Command> {
    let query = query.to_lowercase();
    let mut ranked: Vec<_> = commands
        .iter()
        .filter_map(|command| {
            let name = command.name.to_lowercase();
            let tier = if name.starts_with(&query) {
                0
            } else if name.contains(&query) {
                1
            } else if subsequence(&query, &name) {
                2
            } else {
                return None;
            };
            Some((tier, command))
        })
        .collect();
    ranked.sort_by_key(|(tier, _)| *tier);
    ranked.into_iter().map(|(_, command)| command).collect()
}

// ---- Files ---------------------------------------------------------------------------------

/// A file or folder of the chat's folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative to the chat's folder, `/` between its parts, no trailing `/`.
    pub path: String,
    #[serde(default)]
    pub dir: bool,
    /// A file's size; none for a folder.
    #[serde(default)]
    pub bytes: Option<u64>,
}

impl FileEntry {
    /// What accepting it puts in the draft: its path, a folder's with a trailing `/`, in
    /// backticks when it has blank space so it reads as one reference.
    pub fn insertion(&self) -> String {
        let path = if self.dir {
            format!("{}/", self.path)
        } else {
            self.path.clone()
        };
        if path.chars().any(char::is_whitespace) {
            format!("`{path}`")
        } else {
            path
        }
    }

    /// The last part of its path.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// One walk of a chat's folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    /// Breadth first, each folder's entries by name.
    pub entries: Vec<FileEntry>,
    /// The walk stopped at a bound: there is more than `entries`.
    #[serde(default)]
    pub truncated: bool,
}

/// How far `list_files_within` may go.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub files: usize,
    pub scanned: usize,
    pub budget: Duration,
    pub depth: usize,
}

impl Bounds {
    pub const DEFAULT: Self = Self {
        files: FILE_BOUND,
        scanned: SCAN_BOUND,
        budget: WALK_BUDGET,
        depth: MAX_DEPTH,
    };
}

/// The files and folders under `root` within the default bounds.
pub fn list_files(root: &Path) -> Listing {
    list_files_within(root, Bounds::DEFAULT)
}

/// The files and folders under `root`, breadth first so the folder's own files come before
/// deep ones, without what `.gitignore` files (and `.git/info/exclude`) leave out, `.git`
/// itself, or symbolic links (a link can lead out of the folder). A folder that cannot be
/// read is skipped.
pub fn list_files_within(root: &Path, bounds: Bounds) -> Listing {
    use ignore::gitignore::{Gitignore, GitignoreBuilder};
    let began = Instant::now();
    let mut listing = Listing::default();
    let mut scanned = 0usize;
    // Each folder still to read, its path relative to the root, and the ignore rules of it and
    // its parents, the nearest last.
    let mut queue = std::collections::VecDeque::from([(
        root.to_path_buf(),
        String::new(),
        Vec::<Arc<Gitignore>>::new(),
    )]);
    while let Some((dir, relative, mut rules)) = queue.pop_front() {
        let mut builder = GitignoreBuilder::new(&dir);
        let mut has_rules = false;
        for file in [dir.join(".gitignore"), dir.join(".git/info/exclude")] {
            if file.is_file() {
                // A line that is not a pattern is skipped; the others still count.
                let _ = builder.add(&file);
                has_rules = true;
            }
        }
        if has_rules && let Ok(matcher) = builder.build() {
            rules.push(Arc::new(matcher));
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<_> = read.filter_map(Result::ok).collect();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            scanned += 1;
            if scanned > bounds.scanned || began.elapsed() > bounds.budget {
                listing.truncated = true;
                return listing;
            }
            let Some(name) = child.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name == ".git" || name == ".DS_Store" {
                continue;
            }
            let Ok(kind) = child.file_type() else {
                continue;
            };
            if kind.is_symlink() || !(kind.is_dir() || kind.is_file()) {
                continue;
            }
            let path = child.path();
            let ignored = rules
                .iter()
                .rev()
                .map(|matcher| matcher.matched(&path, kind.is_dir()))
                .find(|matched| !matched.is_none())
                .is_some_and(|matched| matched.is_ignore());
            if ignored {
                continue;
            }
            if listing.entries.len() == bounds.files {
                listing.truncated = true;
                return listing;
            }
            let child_relative = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            if kind.is_dir() {
                let depth = child_relative.matches('/').count() + 1;
                if depth < bounds.depth {
                    queue.push_back((path, child_relative.clone(), rules.clone()));
                } else {
                    listing.truncated = true;
                }
            }
            listing.entries.push(FileEntry {
                path: child_relative,
                dir: kind.is_dir(),
                bytes: kind
                    .is_file()
                    .then(|| child.metadata().map(|m| m.len()).ok())
                    .flatten(),
            });
        }
    }
    listing
}

/// The entries that match `query`, best first and at most `limit`: a name that starts with
/// it, then a path that does or has a part that does, then a path that holds it, then one
/// that holds its letters in order. Ties go to the shallower path, then the shorter, then the
/// first by character. Case is ignored. An empty query keeps every entry, the shallowest
/// first.
pub fn rank_files<'a>(query: &str, entries: &'a [FileEntry], limit: usize) -> Vec<&'a FileEntry> {
    let query = query.to_lowercase();
    let mut ranked: Vec<_> = entries
        .iter()
        .filter_map(|entry| {
            let path = entry.path.to_lowercase();
            let name = path.rsplit('/').next().unwrap_or(&path);
            let tier = if query.is_empty() || name.starts_with(&query) {
                0
            } else if path.starts_with(&query) || path.contains(&format!("/{query}")) {
                1
            } else if path.contains(&query) {
                2
            } else if subsequence(&query, &path) {
                3
            } else {
                return None;
            };
            let depth = entry.path.matches('/').count();
            Some(((tier, depth, entry.path.chars().count()), entry))
        })
        .collect();
    ranked.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.path.cmp(&y.path)));
    ranked.truncate(limit);
    ranked.into_iter().map(|(_, entry)| entry).collect()
}

/// Whether `needle`'s characters appear in `haystack` in order.
fn subsequence(needle: &str, haystack: &str) -> bool {
    let mut letters = haystack.chars();
    needle.chars().all(|c| letters.any(|h| h == c))
}

/// Listings by folder, each kept `FILE_TTL` from when it was read: a composer filters what it
/// has and walks a folder again only once its listing is stale.
#[derive(Default)]
pub struct FileCache {
    listings: HashMap<PathBuf, (Instant, Arc<Listing>)>,
}

impl FileCache {
    /// The listing of `root`, if one was read less than `FILE_TTL` before `now`.
    pub fn fresh(&self, root: &Path, now: Instant) -> Option<Arc<Listing>> {
        self.listings
            .get(root)
            .filter(|(read, _)| now.saturating_duration_since(*read) < FILE_TTL)
            .map(|(_, listing)| listing.clone())
    }

    /// The listing of `root`, however old.
    pub fn any(&self, root: &Path) -> Option<Arc<Listing>> {
        self.listings.get(root).map(|(_, listing)| listing.clone())
    }

    pub fn put(&mut self, root: PathBuf, read: Instant, listing: Arc<Listing>) {
        self.listings.insert(root, (read, listing));
    }
}

// ---- Previews ------------------------------------------------------------------------------

/// What the card beside the list says about the highlighted entry before it is accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    /// The entry as it reads: `/compact`, `src/main.rs`.
    pub title: String,
    /// What kind of thing it is, with its size for a file: "Claude command", "File · 12 KB".
    pub kind: String,
    /// What it is or what it holds: a command's summary and argument, a file's first lines.
    pub body: Vec<String>,
    /// What accepting puts in the draft.
    pub inserts: String,
    /// What happens to it: what sending does for a command, that a path is only text.
    pub effect: String,
}

/// The preview of a command offered to a chat of `provider`.
pub fn command_preview(command: &Command, provider: Provider) -> Preview {
    let kind = match command.handler {
        Handler::Provider => format!("{} command", provider_title(provider)),
        Handler::Riwork => "RiWork command".to_owned(),
    };
    let mut body = vec![command.summary.clone()];
    body.push(match &command.argument {
        Some(argument) => format!("Takes {argument}"),
        None => "Takes no argument".to_owned(),
    });
    Preview {
        title: command.insertion(),
        kind,
        body,
        inserts: command.insertion(),
        effect: format!("Sending: {}", command.sends),
    }
}

/// The preview of a file or folder, with `peek`'s first lines when they were read.
pub fn file_preview(entry: &FileEntry, peek: Option<&Peek>) -> Preview {
    let kind = if entry.dir {
        "Folder".to_owned()
    } else {
        match entry.bytes {
            Some(bytes) => format!("File · {}", size_label(bytes)),
            None => "File".to_owned(),
        }
    };
    let body = match peek {
        Some(Peek::Text(lines)) if lines.is_empty() => vec!["(empty)".to_owned()],
        Some(Peek::Text(lines)) => lines.clone(),
        Some(Peek::Binary) => vec!["Not text".to_owned()],
        Some(Peek::Unreadable) => vec!["Cannot be read".to_owned()],
        None => Vec::new(),
    };
    Preview {
        title: entry.insertion(),
        kind,
        body,
        inserts: entry.insertion(),
        effect: "Puts in its path, relative to the chat's folder; nothing is attached or sent."
            .to_owned(),
    }
}

/// A size as a preview says it: bytes below 1 KB, whole KB below 1 MB, then MB to a tenth.
pub fn size_label(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes == 1 {
        "1 byte".to_owned()
    } else if bytes < KB {
        format!("{bytes} bytes")
    } else if bytes < MB {
        format!("{} KB", ((bytes + KB / 2) / KB).min(1023))
    } else {
        // (bytes * 10 + MB / 2) / MB, without the product, which the largest sizes overflow.
        let tenths = bytes / MB * 10 + ((bytes % MB) * 10 + MB / 2) / MB;
        format!("{}.{} MB", tenths / 10, tenths % 10)
    }
}

fn provider_title(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
        Provider::Muse => "Muse",
    }
}

/// What the start of a file holds, for its preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Peek {
    /// Its first `PEEK_LINES` lines (from at most `PEEK_BYTES`), tabs as spaces, long ones cut.
    Text(Vec<String>),
    Binary,
    Unreadable,
}

/// The start of the file at `path` under `root`. Reads `PEEK_BYTES` at most, and nothing
/// outside `root` or through a symbolic link.
pub fn peek(root: &Path, path: &str) -> Peek {
    let relative = Path::new(path);
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Peek::Unreadable;
    }
    let full = root.join(relative);
    let inside = match (full.canonicalize(), root.canonicalize()) {
        (Ok(full), Ok(root)) => full.starts_with(root),
        _ => false,
    };
    let regular = std::fs::symlink_metadata(&full).is_ok_and(|meta| meta.file_type().is_file());
    if !inside || !regular {
        return Peek::Unreadable;
    }
    let mut bytes = Vec::with_capacity(PEEK_BYTES);
    let read = std::fs::File::open(&full)
        .and_then(|file| file.take(PEEK_BYTES as u64).read_to_end(&mut bytes));
    if read.is_err() {
        return Peek::Unreadable;
    }
    Peek::from_bytes(&bytes)
}

impl Peek {
    /// What `bytes`, the start of a file, show.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.contains(&0) {
            return Self::Binary;
        }
        let text = match std::str::from_utf8(bytes) {
            Ok(text) => text,
            // A character cut by the read's end is fine; anything else is not text.
            Err(error) if error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or_default()
            }
            Err(_) => return Self::Binary,
        };
        Self::Text(
            text.lines()
                .take(PEEK_LINES)
                .map(|line| {
                    let line = line.replace('\t', "    ");
                    if line.chars().count() > PEEK_LINE_CHARS {
                        let cut: String = line.chars().take(PEEK_LINE_CHARS - 1).collect();
                        format!("{cut}…")
                    } else {
                        line
                    }
                })
                .collect(),
        )
    }
}

// ---- The phone's reply ---------------------------------------------------------------------

/// What `riwork chat suggestions CHAT --json` prints, and the connector's `chat.suggestions`
/// passes to the phone: the chat's commands and its folder's listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub chat_id: String,
    pub provider: Provider,
    /// The chat's folder, which the listing's paths are relative to.
    pub root: String,
    pub commands: Vec<Command>,
    pub files: Listing,
    /// The most entries a listing holds (`FILE_BOUND`), so the phone can say so.
    pub file_bound: usize,
}

/// The reply for chat `chat_id` of `provider` working in `root`. Walks `root` once.
pub fn reply(chat_id: &str, provider: Provider, root: &Path) -> Reply {
    Reply {
        chat_id: chat_id.to_owned(),
        provider,
        root: root.display().to_string(),
        commands: commands(provider),
        files: list_files(root),
        file_bound: FILE_BOUND,
    }
}

#[cfg(test)]
#[path = "suggest_tests.rs"]
mod tests;
