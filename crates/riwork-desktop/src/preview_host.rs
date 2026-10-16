//! Bridges explicit coding-shell requests to project-window preview entities.

use super::*;
use riwork_engine::preview_control::{ControlError, PreviewAction, PreviewControl, PreviewReply};

#[derive(Clone)]
pub(crate) struct PreviewHost {
    pub store: PreviewControl,
    pub id: String,
}

impl PreviewHost {
    pub fn open(home: &Path) -> Result<Self, ControlError> {
        Ok(Self {
            store: PreviewControl::open(home)?,
            id: uuid::Uuid::new_v4().to_string(),
        })
    }
}

/// IDs name one view lifetime. A delayed publisher or close must never act on
/// another instance that happens to restore the same saved configuration.
pub(crate) fn instance_id(saved: String) -> String {
    static USED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut used = USED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved = uuid::Uuid::parse_str(&saved)
        .map(|id| id.to_string())
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    if used.insert(saved.clone()) {
        return saved;
    }
    let fresh = uuid::Uuid::new_v4().to_string();
    used.insert(fresh.clone());
    fresh
}

impl Workspace {
    pub(crate) fn start_preview_host(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.preview_host.clone() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let project = match this.update(cx, |workspace, _| workspace.project_id.clone()) {
                    Ok(project) => project,
                    Err(_) => break,
                };
                let reader = host.clone();
                let read_project = project.clone();
                let work = cx.background_executor().spawn(async move { reader.store.take_requests(&reader.id, &read_project) });
                match work.await {
                    Ok(requests) => {
                        for request in requests {
                            let result = this.update_in(cx, |workspace, window, cx| {
                                if workspace.project_id != project {
                                    return Err(ControlError::new("project_changed", "The project window changed before accepting this request."));
                                }
                                workspace.preview_request(request.action, window, cx)
                            }).unwrap_or_else(|_| Err(ControlError::new("host_unavailable", "The project window closed.")));
                            let writer = host.clone();
                            let _ = cx.background_executor().spawn(async move { writer.store.respond(&request.id, result) }).await;
                        }
                    }
                    Err(error) => {
                        let _ = this.update(cx, |workspace, cx| {
                            let message = format!("Preview control is unavailable: {error}");
                            if workspace.notice.as_ref() != Some(&message) { workspace.notice = Some(message); cx.notify(); }
                        });
                    }
                }
                cx.background_executor().timer(Duration::from_millis(250)).await;
            }
            let _ = host.store.remove_host(&host.id);
        }).detach();
    }

    pub(crate) fn preview_request(
        &mut self,
        action: PreviewAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PreviewReply, ControlError> {
        match action {
            PreviewAction::Open {
                project_id,
                spec,
                background,
            } => {
                if project_id != self.project_id || self.is_remote() {
                    return Err(ControlError::new(
                        "wrong_project",
                        "Open this local project in RiWork before creating its preview.",
                    ));
                }
                if !self.ensure_layout(window, cx) {
                    return Err(ControlError::new(
                        "not_ready",
                        "The project layout is not ready.",
                    ));
                }
                let existing = self.panes.iter().find_map(|(pane_id, pane)| {
                    pane.tabs.iter().find_map(|tab| {
                        if let TabContent::AppPreview {
                            preview_id,
                            spec: current,
                            view,
                        } = &tab.content
                            && current == &spec
                        {
                            Some((*pane_id, tab.id, preview_id.clone(), view.clone()))
                        } else {
                            None
                        }
                    })
                });
                let (pane_id, tab_id, preview_id, view, reused) =
                    if let Some((pane, tab, id, view)) = existing {
                        (pane, tab, id, view, true)
                    } else {
                        let pane_id = self.new_tab_pane();
                        let tab = self.app_preview_tab(
                            uuid::Uuid::new_v4().to_string(),
                            spec,
                            window,
                            cx,
                        );
                        let tab_id = tab.id;
                        let TabContent::AppPreview {
                            preview_id, view, ..
                        } = &tab.content
                        else {
                            unreachable!()
                        };
                        let (preview_id, view) = (preview_id.clone(), view.clone());
                        let pane = self.panes.get_mut(&pane_id).ok_or_else(|| {
                            ControlError::new("not_ready", "The destination pane no longer exists.")
                        })?;
                        // Selection is explicit below; background opens preserve focus and selection.
                        append_background_tab(pane, tab);
                        (pane_id, tab_id, preview_id, view, false)
                    };
                view.update(cx, |view, cx| view.start_controlled(cx))
                    .map_err(|error| ControlError::new("launch_failed", error))?;
                if !background {
                    self.select_tab(pane_id, tab_id, window, cx);
                }
                self.save_layout();
                cx.notify();
                Ok(PreviewReply { preview_id, reused })
            }
            PreviewAction::Close { preview_id } => {
                let Some((pane_id, index, _)) = self.find_preview(&preview_id) else {
                    return Err(ControlError::new(
                        "not_found",
                        "This preview tab is not open in its owning window.",
                    ));
                };
                if let (Some(host), TabContent::AppPreview { spec, .. }) = (
                    &self.preview_host,
                    &self.panes[&pane_id].tabs[index].content,
                ) {
                    host.store
                        .closed(&preview_id, &host.id, &self.project_id, spec)?;
                }
                let id = self.panes[&pane_id].tabs[index].id;
                self.remove_tab(pane_id, id, window, cx);
                Ok(PreviewReply {
                    preview_id,
                    reused: true,
                })
            }
            PreviewAction::Input { preview_id, input } => {
                let (_, _, view) = self.find_preview(&preview_id).ok_or_else(|| {
                    ControlError::new(
                        "not_found",
                        "This preview tab is not open in its owning window.",
                    )
                })?;
                view.update(cx, |view, _| view.input_controlled(input))
                    .map_err(|message| ControlError::new("not_ready", message))?;
                Ok(PreviewReply {
                    preview_id,
                    reused: true,
                })
            }
            PreviewAction::Refresh { preview_id } => {
                let (_, _, view) = self.find_preview(&preview_id).ok_or_else(|| {
                    ControlError::new(
                        "not_found",
                        "This preview tab is not open in its owning window.",
                    )
                })?;
                view.update(cx, |view, _| view.refresh_controlled())
                    .map_err(|message| ControlError::new("not_ready", message))?;
                Ok(PreviewReply {
                    preview_id,
                    reused: true,
                })
            }
        }
    }

    fn find_preview(&self, preview_id: &str) -> Option<(PaneId, usize, Entity<AppPreviewView>)> {
        self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs.iter().enumerate().find_map(|(index, tab)| {
                if let TabContent::AppPreview {
                    preview_id: id,
                    view,
                    ..
                } = &tab.content
                    && id == preview_id
                {
                    Some((*pane_id, index, view.clone()))
                } else {
                    None
                }
            })
        })
    }
}
