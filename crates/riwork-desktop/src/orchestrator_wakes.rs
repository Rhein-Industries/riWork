//! GPUI delivery for the engine's orchestrator wakes. Like schedule dispatch,
//! it runs in every app process (bundled or not, as no notification center is
//! involved); the engine lets one process deliver at a time.
pub use riwork_engine::orchestrator_wakes::*;
use std::{path::PathBuf, time::Duration};

pub fn start(home: PathBuf, cx: &mut gpui::App) {
    cx.spawn(async move |cx| {
        let mut runner = Runner::at(home);
        let mut last_error = None;
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let (returned, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = runner.tick();
                    (runner, result)
                })
                .await;
            runner = returned;
            match result {
                Ok(()) => last_error = None,
                Err(error) => {
                    if last_error.as_ref() != Some(&error) {
                        eprintln!("riwork orchestrator wakes: {error}");
                        last_error = Some(error);
                    }
                }
            }
        }
    })
    .detach();
}
