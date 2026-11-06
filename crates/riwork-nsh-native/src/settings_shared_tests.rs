use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct SharedBackend {
    value: Mutex<Result<Arc<CoreSettings>, String>>,
    owned_reads: AtomicUsize,
}

impl SettingsBackend for SharedBackend {
    fn get(&self) -> Result<CoreSettings, String> {
        self.owned_reads.fetch_add(1, Ordering::Relaxed);
        self.get_shared().map(|value| (*value).clone())
    }
    fn get_shared(&self) -> Result<Arc<CoreSettings>, String> {
        self.value.lock().unwrap().clone()
    }
    fn set(&self, _: SettingsPatch) -> Result<CoreSettings, String> {
        self.get()
    }
    fn change_text_size(&self, _: SizeChange) -> Result<CoreSettings, String> {
        self.get()
    }
}

fn settings(overrides: usize) -> CoreSettings {
    CoreSettings {
        chat_display_modes: (0..overrides)
            .map(|n| (format!("chat-{n:08}"), Default::default()))
            .collect(),
        ..CoreSettings::default()
    }
}

#[test]
fn shared_preferences_retain_coherent_generations_and_errors_without_owned_reads() {
    let first = Arc::new(settings(100));
    let backend = Arc::new(SharedBackend {
        value: Mutex::new(Ok(first.clone())),
        owned_reads: AtomicUsize::new(0),
    });
    let service = PreferenceService::with_backend(backend.clone());
    let (held, error) = service.core();
    assert!(error.is_none());
    assert!(Arc::ptr_eq(&first, &held));
    service.refresh();
    assert!(Arc::ptr_eq(&held, &service.core().0));

    *backend.value.lock().unwrap() = Err("io: fixture denied".into());
    service.refresh();
    let (failed, error) = service.core();
    assert!(Arc::ptr_eq(&held, &failed));
    assert_eq!(error.as_deref(), Some("io: fixture denied"));
    assert_eq!(service.get().error.as_deref(), Some("io: fixture denied"));

    let next = Arc::new(CoreSettings {
        theme: ThemeChoice::Native,
        chat_display_modes: Default::default(),
        ..(*first).clone()
    });
    *backend.value.lock().unwrap() = Ok(next.clone());
    service.refresh();
    let (latest, error) = service.core();
    assert!(error.is_none());
    assert!(Arc::ptr_eq(&next, &latest));
    assert_eq!(held.chat_display_modes.len(), 100);
    assert!(latest.chat_display_modes.is_empty());
    assert_eq!(backend.owned_reads.load(Ordering::Relaxed), 0);
}

#[test]
fn core_backend_shared_reads_keep_owned_results_independent_and_refresh_files() {
    let dir = tempfile::tempdir().unwrap();
    let backend = CoreSettingsBackend::new(dir.path().into()).unwrap();
    let first = backend.get_shared().unwrap();
    assert!(Arc::ptr_eq(&first, &backend.get_shared().unwrap()));
    let mut owned = backend.get().unwrap();
    owned.theme = ThemeChoice::Native;
    owned
        .chat_display_modes
        .insert("owned-only".into(), Default::default());
    assert!(
        !backend
            .get_shared()
            .unwrap()
            .chat_display_modes
            .contains_key("owned-only")
    );
    std::fs::write(
        dir.path().join("settings.json"),
        serde_json::to_vec(&owned).unwrap(),
    )
    .unwrap();
    let latest = backend.get_shared().unwrap();
    assert_eq!(*latest, owned);
    assert!(!Arc::ptr_eq(&first, &latest));
    assert!(!first.chat_display_modes.contains_key("owned-only"));
    std::fs::write(dir.path().join("settings.json"), b"invalid").unwrap();
    assert!(backend.get_shared().is_err());
    assert!(backend.get().is_err());
}

#[test]
fn borrowed_theme_previews_match_cloned_settings_for_every_theme_and_terminal_flag() {
    for saved in [ThemeChoice::Native, ThemeChoice::Ghostty] {
        for native in [false, true] {
            for forced in [false, true] {
                let base = CoreSettings {
                    theme: saved,
                    native_terminal_colors: native,
                    use_riwork_colors: forced,
                    ..settings(32)
                };
                let backend = Arc::new(SharedBackend {
                    value: Mutex::new(Ok(Arc::new(base.clone()))),
                    owned_reads: AtomicUsize::new(0),
                });
                let service = PreferenceService::with_backend(backend.clone());
                for error in [None, Some("io: fixture denied".to_owned())] {
                    if let Some(error) = &error {
                        *backend.value.lock().unwrap() = Err(error.clone());
                        service.refresh();
                    }
                    for theme in ThemeChoice::ALL {
                        for dark in [false, true] {
                            let mut previous = base.clone();
                            previous.theme = theme;
                            assert_eq!(
                                service.appearance(Some(&theme_id(theme)), dark).unwrap(),
                                AppearanceDto::from_settings(&previous, dark, error.clone())
                            );
                        }
                    }
                    assert_eq!(
                        service.appearance(None, false).unwrap(),
                        AppearanceDto::from_settings(&base, false, error)
                    );
                }
                assert!(service.appearance(Some("not-a-theme"), false).is_err());
                assert_eq!(service.core().0.theme, saved);
                assert_eq!(backend.owned_reads.load(Ordering::Relaxed), 0);
            }
        }
    }
}

#[test]
#[ignore = "manual paired preference snapshot and appearance benchmark"]
fn measure_shared_preference_reads() {
    use std::{hint::black_box, time::Instant};
    for overrides in [10, 10_000] {
        let backend = Arc::new(SharedBackend {
            value: Mutex::new(Ok(Arc::new(settings(overrides)))),
            owned_reads: AtomicUsize::new(0),
        });
        let service = PreferenceService::with_backend(backend);
        for appearance in [false, true] {
            let mut samples = [Vec::new(), Vec::new()];
            for sample in 0..7 {
                for candidate in if sample % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                } {
                    let started = Instant::now();
                    for _ in 0..100 {
                        if candidate {
                            if appearance {
                                black_box(service.appearance(Some("native"), true).unwrap());
                            } else {
                                black_box(service.core());
                            }
                        } else {
                            // Prior path: clone the full settings while the state
                            // lock is held, then build an optional theme preview.
                            let state = service.state.lock().unwrap();
                            let mut owned = (*state.settings).clone();
                            if appearance {
                                owned.theme = parse_theme("native").unwrap();
                                black_box(AppearanceDto::from_settings(
                                    &owned,
                                    true,
                                    state.error.clone(),
                                ));
                            } else {
                                black_box((owned, state.error.clone()));
                            }
                        }
                    }
                    samples[usize::from(candidate)]
                        .push(started.elapsed().as_secs_f64() * 1e6 / 100.0);
                }
            }
            let raw = samples.clone();
            for values in &mut samples {
                values.sort_by(f64::total_cmp);
            }
            eprintln!(
                "PREFERENCE_BENCH {}",
                serde_json::json!({"overrides":overrides, "appearance":appearance, "before_us":raw[0], "after_us":raw[1], "before_median_us":samples[0][3], "after_median_us":samples[1][3], "iterations":100})
            );
        }
    }
}
