//! Main-thread GPU work belongs to the demo child; this test drives RiWork's
//! actual process supervisor, authenticated socket and frame/input lifecycle.
//!
//! cargo build -p riwork-gpui-preview --example counter
//! RIWORK_PREVIEW_DEMO="$PWD/target/debug/examples/counter" cargo test -p riwork-engine --test app_preview_smoke -- --ignored

use riwork_engine::app_preview::{
    AppPreview, PreviewInput, PreviewMode, PreviewMouseButton, PreviewPhase, PreviewSpec,
    PreviewState,
};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

fn wait_for(preview: &AppPreview, condition: impl Fn(&PreviewState) -> bool) -> PreviewState {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot = preview.snapshot();
        if condition(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "Preview did not reach expected state: phase={:?}, message={}, pid={:?}, frame={:?}, logs={:?}",
            snapshot.phase,
            snapshot.message,
            snapshot.pid,
            snapshot
                .frame
                .as_ref()
                .map(|frame| (frame.width, frame.height, frame.sequence)),
            snapshot.logs,
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "Requires the built, real-GPU windowless counter example; set RIWORK_PREVIEW_DEMO"]
fn real_gpui_child_streams_and_obeys_supervisor_lifecycle() {
    let executable = PathBuf::from(
        std::env::var_os("RIWORK_PREVIEW_DEMO")
            .expect("Set RIWORK_PREVIEW_DEMO to the built counter example"),
    )
    .canonicalize()
    .unwrap();
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let command = format!(
        "exec '{}'",
        executable.to_string_lossy().replace('\'', "'\\''")
    );
    let preview = AppPreview::launch(PreviewSpec {
        command,
        cwd,
        mode: PreviewMode::Cooperative,
        ios: None,
    })
    .unwrap();
    let initial = wait_for(&preview, |state| state.phase == PreviewPhase::Streaming);
    let initial_frame = initial.frame.unwrap();
    assert_eq!((initial_frame.width, initial_frame.height), (960, 600));
    let initial_pid = initial.pid.unwrap();

    preview.input(PreviewInput::Click {
        x: 80.0,
        y: 132.0,
        button: PreviewMouseButton::Left,
    });
    let clicked = wait_for(&preview, |state| {
        state.frame.as_ref().is_some_and(|frame| {
            frame.sequence > initial_frame.sequence && frame.png != initial_frame.png
        })
    });
    let clicked_frame = clicked.frame.unwrap();
    preview.input(PreviewInput::Text {
        text: "Hello 👋 世界".into(),
    });
    let typed = wait_for(&preview, |state| {
        state.frame.as_ref().is_some_and(|frame| {
            frame.sequence > clicked_frame.sequence && frame.png != clicked_frame.png
        })
    });
    assert_eq!(typed.phase, PreviewPhase::Streaming);

    preview.input(PreviewInput::Resize {
        width: 800,
        height: 450,
    });
    wait_for(&preview, |state| {
        state
            .frame
            .as_ref()
            .is_some_and(|frame| (frame.width, frame.height) == (800, 450))
    });
    preview.set_visible(false);
    let paused = wait_for(&preview, |state| state.phase == PreviewPhase::Paused);
    let paused_sequence = paused.frame.unwrap().sequence;
    thread::sleep(Duration::from_millis(300));
    assert_eq!(preview.snapshot().frame.unwrap().sequence, paused_sequence);
    preview.set_visible(true);
    wait_for(&preview, |state| {
        state.phase == PreviewPhase::Streaming
            && state
                .frame
                .as_ref()
                .is_some_and(|frame| frame.sequence > paused_sequence)
    });

    preview.restart();
    let restarted = wait_for(&preview, |state| {
        state.phase == PreviewPhase::Streaming && state.pid.is_some_and(|pid| pid != initial_pid)
    });
    let restarted_pid = restarted.pid.unwrap();
    assert_eq!(
        (
            restarted.frame.as_ref().unwrap().width,
            restarted.frame.as_ref().unwrap().height
        ),
        (960, 600)
    );
    assert_eq!(
        unsafe { libc::kill(initial_pid as libc::pid_t, 0) },
        -1,
        "Restart must reap the previous child"
    );

    preview.stop();
    wait_for(&preview, |state| state.phase == PreviewPhase::Stopped);
    assert_eq!(
        unsafe { libc::kill(restarted_pid as libc::pid_t, 0) },
        -1,
        "Stop must reap the child"
    );
}
