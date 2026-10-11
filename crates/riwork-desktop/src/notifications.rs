//! GPUI delivery for the engine's durable project completion ledger.
#[cfg(test)]
use crate::sessions::HarnessKind;
use crate::store::Project;
use gpui::{App, SystemNotification, SystemNotificationAction};
pub use riwork_engine::notifications::*;
#[cfg(test)]
use std::path::Path;
use std::{path::PathBuf, time::Duration};

trait NativeCompletionNotice {
    fn notification(&self) -> SystemNotification;
}
impl NativeCompletionNotice for CompletionNotice {
    fn notification(&self) -> SystemNotification {
        let presentation = self.presentation();
        SystemNotification {
            tag: presentation.tag.into(),
            title: presentation.title.into(),
            body: presentation.body.into(),
            actions: presentation
                .actions
                .into_iter()
                .map(|action| SystemNotificationAction {
                    id: action.id.into(),
                    label: action.label.into(),
                })
                .collect(),
        }
    }
}

pub fn show_enabled(project: &Project, cx: &App) {
    // On macOS this first user-initiated post asks for notification permission.
    // Subsequent completions use the same RiWork app-bundle notification center.
    cx.show_system_notification(SystemNotification {
        tag: format!("riwork-notifications-enabled:{}", project.id).into(),
        title: compact_label(&project.name).into(),
        body: "Agent completion notifications enabled.".into(),
        actions: Vec::new(),
    });
}

pub fn start(home: PathBuf, cx: &mut App) {
    if !std::env::current_exe().is_ok_and(|executable| in_app_bundle(&executable)) {
        return;
    }
    cx.spawn(async move |cx| {
        let mut last_error = None;
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let task_home = home.clone();
            let work = cx
                .background_executor()
                .spawn(async move { claim_pending(&task_home) });
            match work.await {
                Ok(notices) => {
                    last_error = None;
                    cx.update(|cx| {
                        for notice in notices {
                            cx.show_system_notification(notice.notification());
                        }
                    });
                }
                Err(error) => {
                    if last_error.as_ref() != Some(&error) {
                        eprintln!("riwork notifications: {error}");
                        last_error = Some(error);
                    }
                }
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    // The ledger's opt-in/claim test stays in engine; its native conversion assertions
    // stay here because SystemNotification belongs to GPUI.
    #[test]
    fn completion_notice_routes_the_original_open_action() {
        let notice = CompletionNotice {
            completion_id: "3fb67c82-81c2-4a4c-94bb-2f134d35f85d".into(),
            project_id: "15987468-87e5-412a-a098-2f5a66a9d102".into(),
            shell_id: "10e459c4-0680-4b11-aec2-6e2a6cd0e33d".into(),
            project_name: "Fixture".into(),
            worktree: None,
            provider: HarnessKind::Codex,
        };
        let alert = notice.notification();
        assert_eq!(
            response_target(&alert.tag),
            Some((notice.project_id.as_str(), notice.shell_id.as_str()))
        );
        assert_eq!(alert.actions[0].id.as_ref(), "open");
    }

    #[test]
    fn only_an_app_bundle_process_claims_alerts() {
        assert!(in_app_bundle(Path::new(
            "/Applications/RiWork.app/Contents/MacOS/riwork"
        )));
        assert!(in_app_bundle(Path::new(
            "/tmp/target/debug/RiWork.app/Contents/MacOS/riwork"
        )));
        assert!(!in_app_bundle(Path::new("/tmp/target/debug/riwork")));
        assert!(!in_app_bundle(Path::new(
            "/tmp/target/debug/RiWork.app/riwork"
        )));
        assert!(!in_app_bundle(Path::new("/tmp/Contents/MacOS/riwork")));
        assert!(!in_app_bundle(Path::new("riwork")));
    }
}
