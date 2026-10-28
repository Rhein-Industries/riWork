//! Memo edits survive AppView destruction through the host persistence barrier.
//!
//! Staging is synchronous memory-only work. Every disk writer checks the stage
//! revision while holding the process store-write lock; revisions remain known
//! after a flush, so an older async request cannot overwrite the close save.
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

type Writer = dyn Fn(&str, &str, u32) -> Result<(), String> + Send + Sync;

#[derive(Clone)]
struct Draft {
    text: String,
    revision: u32,
    dirty: bool,
}

#[derive(Default)]
struct State {
    revision: u32,
    drafts: BTreeMap<String, Draft>,
}

struct MemoRegistry {
    state: Mutex<State>,
    flush: Mutex<()>,
    writer: Arc<Writer>,
}

impl MemoRegistry {
    fn new(writer: impl Fn(&str, &str, u32) -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self {
            state: Mutex::new(State::default()),
            flush: Mutex::new(()),
            writer: Arc::new(writer),
        }
    }

    fn stage(&self, project: &str, text: &str) -> Result<u32, String> {
        crate::service::validate_uuid(project)?;
        let text = riwork_core::project_memo::normalize(text);
        if let Some(message) = riwork_core::project_memo::too_large(&text) {
            return Err(format!("invalid: {message}"));
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let revision = state
            .revision
            .checked_add(1)
            .ok_or("unavailable: memo revision space is exhausted")?;
        state.revision = revision;
        state.drafts.insert(
            project.into(),
            Draft {
                text: text.into_owned(),
                revision,
                dirty: true,
            },
        );
        Ok(revision)
    }

    fn is_current(&self, project: &str, text: &str, revision: u32) -> bool {
        let text = riwork_core::project_memo::normalize(text);
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drafts
            .get(project)
            .is_some_and(|draft| draft.revision == revision && draft.text == text)
    }

    fn saved(&self, project: &str, text: &str, revision: u32) {
        let text = riwork_core::project_memo::normalize(text);
        if let Some(draft) = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drafts
            .get_mut(project)
        {
            if draft.revision == revision && draft.text == text {
                draft.dirty = false;
            }
        }
    }

    fn flush_blocking(&self) -> Result<(), String> {
        // Close and reload barriers can arrive together. Never hold the stage
        // lock across IO: another window can continue typing during the flush.
        let _flush = self.flush.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let pending: Vec<_> = self
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .drafts
                .iter()
                .filter(|(_, draft)| draft.dirty)
                .map(|(project, draft)| (project.clone(), draft.clone()))
                .collect();
            if pending.is_empty() {
                return Ok(());
            }
            let mut errors = Vec::new();
            for (project, draft) in pending {
                match (self.writer)(&project, &draft.text, draft.revision) {
                    Ok(()) => self.saved(&project, &draft.text, draft.revision),
                    Err(error) => errors.push(error),
                }
            }
            if !errors.is_empty() {
                return Err(errors.join("; "));
            }
            // An edit staged while the previous snapshot was being written
            // remains dirty and is included before the host barrier returns.
        }
    }
}

static MEMOS: OnceLock<Arc<MemoRegistry>> = OnceLock::new();

/// Install only for the chosen process service. The writer must share the
/// service's store mutation lock, and check `is_current` under that lock.
pub(crate) fn init(writer: impl Fn(&str, &str, u32) -> Result<(), String> + Send + Sync + 'static) {
    MEMOS.get_or_init(|| {
        let registry = Arc::new(MemoRegistry::new(writer));
        crate::host_flush::register_draft_flush(flush_blocking);
        registry
    });
}

pub(crate) fn stage(project: &str, text: &str) -> Result<u32, String> {
    MEMOS
        .get()
        .ok_or("unavailable: memo service is not initialized")?
        .stage(project, text)
}

pub(crate) fn is_current(project: &str, text: &str, revision: u32) -> bool {
    MEMOS
        .get()
        .is_some_and(|registry| registry.is_current(project, text, revision))
}

pub(crate) fn saved(project: &str, text: &str, revision: u32) {
    if let Some(registry) = MEMOS.get() {
        registry.saved(project, text, revision);
    }
}

