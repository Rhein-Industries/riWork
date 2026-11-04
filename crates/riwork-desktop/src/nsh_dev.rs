//! Opt-in development connection. The pinned embedding API reloads a whole
//! view; it cannot apply NSH's private state-preserving component patches.
use nsh_dev::{
    DevClient, DevEvent, HostInfo,
    protocol::{BundleFormat, HostPlatform},
};
use nsh_platform_macos::Bundle;
use riwork_engine::runtime::RuntimeWindow;
use serde_json::{Value, json};

pub const ENV: &str = "RIWORK_NSH_DEV_URL";

pub struct Build {
    pub session: u64,
    pub version: u64,
    pub bundle: Bundle,
}

/// Keep only the newest build while a previous one is flushing or draining.
/// A failed flush leaves its existing view alive and permits the next build.
#[derive(Default)]
pub struct Reloads {
    pending: Option<Build>,
    pub active: bool,
}
impl Reloads {
    pub fn push(&mut self, build: Build) {
        self.pending = Some(build);
    }
    pub fn next(&mut self, blocked: bool) -> Option<Build> {
        if self.active || blocked {
            return None;
        }
        let build = self.pending.take()?;
        self.active = true;
        Some(build)
    }
}

/// Development source is executable UI code. Only an explicit local server
/// is accepted; normal builds do not contain the dev client at all.
pub fn asset_base(url: &str) -> Result<String, String> {
    let port = url
        .strip_prefix("ws://127.0.0.1:")
        .and_then(|rest| rest.strip_suffix("/ws"))
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("{ENV} must be ws://127.0.0.1:PORT/ws"))?;
    Ok(format!("http://127.0.0.1:{port}/assets/"))
}

pub fn start(
    url: String,
    mut receive: impl FnMut(Result<Build, String>) + Send + 'static,
) -> Result<DevClient, String> {
    asset_base(&url)?;
    DevClient::start(
        url,
        HostInfo {
            device: format!("Somtus development ({})", std::process::id()),
            platform: HostPlatform::MacOs,
            bytecode_version: "riwork-source-only".into(),
            ui_sets: vec!["nsh/base".into(), "riwork/native".into()],
            tests: false,
        },
        move |event| match event {
            DevEvent::Bundle {
                session,
                version,
                format,
                bytes,
                source_map,
                ..
            } => {
                let build = (|| {
                    if format != BundleFormat::Source {
                        return Err("NSH dev server did not provide JavaScript source".into());
                    }
                    let source = String::from_utf8(bytes).map_err(|error| error.to_string())?;
                    // Use the same app identity/capabilities as packaged RiWork.
                    let mut bundle = Bundle::source(source)
                        .with_app_id(Some(super::nsh_host::APP_ID))
                        .with_capabilities_json(Some(super::nsh_host::CAPABILITIES))?;
                    if let Some(map) = source_map {
                        bundle = bundle.with_source_map(map);
                    }
                    Ok(Build {
                        session,
                        version,
                        bundle,
                    })
                })();
                receive(build);
            }
            DevEvent::BuildFailed(message) | DevEvent::CannotRun { message, .. } => {
                receive(Err(message));
            }
            // Each patch is followed by the full bundle. Public AppView APIs
            // support the latter only; never evaluate a patch as a new app.
            _ => {}
        },
    )
    .map_err(|error| format!("Cannot start NSH dev client: {error}"))
}

pub fn restore_params(mut params: Value, window: &RuntimeWindow) -> Value {
    params["projectId"] = json!(window.project_id);
    params["path"] = json!(window.path);
    params["restore"] = json!({
        "layout": window.layout,
        "focusMode": window.focus_mode,
        "focusCentered": window.focus_centered,
        "global": window.global,
    });
    if let Some(global) = &window.global {
        // An initial "start global chat" intent must not run a second time.
        params["global"] = json!({"view": global.view.name(), "start": false});
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;
    fn build(version: u64) -> Build {
        Build {
            session: 1,
            version,
            bundle: Bundle::source(String::new()),
        }
    }
    #[test]
    fn edits_during_a_reload_coalesce_and_a_failed_flush_can_retry() {
        let mut reloads = Reloads::default();
        reloads.push(build(1));
        assert!(reloads.next(true).is_none());
        assert_eq!(reloads.next(false).unwrap().version, 1);
        reloads.push(build(2));
        reloads.push(build(3));
        assert!(reloads.next(false).is_none());
        reloads.active = false;
        assert_eq!(reloads.next(false).unwrap().version, 3);
        reloads.active = false;
        assert!(reloads.next(false).is_none());
    }
    #[test]
    fn only_explicit_loopback_dev_servers_are_accepted() {
        assert_eq!(
            asset_base("ws://127.0.0.1:8790/ws").unwrap(),
            "http://127.0.0.1:8790/assets/"
        );
        for url in [
            "ws://example.com:8790/ws",
            "ws://127.0.0.1:0/ws",
            "ws://127.0.0.1:8790/ws?x",
            "ws://127.0.0.1:8790/other",
        ] {
            assert!(asset_base(url).is_err(), "{url}");
        }
    }
    #[test]
    fn remount_keeps_window_identity_and_uses_current_project_and_layout() {
        use riwork_engine::runtime::{WindowKind, WindowMode};
        let window = RuntimeWindow {
            project_id: Some("switched-project".into()),
            path: "/current".into(),
            bounds: None,
            mode: WindowMode::Windowed,
            layout: None,
            focus_mode: true,
            focus_centered: true,
            kind: WindowKind::Project,
            global: None,
        };
        let params = restore_params(
            json!({"windowId":"original", "projectId":"old", "appearance":"dark"}),
            &window,
        );
        assert_eq!(params["windowId"], "original");
        assert_eq!(params["projectId"], "switched-project");
        assert_eq!(params["path"], "/current");
        assert_eq!(params["appearance"], "dark");
        assert_eq!(params["restore"]["focusMode"], true);
        assert_eq!(params["restore"]["focusCentered"], true);
    }
}
