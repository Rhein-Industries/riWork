//! Image scopes over real PNG files: two worktrees of one project, a second project, a RiWork
//! data directory with orchestrator folders, links and unregistered files beside them.
use super::*;
use crate::{
    chat::model::{ChatImage, ImageSource},
    file_preview::PreviewContent,
    store::{Project, Worktree},
};
use base64::Engine;
use serde_json::json;
use std::fs;

const ALPHA: &str = "aaaaaaaa-0000-4000-8000-0000000000a1";
const BETA: &str = "bbbbbbbb-0000-4000-8000-0000000000b2";
/// The fixture's chat, and another chat beside it.
const CHAT: &str = "11111111-2222-4333-8444-555555555555";
const OTHER_CHAT: &str = "99999999-2222-4333-8444-555555555555";

pub(crate) struct Fixture {
    /// The fixture's folder as the system temp dir names it (on macOS under the `/var` link).
    pub base: PathBuf,
    pub home: PathBuf,
    pub alpha: PathBuf,
    pub feature: PathBuf,
    pub beta: PathBuf,
    pub state: State,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Send `source` to chat `id` as the chat host does: an owned snapshot in the chat's own
/// folder, named `content`, with its preview beside it. Returns the snapshot's path.
pub(crate) fn sent(home: &Path, id: &str, source: &Path) -> PathBuf {
    let chat = crate::chat::log::chat_dir(home, id).unwrap();
    fs::create_dir_all(&chat).unwrap();
    crate::chat::attachments::stage(&chat, source).unwrap().path
}

pub(crate) fn png(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbaImage::from_pixel(3, 2, image::Rgba([200, 40, 10, 255]))
        .save_with_format(path, image::ImageFormat::Png)
        .unwrap();
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let base =
            std::env::temp_dir().join(format!("riwork-image-scope-{}", uuid::Uuid::new_v4()));
        let home = base.join("home");
        let alpha = base.join("alpha");
        let feature = base.join("alpha-feature");
        let beta = base.join("beta");
        png(&alpha.join("shot.png"));
        fs::write(alpha.join("notes.txt"), "not an image").unwrap();
        png(&feature.join("my shot ü.png"));
        png(&feature.join("deep/inner.png"));
        fs::create_dir_all(feature.join("sub")).unwrap();
        png(&beta.join("b.png"));
        png(&base.join("outside.png"));
        png(&home.join("orchestrator/global.png"));
        png(&home.join(format!("orchestrators/projects/{ALPHA}/artifact.png")));
        png(&home.join(format!("orchestrators/projects/{BETA}/b-artifact.png")));
        png(&home.join("chats/some-chat/attachments/upload.png"));
        // Links out of a worktree: to a file and to a folder of the other project.
        std::os::unix::fs::symlink(beta.join("b.png"), alpha.join("link.png")).unwrap();
        std::os::unix::fs::symlink(&beta, feature.join("linkdir")).unwrap();
        let project = |id: &str, name: &str, root: &Path| Project {
            id: id.into(),
            name: name.into(),
            root: root.to_path_buf(),
            repository_roots: Vec::new(),
            folder_id: None,
            notify_on_agent_done: false,
            wake_orchestrators_on_agent_done: false,
            orchestrator: Default::default(),
            delegation_policy: Default::default(),
            codex_account: Default::default(),
            created_at: 0,
        };
        let worktree = |id: &str, project: &str, path: &Path, primary: bool| Worktree {
            id: id.into(),
            project_id: project.into(),
            branch: id.into(),
            path: path.to_path_buf(),
            is_primary: primary,
            repository_root: None,
            created_at: 0,
        };
        let state = State {
            projects: vec![
                project(ALPHA, "alpha", &alpha),
                project(BETA, "beta", &beta),
            ],
            worktrees: vec![
                worktree("alpha-main", ALPHA, &alpha, true),
                worktree("alpha-feature", ALPHA, &feature, false),
                worktree("beta-main", BETA, &beta, true),
            ],
            ..State::default()
        };
        Self {
            base,
            home,
            alpha,
            feature,
            beta,
            state,
        }
    }

