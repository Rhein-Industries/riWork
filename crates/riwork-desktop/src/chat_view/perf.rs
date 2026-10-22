//! Opt-in wall timings for native transcript profiling (no chat text is logged).
use std::{sync::OnceLock, time::Instant};

pub(super) struct Span {
    name: &'static str,
    started: Option<Instant>,
}
impl Span {
    pub fn new(name: &'static str) -> Self {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        Self {
            name,
            started: ENABLED
                .get_or_init(|| std::env::var_os("RIWORK_TOOL_UI_PERF").is_some())
                .then(Instant::now),
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            eprintln!(
                "tool-ui-perf {} {:.3} ms",
                self.name,
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

/// Debug-only, opt-in deterministic driver for Time Profiler runs. Commands are
/// accepted only with an explicitly isolated /tmp RIWORK_HOME; no providers run.
#[cfg(debug_assertions)]
impl super::ChatView {
    pub(super) fn start_perf_control(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.perf_control_started {
            return;
        }
        self.perf_control_started = true;
        let Some(path) =
            std::env::var_os("RIWORK_TOOL_UI_PERF_CONTROL").map(std::path::PathBuf::from)
        else {
            return;
        };
        let isolated = std::env::var_os("RIWORK_HOME")
            .map(std::path::PathBuf::from)
            .and_then(|home| home.canonicalize().ok())
            .is_some_and(|home| {
                home.starts_with(std::path::Path::new("/tmp").canonicalize().unwrap())
            });
        if !isolated {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let mut last = String::new();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                let path = path.clone();
                let text = cx
                    .background_executor()
                    .spawn(async move { std::fs::read_to_string(path).unwrap_or_default() })
                    .await;
                if text.is_empty() || text == last {
                    continue;
                }
                last = text.clone();
                let command = text
                    .trim()
                    .split_once(' ')
                    .map_or(text.trim(), |(_, command)| command)
                    .to_owned();
                if this
                    .update_in(cx, |view, window, cx| {
                        eprintln!("PHASE {command}");
                        let started = std::time::Instant::now();
                        match command.as_str() {
                            "normal" => view.choose_display(super::DisplayMode::Normal, cx),
                            "verbose" => view.choose_display(super::DisplayMode::Verbose, cx),
                            "expand" => view.expand_transcript(true, cx),
                            "collapse" => view.expand_transcript(false, cx),
                            "quit" => cx.quit(),
                            _ if command.starts_with("scroll:") => {
                                if let Ok(at) = command[7..].parse::<usize>() {
                                    view.list.pause_following_tail();
                                    view.list.scroll_to(gpui::ListOffset {
                                        item_ix: at.min(view.items_in_list),
                                        offset_in_item: gpui::px(0.),
                                    });
                                }
                            }
                            _ if command.starts_with("toggle:") => {
                                view.toggle_projected(&command[7..], cx)
                            }
                            _ if command.starts_with("preview:") => {
                                if let Some((_, item)) = view.projected_item(&command[8..])
                                    && let Some(image) = item.presentation.images.first().cloned()
                                {
                                    let owner = item.id.clone();
                                    view.preview_image_reference(
                                        &image,
                                        &format!("{owner}:image:0"),
                                        Some(&owner),
                                        cx,
                                    );
                                }
                            }
                            _ => {}
                        }
                        eprintln!(
                            "tool-ui-perf command {command} {:.3} ms",
                            started.elapsed().as_secs_f64() * 1000.
                        );
                        window.refresh();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}
