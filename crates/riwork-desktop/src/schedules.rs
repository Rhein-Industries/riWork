//! GPUI schedule status; dispatch and persistence are owned by engine.
pub use riwork_engine::schedules::*;
use std::{path::PathBuf, time::Duration};

#[derive(Default)]
pub struct RuntimeStatus(pub Option<String>);
impl gpui::Global for RuntimeStatus {}
pub fn start(home: PathBuf, cx: &mut gpui::App) {
    cx.set_global(RuntimeStatus::default());
    cx.spawn(async move |cx| {
        let mut trackers = std::collections::HashMap::new();
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let path = home.clone();
            let (returned_trackers, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = ScheduleStore::at(path)
                        .and_then(|store| store.tick_tracked(now(), &mut trackers));
                    (trackers, result)
                })
                .await;
            trackers = returned_trackers;
            cx.update(|cx| {
                let error = result.err();
                if cx.global::<RuntimeStatus>().0 != error {
                    cx.set_global(RuntimeStatus(error));
                }
            });
        }
    })
    .detach();
}