    pub(crate) fn chat(
        &self,
        project: Option<&str>,
        worktree: Option<&str>,
        cwd: &Path,
        orchestrator: Option<serde_json::Value>,
    ) -> ChatInfo {
        let mut value = json!({
            "id": CHAT,
            "provider": "codex",
            "project_id": project,
            "worktree_id": worktree,
            "cwd": cwd,
            "title": "Chat",
            "created_at_unix": 1,
        });
        if let Some(scope) = orchestrator {
            value["orchestrator"] = scope;
        }
        serde_json::from_value(value).unwrap()
    }

    /// An ordinary chat of project alpha working in its main worktree, whose Files root that is.
    pub(crate) fn alpha_chat(&self) -> ImageScope {
        let info = self.chat(Some(ALPHA), Some("alpha-main"), &self.alpha, None);
        ImageScope::for_chat(
            &info,
            None,
            &self.state,
            &self.home,
            Some(self.alpha.clone()),
        )
    }

    pub(crate) fn global_chat(&self) -> ImageScope {
        let info = self.chat(
            None,
            None,
            &self.home.join("orchestrator"),
            Some(json!({"scope": "global"})),
        );
        ImageScope::for_chat(&info, None, &self.state, &self.home, None)
    }
}

/// Load a local reference as an image card does: `Ok(in_files)` for a decoded image.
pub(crate) fn show(scope: &ImageScope, target: &str) -> Result<bool, String> {
    let image = ChatImage {
        label: "Image".into(),
        source: ImageSource::Local {
            path: target.into(),
        },
    };
    let (content, in_files) = super::super::media::load(&image, scope);
    match content? {
        PreviewContent::Image { .. } => Ok(in_files),
        PreviewContent::Message(message) => Err(message),
        _ => Err("other content".into()),
    }
}

