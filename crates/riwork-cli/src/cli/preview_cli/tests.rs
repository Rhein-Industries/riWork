use super::*;
use riwork_core::store::Store;

struct Fixture {
    path: PathBuf,
    home: PathBuf,
    project_id: String,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-preview-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("project")).unwrap();
        let home = path.join("state");
        let project_id = Store::open(&home)
            .unwrap()
            .add_project(path.join("project"), Some("Preview"))
            .unwrap()
            .id;
        Self {
            path,
            home,
            project_id,
        }
    }
    fn args(&self, words: &[&str]) -> Vec<String> {
        let mut args = words.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        args.extend([
            "--project".into(),
            self.project_id.clone(),
            "--cwd".into(),
            self.path.join("project").to_string_lossy().into_owned(),
        ]);
        args
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn ios_launch_has_owned_device_selection_and_explicit_wait_background_flags() {
    let f = Fixture::new();
    let (project, spec, background, wait) = open_arguments(
        &f.home,
        f.args(&[
            "--ios",
            "--scheme",
            "PhoneApp",
            "--xcode-project",
            "PhoneApp.xcodeproj",
            "--device-type",
            "iPhone 17",
            "--runtime",
            "iOS 26.0",
            "--wait",
            "--background",
        ]),
    )
    .unwrap();
    assert_eq!(project, f.project_id);
    assert!(background && wait);
    assert_eq!(spec.mode, PreviewMode::IosSimulator);
    assert!(spec.command.is_empty());
    let ios = spec.ios.unwrap();
    assert_eq!(ios.scheme, "PhoneApp");
    assert_eq!(ios.configuration, "Debug");
    assert_eq!(ios.project, Some(PathBuf::from("PhoneApp.xcodeproj")));
    assert_eq!(ios.device_type.as_deref(), Some("iPhone 17"));
    assert!(open_arguments(&f.home, f.args(&["--ios", "--udid", "existing-device"])).is_err());
}

#[test]
fn ambiguous_or_unknown_launch_arguments_fail_before_enqueue() {
    let f = Fixture::new();
    for words in [
        vec!["--ios", "--command", "app"],
        vec!["--ios", "--mode", "cooperative"],
        vec!["--command", "app", "--mode", "surprise"],
        vec!["--command", "app", "--scheme", "Ignored"],
        vec![],
    ] {
        assert!(
            open_arguments(&f.home, f.args(&words)).is_err(),
            "{words:?}"
        );
    }
    let (_, spec, _, _) = open_arguments(
        &f.home,
        f.args(&[
            "--command",
            "cargo run --example preview",
            "--mode",
            "window_capture",
        ]),
    )
    .unwrap();
    assert_eq!(spec.mode, PreviewMode::WindowCapture);
    assert_eq!(spec.command, "cargo run --example preview");
}

#[test]
fn touch_input_preserves_frame_coordinates_and_rejects_nan_or_extra_arguments() {
    let id = uuid::Uuid::new_v4().to_string();
    let (_, event) = input_arguments("tap", vec![id.clone(), "12.5".into(), "220".into()]).unwrap();
    assert_eq!(
        event,
        PreviewInput::Click {
            x: 12.5,
            y: 220.,
            button: PreviewMouseButton::Left
        }
    );
    for value in ["NaN", "inf", "-1"] {
        assert!(input_arguments("tap", vec![id.clone(), value.into(), "220".into()]).is_err());
    }
    let (_, event) = input_arguments(
        "input",
        vec![
            id.clone(),
            "--input-json".into(),
            "{\"kind\":\"pointer_down\",\"x\":20,\"y\":30,\"button\":\"left\"}".into(),
        ],
    )
    .unwrap();
    assert!(matches!(
        event,
        PreviewInput::PointerDown { x: 20., y: 30., .. }
    ));
    assert!(input_arguments("input", vec![id, "--input-json".into(), "{}".into()]).is_err());
}

#[test]
fn project_inference_uses_longest_matching_worktree_path() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.path.join("project/nested")).unwrap();
    let id = project_for(&f.home, None, &f.path.join("project/nested")).unwrap();
    assert_eq!(id, f.project_id);
    assert!(project_for(&f.home, None, &f.path).is_err());
}
