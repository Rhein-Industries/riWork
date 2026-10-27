//! Blocking persistence barrier for host close, quit and reload.
//!
//! Phase 1 has no draft writer. A later draft service registers its barrier
//! here; the host already propagates failure before tearing down any engine.
//! Layout barriers are window-scoped and remain owned by host_layouts.
use std::sync::{Arc, Mutex, OnceLock};

type Flush = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

fn hooks() -> &'static Mutex<Vec<Flush>> {
    static HOOKS: OnceLock<Mutex<Vec<Flush>>> = OnceLock::new();
    HOOKS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn register_draft_flush(hook: impl Fn() -> Result<(), String> + Send + Sync + 'static) {
    hooks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Arc::new(hook));
}

/// Invoke only on the blocking host service thread. Do not hold the registry
/// mutex during IO, and attempt every writer even if an earlier writer fails.
pub fn flush_blocking() -> Result<(), String> {
    let snapshot = hooks().lock().unwrap_or_else(|e| e.into_inner()).clone();
    flush(&snapshot)
}

/// Opt-in failure injection for an isolated real-host verification process.
/// This is absent from production builds; it exercises the same registered
/// writer barrier as a draft IO failure without needing deferred draft UI.
#[cfg(feature = "test-support")]
pub(crate) fn install_fixture() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("RIWORK_NSH_TEST_FAIL_DRAFT_FLUSH").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            register_draft_flush(|| Err("fixture draft flush failure".into()));
        }
    });
}

fn flush(hooks: &[Flush]) -> Result<(), String> {
    let errors: Vec<_> = hooks.iter().filter_map(|hook| hook().err()).collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_draft_flush_is_reported_after_every_writer_was_attempted() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let last = count.clone();
        assert_eq!(
            flush(&[
                Arc::new(|| Err("draft save failed".into())),
                Arc::new(move || {
                    last.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }),
            ]),
            Err("draft save failed".into())
        );
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert!(flush(&[]).is_ok());
    }
}