pub(crate) fn flush_blocking() -> Result<(), String> {
    MEMOS
        .get()
        .map_or(Ok(()), |registry| registry.flush_blocking())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Fixture {
        _dir: tempfile::TempDir,
        home: std::path::PathBuf,
        store: riwork_core::store::Store,
        project: String,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            let store = riwork_core::store::Store::open(&home).unwrap();
            let project = store
                .create_project(dir.path().join("project"), Some("Memo fixture"), false)
                .unwrap()
                .id;
            Self {
                _dir: dir,
                home,
                store,
                project,
            }
        }
        fn text(&self) -> String {
            self.store
                .snapshot()
                .unwrap()
                .project_memo(&self.project)
                .text
        }
    }

    #[test]
    fn close_barrier_persists_latest_text_without_a_javascript_engine() {
        // The host hook registry is process-global. A dedicated test process
        // keeps this real barrier test independent of chat's shutdown test.
        const CHILD: &str = "RIWORK_TEST_MEMO_CLOSE_BARRIER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "memo::tests::close_barrier_persists_latest_text_without_a_javascript_engine",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let fixture = Fixture::new();
        struct EmptySessions;
        impl crate::backend::SessionsBackend for EmptySessions {
            fn list(&self) -> anyhow::Result<crate::dto::SessionsSnapshot> {
                Ok(crate::dto::SessionsSnapshot::default())
            }
        }
        let service = crate::service::ProcessService::with_backends(
            fixture.home.clone(),
            Arc::new(crate::backend::CoreStoreBackend::new(fixture.home.clone())),
            Arc::new(EmptySessions),
        )
        .unwrap();
        crate::service::install_process_service(service.clone()).unwrap();
        let old = stage(&fixture.project, "first").unwrap();
        let revision = stage(&fixture.project, "last 日本語\r\n").unwrap();
        assert_eq!(fixture.text(), "");
        crate::host_flush::flush_blocking().unwrap();
        assert_eq!(fixture.text(), "last 日本語\n");
        assert!(is_current(&fixture.project, "last 日本語\r\n", revision));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let stale = runtime.block_on(service.set_project_memo(
            fixture.project.clone(),
            "first".into(),
            Some(old),
        ));
        assert!(stale.unwrap_err().contains("revision is stale"));
        assert_eq!(fixture.text(), "last 日本語\n");
        let registry = MEMOS.get().unwrap();
        assert!(!registry.state.lock().unwrap().drafts[&fixture.project].dirty);
    }

    #[test]
    fn flushed_latest_revision_refuses_an_older_async_save_after_close() {
        let fixture = Fixture::new();
        let home = fixture.home.clone();
        let registry = MemoRegistry::new(move |project, text, _| {
            riwork_core::store::Store::open(&home)?.set_project_memo(project, text)?;
            Ok(())
        });
        let old = registry.stage(&fixture.project, "A").unwrap();
        let latest = registry.stage(&fixture.project, "B").unwrap();
        registry.saved(&fixture.project, "A", old);
        registry.flush_blocking().unwrap();
        assert_eq!(fixture.text(), "B");
        // This is the check the async writer performs under the same store
        // mutation lock after it arrives behind the close barrier.
        assert!(!registry.is_current(&fixture.project, "A", old));
        assert!(registry.is_current(&fixture.project, "B", latest));
        assert_eq!(fixture.text(), "B");
    }

    #[test]
    fn failed_flush_keeps_dirty_text_for_a_successful_retry() {
        let fixture = Fixture::new();
        let fail = Arc::new(AtomicBool::new(true));
        let writer_fail = fail.clone();
        let home = fixture.home.clone();
        let registry = MemoRegistry::new(move |project, text, _| {
            if writer_fail.load(Ordering::SeqCst) {
                return Err("fixture write failure".into());
            }
            riwork_core::store::Store::open(&home)?.set_project_memo(project, text)?;
            Ok(())
        });
        registry
            .stage(&fixture.project, "kept after failure")
            .unwrap();
        assert!(
            registry
                .flush_blocking()
                .unwrap_err()
                .contains("fixture write failure")
        );
        assert_eq!(fixture.text(), "");
        assert!(registry.state.lock().unwrap().drafts[&fixture.project].dirty);
        fail.store(false, Ordering::SeqCst);
        registry.flush_blocking().unwrap();
        assert_eq!(fixture.text(), "kept after failure");
    }

    #[test]
    fn stage_validates_utf8_bytes_before_replacing_the_pending_draft() {
        let project = uuid::Uuid::new_v4().to_string();
        let registry = MemoRegistry::new(|_, _, _| panic!("staging must not perform IO"));
        let exact = "é".repeat(16_384);
        let revision = registry.stage(&project, &exact).unwrap();
        assert!(
            registry
                .stage(&project, &format!("{exact}é"))
                .unwrap_err()
                .contains("32768 bytes")
        );
        assert!(registry.is_current(&project, &exact, revision));
        assert!(registry.stage("../project", "x").is_err());
        // Validate normalized bytes, exactly as the durable core Store does.
        assert!(registry.stage(&project, &format!("{exact}\0\r")).is_err());
        assert!(registry.stage(&project, &format!("{exact}\0")).is_ok());
    }
}