fn url(path: &Path) -> String {
    let mut out = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[test]
fn a_project_chat_shows_images_from_every_worktree_of_its_own_project() {
    let fixture = Fixture::new();
    let scope = fixture.alpha_chat();
    // Its own worktree, relative to its working directory: Files can show it too.
    assert_eq!(show(&scope, "shot.png"), Ok(true));
    assert_eq!(show(&scope, "./shot.png"), Ok(true));
    // Another worktree of the same project, by path with a space and a non-ASCII name, and as
    // a percent-encoded file URL. Files cannot reach it, so the card offers no Files preview.
    let other = fixture.feature.join("my shot ü.png");
    assert_eq!(show(&scope, &other.to_string_lossy()), Ok(false));
    assert!(url(&other).contains("%20") && url(&other).contains("%C3%BC"));
    assert_eq!(show(&scope, &url(&other)), Ok(false));
    assert_eq!(
        show(
            &scope,
            &format!("<{}>", fixture.feature.join("deep/inner.png").display())
        ),
        Ok(false)
    );
    // The project orchestrator's own folder belongs to the project.
    let artifact = fixture
        .home
        .join(format!("orchestrators/projects/{ALPHA}/artifact.png"));
    assert_eq!(show(&scope, &artifact.to_string_lossy()), Ok(false));
    // The same files under the resolved form of the temp folder, as an agent may print them.
    let resolved = other.canonicalize().unwrap();
    assert_eq!(show(&scope, &resolved.to_string_lossy()), Ok(false));
}

#[test]
fn a_project_chat_never_reaches_another_project_the_data_directory_or_unregistered_files() {
    let fixture = Fixture::new();
    let scope = fixture.alpha_chat();
    for target in [
        fixture.beta.join("b.png"),
        fixture
            .home
            .join(format!("orchestrators/projects/{BETA}/b-artifact.png")),
        fixture.home.join("orchestrator/global.png"),
        fixture.home.join("chats/some-chat/attachments/upload.png"),
        fixture.base.join("outside.png"),
    ] {
        let refused = show(&scope, &target.to_string_lossy()).unwrap_err();
        assert!(
            refused.contains("outside the folders"),
            "{target:?}: {refused}"
        );
    }
    // Relative escapes are taken out before the folder is chosen.
    assert!(
        show(&scope, "../beta/b.png")
            .unwrap_err()
            .contains("outside")
    );
    assert!(
        show(&scope, "../outside.png")
            .unwrap_err()
            .contains("outside")
    );
    let deep = "../".repeat(64) + "etc/hosts";
    assert!(show(&scope, &deep).is_err());
    let around = format!("{}/../beta/b.png", fixture.feature.display());
    assert!(show(&scope, &around).unwrap_err().contains("outside"));
}

#[test]
fn links_folders_missing_files_and_other_kinds_are_refused() {
    let fixture = Fixture::new();
    let scope = fixture.alpha_chat();
    // A link inside a worktree is not followed, to a file or through a folder.
    assert!(show(&scope, "link.png").is_err());
    let through = fixture.feature.join("linkdir/b.png");
    assert!(show(&scope, &through.to_string_lossy()).is_err());
    // A folder itself, and a worktree's root, are not images.
    assert!(show(&scope, &fixture.feature.to_string_lossy()).is_err());
    assert!(show(&scope, &fixture.feature.join("deep").to_string_lossy()).is_err());
    // Missing, deleted after the scope was made, and a text file named in an image reference.
    assert!(show(&scope, "missing.png").is_err());
    let doomed = fixture.feature.join("deep/inner.png");
    fs::remove_file(&doomed).unwrap();
    assert!(show(&scope, &doomed.to_string_lossy()).is_err());
    assert_eq!(
        show(&scope, "notes.txt").unwrap_err(),
        "This reference is not a supported image."
    );
    // Not a local file at all.
    assert!(show(&scope, "data:image/png;base64,AAAA").is_err());
    assert!(show(&scope, "mailto:someone@example.com").is_err());
}

#[test]
fn relative_references_start_at_the_chats_own_working_directory() {
    let fixture = Fixture::new();
    let sub = fixture.feature.join("sub");
    let info = fixture.chat(Some(ALPHA), Some("alpha-feature"), &sub, None);
    // The window selected alpha's main worktree; the chat works in the feature worktree.
    let scope = ImageScope::for_chat(
        &info,
        None,
        &fixture.state,
        &fixture.home,
        Some(fixture.alpha.clone()),
    );
    assert_eq!(scope.cwd(), Some(sub.as_path()));
    assert_eq!(show(&scope, "../deep/inner.png"), Ok(false));
    // Never the selected worktree: alpha's shot.png is not found relative to the feature.
    assert!(show(&scope, "shot.png").is_err());
    // By absolute path it is in the Files root, which Files can show whatever the cwd.
    assert_eq!(
        show(&scope, &fixture.alpha.join("shot.png").to_string_lossy()),
        Ok(true)
    );
}

#[test]
fn the_global_orchestrator_shows_every_project_and_orchestrator_folder_and_nothing_else() {
    let fixture = Fixture::new();
    let scope = fixture.global_chat();
    for target in [
        fixture.alpha.join("shot.png"),
        fixture.feature.join("my shot ü.png"),
        fixture.beta.join("b.png"),
        fixture
            .home
            .join(format!("orchestrators/projects/{ALPHA}/artifact.png")),
        fixture
            .home
            .join(format!("orchestrators/projects/{BETA}/b-artifact.png")),
    ] {
        assert_eq!(
            show(&scope, &target.to_string_lossy()),
            Ok(false),
            "{target:?}"
        );
    }
    // Its own folder, relative to it.
    assert_eq!(show(&scope, "global.png"), Ok(false));
    // Not the rest of the data directory, not an unregistered folder, not through a link.
    for target in [
        fixture.home.join("chats/some-chat/attachments/upload.png"),
        fixture.base.join("outside.png"),
        fixture.feature.join("linkdir/b.png"),
    ] {
        assert!(
            show(&scope, &target.to_string_lossy()).is_err(),
            "{target:?}"
        );
    }
    assert!(show(&scope, "../chats/some-chat/attachments/upload.png").is_err());
    assert!(
        !scope
            .roots()
            .iter()
            .any(|root| root == &fixture.home || root == &fixture.base),
        "never the data directory or a common parent: {:?}",
        scope.roots()
    );
}

#[test]
fn only_a_chat_that_is_the_global_orchestrator_gets_the_global_scope() {
    let fixture = Fixture::new();
    let global = json!({"scope": "global"});
    let context = fixture.home.join("orchestrator");
    let refused = |scope: ImageScope| {
        assert!(scope.roots().is_empty(), "{:?}", scope.roots());
        assert!(
            show(&scope, &fixture.beta.join("b.png").to_string_lossy())
                .unwrap_err()
                .contains("unavailable")
        );
    };
    // Claims to be global but has a project, a worktree, or works elsewhere.
    for info in [
        fixture.chat(Some(ALPHA), None, &context, Some(global.clone())),
        fixture.chat(None, Some("alpha-main"), &context, Some(global.clone())),
        fixture.chat(None, None, &fixture.alpha, Some(global.clone())),
        fixture.chat(None, None, &fixture.home, Some(global.clone())),
    ] {
        refused(ImageScope::for_chat(
            &info,
            None,
            &fixture.state,
            &fixture.home,
            None,
        ));
    }
    // The host lists the chat differently from what its transcript says.
    let genuine = fixture.chat(None, None, &context, Some(global.clone()));
    let mut listed = genuine.clone();
    listed.orchestrator = None;
    refused(ImageScope::for_chat(
        &genuine,
        Some(&listed),
        &fixture.state,
        &fixture.home,
        None,
    ));
    // The same listing agrees: global.
    let agreed = ImageScope::for_chat(
        &genuine,
        Some(&genuine),
        &fixture.state,
        &fixture.home,
        None,
    );
    assert_eq!(agreed, fixture.global_chat());
    // A chat without a project, one of a project the store does not have, and a project
    // orchestrator that names another project than its own.
    refused(ImageScope::for_chat(
        &fixture.chat(None, None, &fixture.alpha, None),
        None,
        &fixture.state,
        &fixture.home,
        None,
    ));
    refused(ImageScope::for_chat(
        &fixture.chat(
            Some("cccccccc-0000-4000-8000-0000000000c3"),
            None,
            &fixture.alpha,
            None,
        ),
        None,
        &fixture.state,
        &fixture.home,
        None,
    ));
    refused(ImageScope::for_chat(
        &fixture.chat(
            Some(ALPHA),
            None,
            &fixture.alpha,
            Some(json!({"scope": "project", "project_id": BETA})),
        ),
        None,
        &fixture.state,
        &fixture.home,
        None,
    ));
    // A project orchestrator of its own project is a project chat.
    let orchestrator = fixture.chat(
        Some(ALPHA),
        None,
        &fixture.home.join(format!("orchestrators/projects/{ALPHA}")),
        Some(json!({"scope": "project", "project_id": ALPHA})),
    );
    let scope = ImageScope::for_chat(&orchestrator, None, &fixture.state, &fixture.home, None);
    assert_eq!(show(&scope, "artifact.png"), Ok(false));
    assert!(show(&scope, &fixture.beta.join("b.png").to_string_lossy()).is_err());
}

#[test]
fn the_scope_follows_the_store_and_a_forgotten_worktree_is_out_of_it() {
    let mut fixture = Fixture::new();
    let before = fixture.alpha_chat();
    let other = fixture.feature.join("my shot ü.png");
    assert!(show(&before, &other.to_string_lossy()).is_ok());
    fixture
        .state
        .worktrees
        .retain(|worktree| worktree.id != "alpha-feature");
    let after = fixture.alpha_chat();
    assert_ne!(before, after, "the window replaces the view's scope");
    assert!(show(&after, &other.to_string_lossy()).is_err());
}

#[test]
fn the_original_file_resolver_still_keeps_to_the_chats_worktree() {
    let fixture = Fixture::new();
    let other = fixture.feature.join("my shot ü.png");
    // Chat links and Open in Files preview resolve exactly as before.
    assert!(super::super::links::resolve(&fixture.alpha, &fixture.alpha, "shot.png").is_ok());
    assert!(
        super::super::links::resolve(&fixture.alpha, &fixture.alpha, &other.to_string_lossy())
            .is_err()
    );
    assert!(super::super::links::resolve(&fixture.alpha, &fixture.alpha, "link.png").is_err());
    let scope = fixture.alpha_chat();
    assert!(scope.in_files("shot.png"));
    assert!(!scope.in_files(&other.to_string_lossy()));
    assert!(!fixture.global_chat().in_files("global.png"));
}

#[test]
fn data_and_url_images_are_read_as_before_whatever_the_scope() {
    let fixture = Fixture::new();
    let bytes = fs::read(fixture.alpha.join("shot.png")).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    for scope in [ImageScope::default(), fixture.alpha_chat()] {
        let data = ChatImage {
            label: "Image".into(),
            source: ImageSource::Data {
                mime: "image/png".into(),
                base64: encoded.clone(),
            },
        };
        let (content, in_files) = super::super::media::load(&data, &scope);
        assert!(matches!(content, Ok(PreviewContent::Image { .. })));
        assert!(!in_files);
        let ftp = ChatImage {
            label: "Image".into(),
            source: ImageSource::Url {
                url: "ftp://example.com/a.png".into(),
            },
        };
        assert_eq!(
            super::super::media::load(&ftp, &scope).0.err().as_deref(),
            Some("Unsupported image URL")
        );
        let gone = ChatImage {
            label: "Image".into(),
            source: ImageSource::Unavailable {
                reason: "Unsupported image media type".into(),
            },
        };
        assert_eq!(
            super::super::media::load(&gone, &scope).0.err().as_deref(),
            Some("Unsupported image media type")
        );
    }
}

mod mounted {
    use super::*;
    use crate::chat::{
        model::{ChatEvent, Item, ItemBody, ItemStatus, Presentation},
        wire::Envelope,
    };
    use gpui::{AppContext as _, TestAppContext};
    use gpui_kit::test::TestWindowExt;

    fn local(path: &Path) -> ChatImage {
        ChatImage {
            label: "Image".into(),
            source: ImageSource::Local {
                path: path.to_string_lossy().into_owned(),
            },
        }
    }

    #[gpui::test]
    fn a_mounted_chat_shows_another_worktrees_image_and_drops_it_when_the_scope_goes(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let (_handle, view, _feed) = super::super::super::editor_tests::mount(cx);
        let info = fixture.chat(Some(ALPHA), Some("alpha-main"), &fixture.alpha, None);
        let image = local(&fixture.feature.join("my shot ü.png"));
        let key = "item-1:image:0".to_owned();
        let slot = |cx: &mut TestAppContext| view.read_with(cx, |view, _| view.image_slot(&key));

        // Before the window gives a scope, a local image is not read.
        view.update(cx, |view, cx| {
            view.model
                .transcript
                .apply(&ChatEvent::Info { info: info.clone() });
            view.toggle_image(key.clone(), image.clone(), cx);
        });
        cx.run_until_parked();
        assert!(slot(cx).unwrap().unwrap_err().contains("unavailable"));

        // The scope arrives: the open card is read again under it and shows the image, which
        // is in another worktree than the chat's.
        view.update(cx, |view, cx| {
            view.set_image_scope(fixture.alpha_chat(), cx)
        });
        cx.run_until_parked();
        assert_eq!(slot(cx), Some(Ok(false)));
        // The same scope again changes nothing.
        view.update(cx, |view, cx| {
            view.set_image_scope(fixture.alpha_chat(), cx)
        });
        assert_eq!(slot(cx), Some(Ok(false)));

        // The worktree is forgotten while reads are in flight, allowed and not: what was shown
        // goes at once, and no read started under the old scope comes back to show it.
        let mut forgotten = fixture.state.clone();
        forgotten
            .worktrees
            .retain(|worktree| worktree.id != "alpha-feature");
        let revoked = ImageScope::for_chat(
            &info,
            None,
            &forgotten,
            &fixture.home,
            Some(fixture.alpha.clone()),
        );
        view.update(cx, |view, cx| {
            view.set_image_scope(revoked.clone(), cx);
            assert!(
                view.image_slot(&key).is_none(),
                "the shown image is dropped at once"
            );
            view.set_image_scope(fixture.alpha_chat(), cx);
            view.set_image_scope(revoked.clone(), cx);
        });
        cx.run_until_parked();
        assert!(slot(cx).unwrap().unwrap_err().contains("outside"));
        view.read_with(cx, |view, _| {
            assert!(view.media.viewer.is_none());
        });
    }

    #[gpui::test]
    fn open_in_files_is_offered_only_for_images_files_can_show(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (handle, view, _feed) = super::super::super::editor_tests::mount(cx);
        let info = fixture.chat(Some(ALPHA), Some("alpha-main"), &fixture.alpha, None);
        let snapshot = sent(&fixture.home, CHAT, &fixture.alpha.join("shot.png"));
        let item = Item {
            // User-message images use the image cards in the projected renderer.
            presentation: Presentation {
                images: vec![
                    local(&fixture.feature.join("my shot ü.png")),
                    local(&fixture.alpha.join("shot.png")),
                    local(&snapshot),
                ],
                ..Presentation::default()
            },
            started_at: None,
            ended_at: None,
            id: "item-1".into(),
            turn_id: None,
            status: ItemStatus::Completed,
            body: ItemBody::UserMessage {
                text: "Here are the screenshots.".into(),
            },
        };
        let envelopes = vec![
            Envelope {
                chat_id: "fixture-chat".into(),
                seq: 1,
                event: ChatEvent::Info { info },
            },
            Envelope {
                chat_id: "fixture-chat".into(),
                seq: 2,
                event: ChatEvent::ItemCompleted { item },
            },
        ];
        let scope = fixture.alpha_chat();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.accept(
                    vec![super::super::super::feed::FeedMsg::Events(envelopes)],
                    window,
                    cx,
                );
                view.set_image_scope(scope, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        for key in ["item-1:image:0", "item-1:image:1", "item-1:image:2"] {
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.click(format!("fold:{key}"), cx);
            })
            .unwrap();
            cx.run_until_parked();
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(view.read(cx).image_slot("item-1:image:0"), Some(Ok(false)));
            assert_eq!(view.read(cx).image_slot("item-1:image:1"), Some(Ok(true)));
            // The other worktree's image is viewed in the chat; its own opens in Files too.
            assert!(window.try_find("view:item-1:image:0").is_some());
            assert!(window.try_find("preview:item-1:image:0").is_none());
            assert!(window.try_find("view:item-1:image:1").is_some());
            assert!(window.try_find("preview:item-1:image:1").is_some());
            // The chat's own uploaded snapshot is shown too, and is not a Files item.
            assert_eq!(view.read(cx).image_slot("item-1:image:2"), Some(Ok(false)));
            assert!(window.try_find("view:item-1:image:2").is_some());
            assert!(window.try_find("preview:item-1:image:2").is_none());
            // The full viewer opens for the cross-worktree image.
            // Expanded image actions make this gallery taller than the viewport.
            // Bring the first image back into view before sending a pointer click.
            view.update(cx, |view, cx| {
                view.list.pause_following_tail();
                view.list.scroll_to(gpui::ListOffset::default());
                cx.notify();
            });
            window.render_frame(cx);
            window.click("view:item-1:image:0", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).media.viewer.as_deref(),
                Some("item-1:image:0")
            );
            assert!(window.try_find("close-chat-image").is_some());
        })
        .unwrap();
    }
}

