//! Sessions of a deleted worktree. Closing a tab only detaches its shell, so a
//! worktree removed outside RiWork (by `git worktree remove`, or the tool that made
//! it) would leave its shells and agents running for good, holding memory, with no
//! folder left to work in. `reap_orphaned_worktree_sessions` ends them as `close`
//! does, row and all, once:
//!
//! - the worktree's folder has stayed gone for `GRACE`, while the folder it was in is
//!   still there, so a drive that is not mounted or a parent being moved never reads as
//!   a deletion;
//! - no terminal is attached to the session (no visible tab in any window, no phone),
//!   and no window of this process has it in a tab, in front or behind.
//!
//! A tab only saved in the layout of a project no window shows does not keep it: a
//! layout restores the tabs of live shells only. A primary worktree (the project's own
//! folder) and a session without a worktree are never touched, and exited rows of
//! other sessions stay for attribution (see `prune_exited_editors`).

use super::{SessionManager, ShellSession, no_tmux_server, tmux_error};
use crate::store::{Store, Worktree};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

/// How long a worktree folder must stay gone before its sessions are ended.
const GRACE: Duration = Duration::from_secs(5 * 60);
/// How often one process looks, however many windows ask.
const EVERY: Duration = Duration::from_secs(60);
/// How long a window's report that it shows a session in a tab holds. Windows report
/// on every session refresh, about once a second and a half.
const SHOWN_FOR: Duration = Duration::from_secs(2 * 60);

/// What one process remembers between looks: when it last looked, when each
/// candidate was first seen without its folder, and when a window last showed a
/// session in a tab.
struct Watch {
    last: Option<Instant>,
    missing: BTreeMap<String, Instant>,
    shown: BTreeMap<String, Instant>,
}

static WATCH: Mutex<Watch> = Mutex::new(Watch {
    last: None,
    missing: BTreeMap::new(),
    shown: BTreeMap::new(),
});

impl SessionManager {
    /// End the sessions of deleted worktrees (see the module), and return their ids.
    /// `shown` are the shells the calling window has in its tabs. Cheap to call on
    /// every refresh: it looks at most once every `EVERY` per process, and a look
    /// that cannot read tmux, the registry or the store ends nothing.
    pub fn reap_orphaned_worktree_sessions(&self, shown: &HashSet<String>) -> Vec<String> {
        let now = Instant::now();
        // Another window's look holds it while it ends sessions; a refresh never waits.
        let Ok(mut watch) = WATCH.try_lock() else {
            return Vec::new();
        };
        for id in shown {
            watch.shown.insert(id.clone(), now);
        }
        watch
            .shown
            .retain(|_, at| now.duration_since(*at) < SHOWN_FOR);
        if watch
            .last
            .is_some_and(|last| now.duration_since(last) < EVERY)
        {
            return Vec::new();
        }
        watch.last = Some(now);
        let Ok(registry) = self.read_registry() else {
            return Vec::new();
        };
        if !registry
            .sessions
            .iter()
            .any(|session| session.worktree_id.is_some())
        {
            watch.missing.clear();
            return Vec::new();
        }
        let Ok(state) = Store::open(self.home.clone()).and_then(|store| store.snapshot()) else {
            return Vec::new();
        };
        let Ok(attached) = self.attached_session_names() else {
            return Vec::new();
        };
        let mut keep: HashSet<String> = watch.shown.keys().cloned().collect();
        keep.extend(attached);
        let due = due(
            &registry.sessions,
            &state.worktrees,
            &keep,
            |path| path.exists(),
            &mut watch.missing,
            now,
        );
        let mut closed = Vec::new();
        for (id, folder) in due {
            // A terminal may have attached since the look began.
            if self
                .attached_session_names()
                .map_or(true, |attached| attached.contains(&id))
            {
                continue;
            }
            match self.close(&id) {
                Ok(()) => {
                    eprintln!(
                        "riwork sessions: ended {id}, whose worktree {} was deleted",
                        folder.display()
                    );
                    watch.missing.remove(&id);
                    closed.push(id);
                }
                // Another RiWork may have ended it first; the next look tries again.
                Err(error) => eprintln!(
                    "riwork sessions: could not end {id} of deleted worktree {}: {error}",
                    folder.display()
                ),
            }
        }
        closed
    }

    /// The live sessions a terminal is attached to. A missing server has none.
    fn attached_session_names(&self) -> Result<HashSet<String>, String> {
        let output = self.tmux_command(&[
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_attached}",
        ])?;
        if !output.status.success() {
            if no_tmux_server(&output) {
                return Ok(HashSet::new());
            }
            return Err(tmux_error(&output));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (name, clients) = line.split_once('\t')?;
                (clients.trim().parse::<u32>().ok()? > 0).then(|| name.to_owned())
            })
            .collect())
    }
}

