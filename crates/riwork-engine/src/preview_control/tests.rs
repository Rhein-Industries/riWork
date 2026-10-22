use super::*;
use crate::app_preview::{PreviewFrame, PreviewMode};
use std::sync::Arc;

struct Fixture {
    home: PathBuf,
    control: PreviewControl,
    host: String,
    project: String,
    preview: String,
}
impl Fixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("riwork-preview-control-{}", Uuid::new_v4()));
        let control = PreviewControl::open(&home).unwrap();
        let fixture = Self {
            home,
            control,
            host: Uuid::new_v4().to_string(),
            project: Uuid::new_v4().to_string(),
            preview: Uuid::new_v4().to_string(),
        };
        fixture
            .control
            .begin(&fixture.preview, &fixture.host)
            .unwrap();
        fixture
    }
    fn spec(&self) -> PreviewSpec {
        PreviewSpec {
            command: "development app".into(),
            cwd: self.home.clone(),
            mode: PreviewMode::Cooperative,
            ios: None,
        }
    }
    fn state(&self, frame: bool) -> PreviewState {
        PreviewState {
            phase: if frame {
                PreviewPhase::Streaming
            } else {
                PreviewPhase::Starting
            },
            mode: PreviewMode::Cooperative,
            pid: Some(1234),
            window_id: None,
            windows: vec![],
            message: "Preview fixture".into(),
            warning: None,
            frame: frame.then(|| {
                Arc::new(PreviewFrame {
                    png: vec![137, 80, 78, 71],
                    width: 320,
                    height: 480,
                    sequence: 1,
                })
            }),
            logs: vec![],
            ios: None,
        }
    }
    fn publish(&self, frame: bool) -> PreviewStatus {
        self.control.heartbeat(&self.host, &self.project).unwrap();
        self.control
            .publish(
                &self.preview,
                &self.host,
                &self.project,
                &self.spec(),
                0,
                &self.state(frame),
            )
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

#[test]
fn no_gui_refuses_launch_without_leaving_a_future_command() {
    let f = Fixture::new();
    let error = f
        .control
        .request(
            PreviewAction::Open {
                project_id: f.project.clone(),
                spec: f.spec(),
                background: false,
            },
            Duration::from_millis(1),
        )
        .unwrap_err();
    assert_eq!(error.code, "host_unavailable");
    assert!(f.control.entries("requests").unwrap().is_empty());
}

#[test]
fn open_round_trip_is_claimed_once_by_the_selected_project_window() {
    let f = Fixture::new();
    f.control.heartbeat(&f.host, &f.project).unwrap();
    let client = f.control.clone();
    let project = f.project.clone();
    let spec = f.spec();
    let caller = thread::spawn(move || {
        client.request(
            PreviewAction::Open {
                project_id: project,
                spec,
                background: true,
            },
            Duration::from_secs(3),
        )
    });
    let request = loop {
        let mut items = f.control.take_requests(&f.host, &f.project).unwrap();
        if let Some(request) = items.pop() {
            break request;
        }
        thread::sleep(Duration::from_millis(5));
    };
    assert!(matches!(
        request.action,
        PreviewAction::Open {
            background: true,
            ..
        }
    ));
    assert!(
        f.control
            .take_requests(&f.host, &f.project)
            .unwrap()
            .is_empty()
    );
    f.control
        .respond(
            &request.id,
            Ok(PreviewReply {
                preview_id: f.preview.clone(),
                reused: false,
            }),
        )
        .unwrap();
    assert_eq!(caller.join().unwrap().unwrap().preview_id, f.preview);
}

#[test]
fn controls_route_to_exact_window_even_when_another_window_has_the_project() {
    let f = Fixture::new();
    f.publish(true);
    let other = Uuid::new_v4().to_string();
    f.control.heartbeat(&other, &f.project).unwrap();
    let client = f.control.clone();
    let id = f.preview.clone();
    let caller = thread::spawn(move || {
        client.request(
            PreviewAction::Close { preview_id: id },
            Duration::from_secs(3),
        )
    });
    let request = loop {
        assert!(
            f.control
                .take_requests(&other, &f.project)
                .unwrap()
                .is_empty()
        );
        let mut items = f.control.take_requests(&f.host, &f.project).unwrap();
        if let Some(request) = items.pop() {
            break request;
        }
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(request.host_id, f.host);
    f.control
        .respond(
            &request.id,
            Ok(PreviewReply {
                preview_id: f.preview.clone(),
                reused: false,
            }),
        )
        .unwrap();
    caller.join().unwrap().unwrap();
}

#[test]
fn screenshots_require_a_real_frame_and_closed_tombstone_survives_queued_publish() {
    let f = Fixture::new();
    assert!(!f.publish(false).ready);
    assert_eq!(
        f.control.screenshot(&f.preview).unwrap_err().code,
        "not_ready"
    );
    assert!(f.publish(true).ready);
    let (_, bytes) = f.control.screenshot(&f.preview).unwrap();
    assert_eq!(bytes, [137, 80, 78, 71]);
    f.control
        .closed(&f.preview, &f.host, &f.project, &f.spec())
        .unwrap();
    assert_eq!(f.publish(true).phase, "closed");
    assert!(!f.control.frame_path(&f.preview).exists());
    f.control.begin(&f.preview, &f.host).unwrap();
    assert!(f.publish(true).ready);
}

#[test]
fn no_other_live_window_can_make_an_abandoned_preview_look_alive() {
    let f = Fixture::new();
    f.publish(true);
    f.control.remove_host(&f.host).unwrap();
    let other = Uuid::new_v4().to_string();
    f.control.heartbeat(&other, &f.project).unwrap();
    assert_eq!(
        f.control.status(&f.preview).unwrap_err().code,
        "host_unavailable"
    );
}

#[test]
fn timeouts_cancel_unclaimed_commands_and_invalid_identifiers_never_form_paths() {
    let f = Fixture::new();
    f.control.heartbeat(&f.host, &f.project).unwrap();
    let error = f
        .control
        .request(
            PreviewAction::Open {
                project_id: f.project.clone(),
                spec: f.spec(),
                background: false,
            },
            Duration::from_millis(1),
        )
        .unwrap_err();
    assert_eq!(error.code, "timeout");
    assert!(f.control.entries("requests").unwrap().is_empty());
    assert_eq!(
        f.control.status("../../state").unwrap_err().code,
        "invalid_request"
    );
    assert_eq!(
        f.control.heartbeat("bad", &f.project).unwrap_err().code,
        "invalid_request"
    );
}

#[test]
fn fresh_snapshot_waits_for_a_new_sequence_instead_of_returning_old_pixels() {
    let f = Fixture::new();
    f.publish(true);
    assert_eq!(
        f.control
            .wait_frame(&f.preview, 1, Duration::from_millis(1))
            .unwrap_err()
            .code,
        "timeout"
    );
    let mut state = f.state(true);
    state.frame = Some(Arc::new(PreviewFrame {
        png: vec![1, 2, 3],
        width: 320,
        height: 480,
        sequence: 2,
    }));
    f.control
        .publish(&f.preview, &f.host, &f.project, &f.spec(), 0, &state)
        .unwrap();
    assert_eq!(
        f.control
            .wait_frame(&f.preview, 1, Duration::from_millis(1))
            .unwrap()
            .1,
        [1, 2, 3]
    );
}

#[test]
fn old_window_cannot_close_or_publish_after_preview_ownership_changes() {
    let f = Fixture::new();
    f.publish(true);
    let other = Uuid::new_v4().to_string();
    f.control.heartbeat(&other, &f.project).unwrap();
    assert_eq!(
        f.control.begin(&f.preview, &other).unwrap_err().code,
        "owner_conflict"
    );
    f.control
        .closed(&f.preview, &f.host, &f.project, &f.spec())
        .unwrap();
    f.control.begin(&f.preview, &other).unwrap();
    assert_eq!(
        f.control
            .closed(&f.preview, &f.host, &f.project, &f.spec())
            .unwrap_err()
            .code,
        "owner_conflict"
    );
    assert_eq!(
        f.control
            .publish(
                &f.preview,
                &f.host,
                &f.project,
                &f.spec(),
                0,
                &f.state(true)
            )
            .unwrap_err()
            .code,
        "owner_conflict"
    );
    assert!(
        f.control
            .publish(&f.preview, &other, &f.project, &f.spec(), 0, &f.state(true))
            .unwrap()
            .ready
    );
}

#[test]
fn beginning_a_new_run_invalidates_readiness_and_rejects_old_queued_frames() {
    let f = Fixture::new();
    assert!(f.publish(true).ready);
    f.control
        .begin_run(&f.preview, &f.host, &f.project, &f.spec(), 1)
        .unwrap();
    let status = f.control.status(&f.preview).unwrap();
    assert!(!status.ready);
    assert_eq!(status.phase, "starting");
    assert_eq!(status.revision, 1);
    assert_eq!(
        f.control.screenshot(&f.preview).unwrap_err().code,
        "not_ready"
    );
    let late = f
        .control
        .publish(
            &f.preview,
            &f.host,
            &f.project,
            &f.spec(),
            0,
            &f.state(true),
        )
        .unwrap();
    assert!(!late.ready);
    assert_eq!(late.revision, 1);
    assert!(!f.control.frame_path(&f.preview).exists());
    let new = f
        .control
        .publish(
            &f.preview,
            &f.host,
            &f.project,
            &f.spec(),
            1,
            &f.state(true),
        )
        .unwrap();
    assert!(new.ready);
    assert_eq!(new.revision, 1);
}

#[cfg(unix)]
#[test]
fn a_heartbeat_is_written_only_when_its_record_changes() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    let path = f.control.path("hosts", &f.host);
    // Every write replaces the file, so an unchanged inode means no write.
    let written = || {
        let host: Host = f.control.read(&path).unwrap();
        (
            fs::metadata(&path).unwrap().ino(),
            host.project_id,
            host.updated_at,
        )
    };
    let mut skipped = false;
    for _ in 0..5 {
        f.control.heartbeat(&f.host, &f.project).unwrap();
        let before = written();
        f.control.heartbeat(&f.host, &f.project).unwrap();
        let after = written();
        // A pair that straddles a second moves `updated_at` and is written.
        if after.2 == before.2 {
            assert_eq!(
                after.0, before.0,
                "an unchanged heartbeat was written again"
            );
            skipped = true;
            break;
        }
        assert_ne!(after.0, before.0);
    }
    assert!(skipped);
    // Another project is written at once, as is a host that was removed.
    let other = Uuid::new_v4().to_string();
    f.control.heartbeat(&f.host, &other).unwrap();
    assert_eq!(written().1, other);
    let clone = f.control.clone();
    clone.remove_host(&f.host).unwrap();
    assert!(!path.exists());
    f.control.heartbeat(&f.host, &other).unwrap();
    assert_eq!(written().1, other);
    assert_eq!(f.control.hosts(&other).unwrap().len(), 1);
}

#[test]
#[ignore = "timing: cargo test -p riwork-engine --lib -- --ignored --nocapture time_a_windows_request_poll"]
fn time_a_windows_request_poll() {
    let f = Fixture::new();
    // What each project window runs every 250 ms.
    let rounds = 200;
    let started = Instant::now();
    for _ in 0..rounds {
        f.control.take_requests(&f.host, &f.project).unwrap();
    }
    eprintln!("take_requests: {:?} per call", started.elapsed() / rounds);
}
