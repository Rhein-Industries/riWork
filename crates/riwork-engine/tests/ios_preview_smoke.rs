//! Actual Xcode + CoreSimulator integration proof. Uses a fresh owned simulator
//! and never opens Simulator.app or addresses the global `booted` device alias.
//!
//! cargo test --offline -p riwork-engine --test ios_preview_smoke -- --ignored --nocapture

#![cfg(target_os = "macos")]
use riwork_engine::app_preview::{
    AppPreview, IosPreviewSpec, PreviewInput, PreviewMode, PreviewMouseButton, PreviewPhase,
    PreviewSpec, PreviewState,
};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn wait_for(
    preview: &AppPreview,
    seconds: u64,
    condition: impl Fn(&PreviewState) -> bool,
) -> PreviewState {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut last = String::new();
    loop {
        let state = preview.snapshot();
        if state.message != last {
            eprintln!("{:?}: {}", state.phase, state.message);
            last = state.message.clone();
        }
        if condition(&state) {
            return state;
        }
        if state.phase == PreviewPhase::Error
            || state.phase == PreviewPhase::Exited
            || Instant::now() > deadline
        {
            let details = format!(
                "phase={:?}, message={}, warning={:?}, logs={:?}",
                state.phase, state.message, state.warning, state.logs
            );
            preview.stop();
            let cleanup_deadline = Instant::now() + Duration::from_secs(45);
            while preview.snapshot().phase != PreviewPhase::Stopped
                && Instant::now() < cleanup_deadline
            {
                thread::sleep(Duration::from_millis(50));
            }
            panic!("iOS preview did not reach expected state: {details}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}
fn proof_file(udid: &str) -> PathBuf {
    let output = Command::new("/usr/bin/xcrun")
        .args([
            "simctl",
            "get_app_container",
            udid,
            "dev.riwork.ios-stream-proof",
            "data",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()).join("Documents/proof.json")
}
fn proof(path: &PathBuf) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
#[ignore = "Boots a fresh simulator and builds the real UIKit stream/touch fixture; requires Xcode + installed iOS runtime"]
fn xcode_build_stream_touch_pause_and_cleanup() {
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/ios-preview-bridge/tests")
        .canonicalize()
        .unwrap();
    let preview = AppPreview::launch(PreviewSpec {
        command: String::new(),
        cwd,
        mode: PreviewMode::IosSimulator,
        ios: Some(IosPreviewSpec {
            scheme: "RiWorkPreviewProof".into(),
            ..Default::default()
        }),
    })
    .unwrap();
    // Background shell-created previews must warm up one frame before pausing.
    preview.set_visible(false);
    let initial = wait_for(&preview, 360, |state| state.frame.is_some());
    let udid = initial.ios.as_ref().unwrap().udid.clone();
    let frame = initial.frame.unwrap();
    assert!(frame.width > 200 && frame.height > frame.width);
    assert!(frame.width <= 2048 && frame.height <= 2048);
    wait_for(&preview, 10, |state| state.phase == PreviewPhase::Paused);
    preview.set_visible(true);
    let initial = wait_for(&preview, 10, |state| state.phase == PreviewPhase::Streaming);
    let initial_png = initial.frame.unwrap().png.clone();
    let sequence = preview.snapshot().frame.unwrap().sequence;
    preview.input(PreviewInput::Refresh);
    wait_for(&preview, 10, |state| {
        state
            .frame
            .as_ref()
            .is_some_and(|frame| frame.sequence > sequence)
    });
    let path = proof_file(&udid);
    preview.input(PreviewInput::Click {
        x: frame.width as f32 * 0.5,
        y: frame.height as f32 * 0.4,
        button: PreviewMouseButton::Left,
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while proof(&path)["taps"].as_u64().unwrap_or(0) < 1 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    let tap = proof(&path);
    assert_eq!(
        tap["taps"], 1,
        "Touch must reach the app's actual UIKit event handler"
    );
    assert!(tap["x"].as_f64().unwrap() > 100.0);
    preview.input(PreviewInput::PointerDown {
        x: frame.width as f32 * 0.3,
        y: frame.height as f32 * 0.6,
        button: PreviewMouseButton::Left,
    });
    for step in 1..=8 {
        preview.input(PreviewInput::PointerMove {
            x: frame.width as f32 * (0.3 + step as f32 * 0.03),
            y: frame.height as f32 * 0.6,
        });
        thread::sleep(Duration::from_millis(60));
    }
    preview.input(PreviewInput::PointerUp {
        x: frame.width as f32 * 0.54,
        y: frame.height as f32 * 0.6,
        button: PreviewMouseButton::Left,
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while proof(&path)["moves"].as_u64().unwrap_or(0) == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        proof(&path)["moves"].as_u64().unwrap() > 0,
        "Continuous drag contacts must reach UIKit"
    );
    wait_for(&preview, 10, |state| {
        state
            .frame
            .as_ref()
            .is_some_and(|frame| frame.png != initial_png)
    });
    preview.stop();
    wait_for(&preview, 45, |state| state.phase == PreviewPhase::Stopped);
    let output = Command::new("/usr/bin/xcrun")
        .args(["simctl", "list", "devices", "--json"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(&udid),
        "The owned simulator must be deleted on Stop"
    );
    eprintln!(
        "Verified real Xcode build, first-frame background readiness, live {}x{} framebuffer, UIKit tap+drag, pause/resume, and exact-device cleanup.",
        frame.width, frame.height
    );
}
