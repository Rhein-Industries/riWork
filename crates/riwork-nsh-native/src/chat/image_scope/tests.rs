//! Disposable scope fixtures; no real store, chats, or windows.
use super::*;
use riwork_engine::file_preview;
use serde_json::json;

const ALPHA: &str = "aaaaaaaa-0000-4000-8000-0000000000a1";
const BETA: &str = "bbbbbbbb-0000-4000-8000-0000000000b2";
const CHAT: &str = "11111111-2222-4333-8444-555555555555";

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    alpha: PathBuf,
    feature: PathBuf,
    beta: PathBuf,
    state: State,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let alpha = temp.path().join("alpha");
        let feature = temp.path().join("alpha-feature");
        let beta = temp.path().join("beta");
        for folder in [&home, &alpha, &feature, &beta] {
            std::fs::create_dir_all(folder).unwrap();
            image::RgbaImage::from_pixel(3, 2, image::Rgba([200, 40, 10, 255]))
                .save(folder.join("shot.png"))
                .unwrap();
        }
        let project = |id: &str, root: &Path| {
            serde_json::from_value(json!({
                "id": id, "name": id, "root": root, "created_at": 0
            }))
            .unwrap()
        };
        let state = State {
            projects: vec![project(ALPHA, &alpha), project(BETA, &beta)],
            worktrees: vec![
                serde_json::from_value(json!({
                    "id": "alpha-feature", "project_id": ALPHA, "branch": "feature",
                    "path": feature, "is_primary": false, "created_at": 0
                }))
                .unwrap(),
            ],
            ..Default::default()
        };
        Self {
            _temp: temp,
            home,
            alpha,
            feature,
            beta,
            state,
        }
    }
    fn info(&self) -> ChatInfo {
        serde_json::from_value(json!({"id": CHAT, "provider": "codex", "project_id": ALPHA,
            "cwd": self.alpha, "title": "Fixture", "created_at_unix": 1}))
        .unwrap()
    }
    fn scope(&self, info: &ChatInfo) -> ImageScope {
        ImageScope::for_chat(
            info,
            None,
            &self.state,
            &self.home,
            Some(self.alpha.clone()),
        )
    }
}

#[test]
fn registered_project_and_worktrees_are_allowed_without_widening_files() {
    let f = Fixture::new();
    let scope = f.scope(&f.info());
    for target in [
        "shot.png".into(),
        f.feature.join("shot.png").to_string_lossy().into_owned(),
    ] {
        let resolved = scope.resolve(&target).unwrap();
        assert!(matches!(
            file_preview::load(&resolved.root, &resolved.path, None),
            Ok(file_preview::PreviewContent::Image { .. })
        ));
    }
    assert!(scope.in_files("shot.png"));
    assert!(!scope.in_files(&f.feature.join("shot.png").to_string_lossy()));
    assert!(
        scope
            .resolve(&f.beta.join("shot.png").to_string_lossy())
            .is_err()
    );
    assert!(scope.resolve("../home/shot.png").is_err());
}

#[test]
fn missing_files_and_symlinks_cannot_be_read_through_the_scope() {
    let f = Fixture::new();
    let scope = f.scope(&f.info());
    std::os::unix::fs::symlink(f.beta.join("shot.png"), f.alpha.join("link.png")).unwrap();
    for name in ["link.png", "missing.png"] {
        let resolved = scope.resolve(name).unwrap();
        assert!(file_preview::load(&resolved.root, &resolved.path, None).is_err());
    }
}

#[test]
fn global_scope_requires_its_original_identity_and_cwd() {
    let f = Fixture::new();
    let mut info = f.info();
    info.project_id = None;
    info.orchestrator = Some(OrchestratorScope::Global);
    assert!(
        f.scope(&info)
            .resolve(&f.beta.join("shot.png").to_string_lossy())
            .is_err()
    );
    info.cwd = orchestrator_context(&f.home, None);
    assert!(
        f.scope(&info)
            .resolve(&f.beta.join("shot.png").to_string_lossy())
            .is_ok()
    );
    let mut listed = info.clone();
    listed.project_id = Some(ALPHA.into());
    let denied = ImageScope::for_chat(&info, Some(&listed), &f.state, &f.home, None);
    assert!(
        denied
            .resolve(&f.alpha.join("shot.png").to_string_lossy())
            .is_err()
    );
}

#[test]
fn attachment_scope_is_only_the_original_chats_canonical_id() {
    let f = Fixture::new();
    let mut info = f.info();
    let own = f.home.join(format!("chats/{CHAT}/attachments/shot.png"));
    assert!(f.scope(&info).resolve(&own.to_string_lossy()).is_ok());
    let other = f
        .home
        .join("chats/99999999-2222-4333-8444-555555555555/attachments/shot.png");
    assert!(f.scope(&info).resolve(&other.to_string_lossy()).is_err());
    info.id = "../other".into();
    assert!(f.scope(&info).resolve(&own.to_string_lossy()).is_err());
}