/// The sessions to end now, each with its worktree's folder. `missing` holds when each
/// candidate was first seen without its folder; it is updated here, and forgets a
/// session whose folder is back or that is gone from the registry. `keep` are the
/// sessions shown in a tab or attached to a terminal: tracked, never ended.
fn due(
    sessions: &[ShellSession],
    worktrees: &[Worktree],
    keep: &HashSet<String>,
    exists: impl Fn(&Path) -> bool,
    missing: &mut BTreeMap<String, Instant>,
    now: Instant,
) -> Vec<(String, PathBuf)> {
    let mut seen = HashSet::new();
    let mut due = Vec::new();
    for session in sessions {
        let Some(worktree_id) = session.worktree_id.as_deref() else {
            continue;
        };
        let Some(worktree) = worktrees.iter().find(|w| w.id == worktree_id) else {
            continue;
        };
        let deleted = !worktree.is_primary
            && !exists(&worktree.path)
            && worktree.path.parent().is_some_and(|parent| exists(parent));
        if !deleted {
            continue;
        }
        seen.insert(session.id.clone());
        let since = *missing.entry(session.id.clone()).or_insert(now);
        if !keep.contains(&session.id) && now.duration_since(since) >= GRACE {
            due.push((session.id.clone(), worktree.path.clone()));
        }
    }
    missing.retain(|id, _| seen.contains(id));
    due
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree(id: &str, path: &str, is_primary: bool) -> Worktree {
        Worktree {
            id: id.into(),
            project_id: "project".into(),
            branch: id.into(),
            path: path.into(),
            is_primary,
            repository_root: None,
            created_at: 0,
        }
    }

    fn session(id: &str, worktree_id: Option<&str>) -> ShellSession {
        let mut session: ShellSession = serde_json::from_value(serde_json::json!({
            "id": id,
            "project_id": "project",
            "worktree_id": worktree_id,
            "kind": "project",
            "cwd": "/work",
            "command": null,
            "created_at_unix": 0,
        }))
        .unwrap();
        session.alive = true;
        session
    }

    /// `/work` and `/work/main` exist; `/work/gone` and everything under
    /// `/unmounted` do not.
    fn exists(path: &Path) -> bool {
        matches!(path.to_str(), Some("/" | "/work" | "/work/main"))
    }

    #[test]
    fn a_deleted_worktree_ends_its_sessions_after_the_grace_period_only() {
        let worktrees = [
            worktree("main", "/work/main", true),
            worktree("gone", "/work/gone", false),
        ];
        let sessions = [
            session("in-gone", Some("gone")),
            session("in-main", Some("main")),
            session("project", None),
        ];
        let mut missing = BTreeMap::new();
        let start = Instant::now();
        let none = HashSet::new();
        // First seen: tracked, not ended.
        assert!(due(&sessions, &worktrees, &none, exists, &mut missing, start).is_empty());
        assert_eq!(missing.keys().collect::<Vec<_>>(), ["in-gone"]);
        // Still inside the grace period.
        let early = start + GRACE - Duration::from_secs(1);
        assert!(due(&sessions, &worktrees, &none, exists, &mut missing, early).is_empty());
        // Gone for the whole grace period: ended, with its folder.
        let late = start + GRACE;
        assert_eq!(
            due(&sessions, &worktrees, &none, exists, &mut missing, late),
            [("in-gone".to_owned(), PathBuf::from("/work/gone"))]
        );
    }

    #[test]
    fn a_session_in_a_tab_or_attached_is_kept_and_a_folder_that_returns_starts_over() {
        let worktrees = [worktree("gone", "/work/gone", false)];
        let sessions = [
            session("shown", Some("gone")),
            session("other", Some("gone")),
        ];
        let mut missing = BTreeMap::new();
        let start = Instant::now();
        let keep = HashSet::from(["shown".to_owned()]);
        due(&sessions, &worktrees, &keep, exists, &mut missing, start);
        let late = start + GRACE;
        assert_eq!(
            due(&sessions, &worktrees, &keep, exists, &mut missing, late),
            [("other".to_owned(), PathBuf::from("/work/gone"))]
        );
        // Once nothing shows it, its grace has already run.
        assert_eq!(
            due(
                &sessions,
                &worktrees,
                &HashSet::new(),
                exists,
                &mut missing,
                late
            )
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
            ["shown", "other"]
        );
        // The folder comes back: forgotten, and a later deletion waits a full period again.
        let back = |_: &Path| true;
        assert!(
            due(
                &sessions,
                &worktrees,
                &HashSet::new(),
                back,
                &mut missing,
                late
            )
            .is_empty()
        );
        assert!(missing.is_empty());
        assert!(
            due(
                &sessions,
                &worktrees,
                &HashSet::new(),
                exists,
                &mut missing,
                late
            )
            .is_empty()
        );
    }

    #[test]
    fn a_missing_parent_a_primary_worktree_or_an_unknown_one_is_never_a_deletion() {
        let worktrees = [
            // The drive it was on is not mounted: its parent is missing too.
            worktree("unmounted", "/unmounted/drive/wt", false),
            // The project's own folder is never a worktree to clean up.
            worktree("primary", "/work/gone", true),
        ];
        let sessions = [
            session("on-drive", Some("unmounted")),
            session("in-primary", Some("primary")),
            session("unknown", Some("forgotten")),
        ];
        let mut missing = BTreeMap::new();
        let start = Instant::now();
        let none = HashSet::new();
        due(&sessions, &worktrees, &none, exists, &mut missing, start);
        assert!(
            due(
                &sessions,
                &worktrees,
                &none,
                exists,
                &mut missing,
                start + GRACE * 10
            )
            .is_empty()
        );
        assert!(missing.is_empty());
    }

    #[test]
    fn a_session_that_left_the_registry_is_forgotten() {
        let worktrees = [worktree("gone", "/work/gone", false)];
        let mut missing = BTreeMap::new();
        let start = Instant::now();
        let none = HashSet::new();
        due(
            &[session("closed", Some("gone"))],
            &worktrees,
            &none,
            exists,
            &mut missing,
            start,
        );
        assert_eq!(missing.len(), 1);
        due(&[], &worktrees, &none, exists, &mut missing, start);
        assert!(missing.is_empty());
    }
}