#[test]
fn a_chat_shows_the_images_the_user_sent_it_and_no_other_chats() {
    let fixture = Fixture::new();
    let own = sent(&fixture.home, CHAT, &fixture.alpha.join("shot.png"));
    let other = sent(&fixture.home, OTHER_CHAT, &fixture.beta.join("b.png"));
    // A real snapshot, as Claude's chat shows the user's message: no extension.
    assert_eq!(own.file_name().unwrap(), "content");
    assert!(own.starts_with(fixture.home.join(format!("chats/{CHAT}/attachments"))));
    let chat = fixture.home.join(format!("chats/{CHAT}"));
    png(&chat.join("beside.png"));
    for scope in [fixture.alpha_chat(), fixture.global_chat()] {
        // Its own snapshot and its preview, by path and by file URL; Files cannot show them.
        assert_eq!(show(&scope, &own.to_string_lossy()), Ok(false));
        assert_eq!(show(&scope, &url(&own)), Ok(false));
        let preview = own.with_file_name("preview.png");
        assert_eq!(show(&scope, &preview.to_string_lossy()), Ok(false));
        // The snapshot's metadata is not an image: refused unread.
        assert_eq!(
            show(
                &scope,
                &own.with_file_name("attachment.json").to_string_lossy()
            )
            .unwrap_err(),
            "This reference is not a supported image."
        );
        // Another chat's snapshot, the chat's folder outside its attachments, and the way
        // round through `..`.
        for refused in [
            other.clone(),
            chat.join("beside.png"),
            PathBuf::from(format!(
                "{}/../../{OTHER_CHAT}/attachments/{}/content",
                own.parent().unwrap().parent().unwrap().display(),
                other
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
            )),
        ] {
            let error = show(&scope, &refused.to_string_lossy()).unwrap_err();
            assert!(error.contains("outside"), "{refused:?}: {error}");
        }
    }
    // A chat whose id is not a canonical UUID gets no attachment folder at all; its
    // project's folders are as before.
    for id in [
        "11111111-2222-4333-8444-55555555555A",
        "../11111111-2222-4333-8444-555555555555",
        "11111111-2222-4333-8444-555555555555/../99999999-2222-4333-8444-555555555555",
        "not-a-chat",
        "",
    ] {
        let mut info = fixture.chat(Some(ALPHA), Some("alpha-main"), &fixture.alpha, None);
        info.id = id.into();
        let scope = ImageScope::for_chat(&info, None, &fixture.state, &fixture.home, None);
        assert!(
            scope
                .roots()
                .iter()
                .all(|root| !root.starts_with(fixture.home.join("chats"))),
            "{id:?}: {:?}",
            scope.roots()
        );
        assert!(show(&scope, &own.to_string_lossy()).is_err(), "{id:?}");
        assert!(show(&scope, &other.to_string_lossy()).is_err(), "{id:?}");
        assert_eq!(show(&scope, "shot.png"), Ok(false), "{id:?}");
    }
    // A chat refused for its identity gets no attachment folder either.
    let spoof = fixture.chat(None, None, &fixture.alpha, Some(json!({"scope": "global"})));
    let refused = ImageScope::for_chat(&spoof, None, &fixture.state, &fixture.home, None);
    assert!(refused.roots().is_empty());
    assert!(show(&refused, &own.to_string_lossy()).is_err());
}
