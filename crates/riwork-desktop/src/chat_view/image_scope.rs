//! Where a chat's image cards may read local images from.
//!
//! An image card reads a file read-only, after a click, through the same no-follow
//! regular-file walk Files uses (`file_preview::open_regular_in`). Its folders are not only
//! the chat's own worktree: an agent often shows an image another worktree of its project
//! made, and the global orchestrator one from any project. They come from what the store
//! registers, checked against the chat's own identity, never from the chat's working
//! directory, the window's selection, a common parent, HOME or the RiWork data directory:
//!
//! - an ordinary chat or a project orchestrator chat: its own project's root, repositories and
//!   worktrees, and that project's orchestrator folder (`RIWORK_HOME/orchestrators/projects/ID`);
//! - the global orchestrator chat (`{"scope":"global"}`, no project or worktree, working in
//!   `RIWORK_HOME/orchestrator`): every registered project's folders and orchestrator folder,
//!   and its own folder.
//!
//! A relative reference resolves against the chat's working directory only; an absolute path
//! or a `file://` URL may name a file in any folder of the scope. Files, chat links and Open
//! in Files preview keep the chat's own worktree (`links::resolve`), as before.

use std::path::{Component, Path, PathBuf};

use crate::{
    chat::model::{ChatInfo, OrchestratorScope},
    sessions::orchestrator_context,
    store::{Project, State},
};

/// The folders a chat's image cards may read from, and where its relative references start.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageScope {
    /// The chat's working directory: the only base of a relative reference.
    cwd: Option<PathBuf>,
    /// Registered folders, absolute, sorted and without repeats.
    roots: Vec<PathBuf>,
    /// The chat's own Files root, where Open in Files preview can reveal a file. Unchanged
    /// from before: the chat's worktree, when its project is the window's.
    files_root: Option<PathBuf>,
}

/// A reference the scope allows: the registered folder that holds it, and the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub root: PathBuf,
    pub path: PathBuf,
}

const NOWHERE: &str = "This chat’s project folders are unavailable for local image preview.";
const OUTSIDE: &str = "This image is outside the folders of this chat’s project.";

fn valid_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

fn push_project(roots: &mut Vec<PathBuf>, state: &State, project: &Project, home: &Path) {
    roots.push(project.root.clone());
    roots.extend(project.repository_roots.iter().cloned());
    roots.extend(
        state
            .worktrees
            .iter()
            .filter(|worktree| worktree.project_id == project.id)
            .map(|worktree| worktree.path.clone()),
    );
    // Project ids name a folder here, so only a well-formed one does.
    if valid_uuid(&project.id) {
        roots.push(orchestrator_context(home, Some(&project.id)));
    }
}

impl ImageScope {
    /// The scope of the chat `info` describes, as the transcript has it. `listed` is the chat
    /// as the host lists it, when the window has heard; an identity the two disagree on gets
    /// no folders. So does a chat whose project the store does not have, a project
    /// orchestrator of another project than its own, and a chat that calls itself the global
    /// orchestrator without being one (a project, a worktree, or another working directory).
    pub fn for_chat(
        info: &ChatInfo,
        listed: Option<&ChatInfo>,
        state: &State,
        home: &Path,
        files_root: Option<PathBuf>,
    ) -> Self {
        let nothing = Self {
            files_root: files_root.clone(),
            ..Self::default()
        };
        // Identity only: a working directory or worktree the list has not caught up with yet
        // is the transcript's to say.
        if listed.is_some_and(|listed| {
            listed.id != info.id
                || listed.orchestrator != info.orchestrator
                || listed.project_id != info.project_id
        }) {
            return nothing;
        }
        let mut roots = Vec::new();
        match &info.orchestrator {
            Some(OrchestratorScope::Global) => {
                if info.project_id.is_some()
                    || info.worktree_id.is_some()
                    || info.cwd != orchestrator_context(home, None)
                {
                    return nothing;
                }
                roots.push(orchestrator_context(home, None));
                for project in &state.projects {
                    push_project(&mut roots, state, project, home);
                }
            }
            Some(OrchestratorScope::Project { project_id })
                if info.project_id.as_deref() != Some(project_id.as_str()) =>
            {
                return nothing;
            }
            _ => {
                let Some(project) = info
                    .project_id
                    .as_deref()
                    .and_then(|id| state.projects.iter().find(|project| project.id == id))
                else {
                    return nothing;
                };
                push_project(&mut roots, state, project, home);
            }
        }
        roots.retain(|root| root.is_absolute());
        roots.sort();
        roots.dedup();
        Self {
            cwd: Some(info.cwd.clone()),
            roots,
            files_root,
        }
    }

    /// The registered folders.
    #[cfg(test)]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    #[cfg(test)]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// The folder and file `target` names, if the scope allows it: a relative path from the
    /// chat's working directory, or an absolute path or local `file://` URL, decoded and with
    /// `..` taken out, inside one of the folders (or inside its resolved form when only an
    /// ancestor of the folder is a link, as macOS's `/var` is). The file itself is not opened
    /// here; `file_preview::load` walks to it without following links. Reads the file system
    /// to resolve the folders, so it runs off the window.
    pub fn resolve(&self, target: &str) -> Result<Resolved, String> {
        if self.roots.is_empty() {
            return Err(NOWHERE.into());
        }
        let (path, _) = super::links::local_target(target)?;
        let path = if path.is_absolute() {
            path
        } else {
            match &self.cwd {
                Some(cwd) if cwd.is_absolute() => cwd.join(path),
                _ => return Err(OUTSIDE.into()),
            }
        };
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(OUTSIDE.into());
                    }
                }
                Component::CurDir => {}
                other => normalized.push(other.as_os_str()),
            }
        }
        let mut candidates = Vec::new();
        for root in &self.roots {
            candidates.push(root.clone());
            // A folder that is itself a link stays as it is: the walk refuses it, as Files does.
            let linked = std::fs::symlink_metadata(root)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(true);
            if !linked
                && let Ok(resolved) = root.canonicalize()
                && resolved != *root
            {
                candidates.push(resolved);
            }
        }
        candidates
            .into_iter()
            .filter(|root| normalized.starts_with(root) && normalized != *root)
            .max_by_key(|root| root.as_os_str().len())
            .map(|root| Resolved {
                root,
                path: normalized,
            })
            .ok_or_else(|| OUTSIDE.into())
    }

    /// Whether Open in Files preview can show `target`: it resolves in the chat's own Files
    /// root exactly as that action resolves it.
    pub fn in_files(&self, target: &str) -> bool {
        match (&self.files_root, &self.cwd) {
            (Some(root), Some(cwd)) => super::links::resolve(root, cwd, target).is_ok(),
            _ => false,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
