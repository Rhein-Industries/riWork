//! RiWork's guard on Cua input to its own live windows.
//!
//! An agent that drives the RiWork app it runs under changes what the user is working in:
//! an orchestrator once tested a feature in its own chat window, changing its layout,
//! opening a destructive dialog and typing into its find bar. So `riwork cua mcp` refuses,
//! by default, the tool calls that would send input to a live RiWork app, and passes every
//! other call through untouched:
//!
//! - Read-only tools (window state, screenshots, lists, health) always pass.
//! - A call that names a live RiWork app's pid (`pid` or `target.pid`) is refused, whatever
//!   the tool.
//! - While a RiWork app runs, an input tool that is not aimed at one app's window is refused
//!   too: desktop-scope and frontmost-app input, a bare element token and a replayed
//!   trajectory could all land in RiWork. Naming the intended app's `pid` lets it through.
//! - Launching RiWork itself is refused while one runs.
//!
//! Live means registered in the user's own runtime registry (`runtime::default_directory`)
//! and running. A fixture app started with its own `RIWORK_RUNTIME_DIR` (and `RIWORK_HOME`)
//! registers elsewhere and is not protected, so isolated UI checks keep working. Every
//! window of a live app is protected: all of them, terminal panes included, belong to its
//! one process, any of them can show an orchestrator, and a tool call names no window the
//! registry records.
//!
//! `RIWORK_CUA_ALLOW_RIWORK_INPUT=1` in the proxy's environment turns the guard off for that
//! proxy. The harnesses filter their MCP servers' environments, so a terminal session RiWork
//! launches has it forwarded, on its own pane, when it is set where the session is launched;
//! the tmux server and the chat host, which outlive any one launch, never keep it, so chats
//! (orchestrators included) are always guarded. Nothing the agent does later reaches the
//! proxy already serving it. This is a safety net against accidents, not a sandbox: the
//! `cua-driver` CLI, run directly, does not pass through it.

use serde_json::Value;

/// The variable that, set to `1` in the environment of `riwork cua mcp`, turns the guard off.
pub const ALLOW_RIWORK_INPUT: &str = "RIWORK_CUA_ALLOW_RIWORK_INPUT";

/// The bundle identifier `scripts/bundle-macos.sh` gives RiWork.
const RIWORK_BUNDLE_ID: &str = "dev.riwork.shell";

/// Whether this process's environment turns the guard off.
pub fn riwork_input_allowed() -> bool {
    allows_riwork_input(std::env::var_os(ALLOW_RIWORK_INPUT).as_deref())
}

/// Only exactly `1` turns the guard off.
fn allows_riwork_input(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

/// Tools that only read, which pass whatever they name.
const READ_ONLY: &[&str] = &[
    "check_permissions",
    "clipboard_read",
    "get_accessibility_tree",
    "get_agent_cursor_state",
    "get_browser_state",
    "get_config",
    "get_cursor_position",
    "get_desktop_state",
    "get_recording_state",
    "get_screen_size",
    "get_session",
    "get_session_state",
    "get_window_state",
    "health_report",
    "history_query",
    "history_status",
    "list_apps",
    "list_sessions",
    "list_windows",
    "parse_visual_regions",
    "verify_state",
    "zoom",
];

/// Tools that send input to an app's windows, the frontmost app or the desktop. While RiWork
/// runs, a call of one of these must name the process it means. The browser tools are not
/// among them: they drive pages over the DevTools protocol, which RiWork's windows do not
/// serve. `move_cursor` counts only when it moves the real pointer (`moves_real_pointer`).
const INPUT: &[&str] = &[
    "bring_to_front",
    "click",
    "double_click",
    "drag",
    "hotkey",
    "invoke_menu",
    "kill_app",
    "press_key",
    "replay_trajectory",
    "right_click",
    "scroll",
    "set_value",
    "set_window_frame",
    "type_text",
];

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allow,
    Refuse(String),
}

/// Whether the call of `tool` with `arguments` may reach the driver. `live_apps` gives the
/// pids of the user's running RiWork apps; it is asked only for a call that is not read-only.
pub(crate) fn verdict(
    tool: &str,
    arguments: &Value,
    live_apps: impl FnOnce() -> Vec<u32>,
) -> Verdict {
    if READ_ONLY.contains(&tool) {
        return Verdict::Allow;
    }
    let live = live_apps();
    if live.is_empty() {
        return Verdict::Allow;
    }
    let named = named_pids(arguments);
    if let Some(pid) = named.iter().find(|pid| live.contains(pid)) {
        return Verdict::Refuse(format!(
            "RiWork refused `{tool}`: pid {pid} is the user's running RiWork app, whose \
             windows hold their orchestrator chats and terminals and their work. Agents do not \
             send input to RiWork's own live windows (the riwork-orchestrator skill's Hard \
             boundaries); read-only tools such as get_window_state still work. Check UI in a \
             fixture RiWork with its own RIWORK_HOME and RIWORK_RUNTIME_DIR, or ask the user \
             to look. Only the user can lift this, when they start a session."
        ));
    }
    if tool == "launch_app" && launches_riwork(arguments) {
        return Verdict::Refuse(
            "RiWork refused `launch_app`: the user's RiWork app is running, and launching \
             RiWork would act on it and its live state. Start a fixture RiWork with its own \
             RIWORK_HOME and RIWORK_RUNTIME_DIR instead."
                .to_owned(),
        );
    }
    if sends_input(tool, arguments) && (named.is_empty() || desktop_scoped(arguments)) {
        return Verdict::Refuse(format!(
            "RiWork refused `{tool}`: the call is not aimed at one app's window (desktop \
             scope, the frontmost app, a bare element token or a replay), so with the user's \
             RiWork app running it could land in a RiWork window. Address the app you mean \
             by its `pid` (or a window `target`), without desktop scope; input to RiWork's \
             own windows stays refused."
        ));
    }
    Verdict::Allow
}

/// The processes a call names: its `pid` and its `target`'s.
fn named_pids(arguments: &Value) -> Vec<u32> {
    [arguments.get("pid"), arguments.pointer("/target/pid")]
        .into_iter()
        .flatten()
        .filter_map(|pid| {
            pid.as_u64()
                .or_else(|| pid.as_str()?.trim().parse().ok())
                .and_then(|pid| u32::try_from(pid).ok())
        })
        .collect()
}

fn sends_input(tool: &str, arguments: &Value) -> bool {
    if tool == "move_cursor" {
        return moves_real_pointer(arguments);
    }
    INPUT.contains(&tool)
}

/// `move_cursor` moves only the agent's cursor overlay unless it is told the desktop.
fn moves_real_pointer(arguments: &Value) -> bool {
    desktop_scoped(arguments)
}

/// Desktop scope means screen coordinates or the frontmost app, whatever `pid` says.
fn desktop_scoped(arguments: &Value) -> bool {
    arguments.get("scope").and_then(Value::as_str) == Some("desktop")
        || arguments.pointer("/target/kind").and_then(Value::as_str) == Some("desktop")
}

/// `launch_app` names its app by bundle id, or by name when it has none.
fn launches_riwork(arguments: &Value) -> bool {
    let text = |key| arguments.get(key).and_then(Value::as_str).map(str::trim);
    match text("bundle_id").filter(|id| !id.is_empty()) {
        Some(bundle_id) => bundle_id.eq_ignore_ascii_case(RIWORK_BUNDLE_ID),
        None => text("name").is_some_and(|name| {
            name.eq_ignore_ascii_case("RiWork") || name.eq_ignore_ascii_case("RiWork.app")
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A RiWork app runs as pid 4242; pid 77 is another app.
    const RIWORK: u32 = 4242;
    const OTHER: u32 = 77;

    fn live() -> Vec<u32> {
        vec![RIWORK]
    }

    fn refused(tool: &str, arguments: Value) -> String {
        match verdict(tool, &arguments, live) {
            Verdict::Refuse(reason) => reason,
            Verdict::Allow => panic!("{tool} {arguments} was allowed"),
        }
    }

    fn allowed(tool: &str, arguments: Value) -> bool {
        verdict(tool, &arguments, live) == Verdict::Allow
    }

    #[test]
    fn input_to_a_live_riwork_app_is_refused_by_pid_or_target() {
        for (tool, arguments) in [
            (
                "click",
                json!({"pid": RIWORK, "element_index": 3, "snapshot_id": "s0000abcd", "window_id": 9}),
            ),
            ("click", json!({"pid": RIWORK, "x": 10, "y": 20})),
            (
                "click",
                json!({"target": {"kind": "window", "pid": RIWORK, "window_id": 9}, "x": 1, "y": 2}),
            ),
            ("double_click", json!({"pid": RIWORK, "element_index": 1})),
            ("right_click", json!({"pid": RIWORK, "element_index": 1})),
            ("type_text", json!({"pid": RIWORK, "text": "nuke"})),
            ("press_key", json!({"pid": RIWORK, "key": "return"})),
            ("hotkey", json!({"pid": RIWORK, "keys": ["cmd", "f"]})),
            (
                "drag",
                json!({"pid": RIWORK, "from_x": 1, "from_y": 1, "to_x": 9, "to_y": 9}),
            ),
            ("scroll", json!({"pid": RIWORK, "direction": "down"})),
            (
                "set_value",
                json!({"pid": RIWORK, "element_index": 2, "value": "x"}),
            ),
            (
                "invoke_menu",
                json!({"pid": RIWORK, "path": ["Window", "Zoom"]}),
            ),
            ("bring_to_front", json!({"pid": RIWORK})),
            (
                "set_window_frame",
                json!({"pid": RIWORK, "window_id": 9, "x": 0, "y": 0}),
            ),
            ("kill_app", json!({"pid": RIWORK})),
            (
                "move_cursor",
                json!({"target": {"kind": "window", "pid": RIWORK, "window_id": 9}, "x": 1, "y": 1}),
            ),
            // A pid as text is still that pid.
            ("click", json!({"pid": RIWORK.to_string(), "x": 1, "y": 1})),
            // A tool this list does not know, aimed at RiWork.
            ("some_future_input", json!({"pid": RIWORK})),
        ] {
            let reason = refused(tool, arguments);
            assert!(reason.contains(&format!("`{tool}`")), "{reason}");
            assert!(reason.contains(&format!("pid {RIWORK}")), "{reason}");
            assert!(reason.contains("Hard boundaries"), "{reason}");
            assert!(!reason.contains(ALLOW_RIWORK_INPUT), "{reason}");
        }
    }

    #[test]
    fn read_only_tools_pass_even_against_riwork() {
        for tool in READ_ONLY {
            assert!(
                allowed(tool, json!({"pid": RIWORK, "window_id": 9})),
                "{tool}"
            );
        }
        // Without asking for live apps at all.
        assert_eq!(
            verdict("get_window_state", &json!({"pid": RIWORK}), || {
                panic!("read-only calls need no registry")
            }),
            Verdict::Allow
        );
    }

    #[test]
    fn input_to_other_apps_passes() {
        for (tool, arguments) in [
            ("click", json!({"pid": OTHER, "element_token": "t-1"})),
            ("type_text", json!({"pid": OTHER, "text": "hello"})),
            (
                "press_key",
                json!({"target": {"kind": "window", "pid": OTHER, "window_id": 3}, "key": "tab"}),
            ),
            ("kill_app", json!({"pid": OTHER})),
            ("launch_app", json!({"bundle_id": "com.apple.calculator"})),
            ("launch_app", json!({"name": "Calculator"})),
            // Moving the agent's own cursor overlay is no input.
            ("move_cursor", json!({"x": 5, "y": 5})),
            // Browser tools drive pages over CDP, which RiWork windows do not serve.
            (
                "browser_click",
                json!({"target_id": "t", "tab_id": "b", "session": "s", "ref": "r"}),
            ),
            // Session and settings tools name no window.
            ("start_session", json!({"session": "s"})),
            ("clipboard_write", json!({"text": "x"})),
        ] {
            assert!(allowed(tool, arguments.clone()), "{tool} {arguments}");
        }
    }

    #[test]
    fn untargeted_input_is_refused_while_riwork_runs() {
        for (tool, arguments) in [
            ("type_text", json!({"scope": "desktop", "text": "x"})),
            ("type_text", json!({"text": "into the frontmost app"})),
            ("press_key", json!({"key": "escape"})),
            ("hotkey", json!({"keys": ["cmd", "w"]})),
            ("click", json!({"scope": "desktop", "x": 100, "y": 100})),
            (
                "click",
                json!({"target": {"kind": "desktop", "display_id": "primary"}, "x": 1, "y": 1}),
            ),
            // An element token carries its pid out of sight.
            ("click", json!({"element_token": "opaque"})),
            (
                "drag",
                json!({"scope": "desktop", "from_x": 1, "from_y": 1, "to_x": 2, "to_y": 2}),
            ),
            ("scroll", json!({"scope": "desktop", "direction": "up"})),
            ("move_cursor", json!({"scope": "desktop", "x": 1, "y": 1})),
            (
                "move_cursor",
                json!({"target": {"kind": "desktop", "display_id": "primary"}, "x": 1, "y": 1}),
            ),
            ("replay_trajectory", json!({"dir": "~/trajectory"})),
            // Desktop scope acts on screen coordinates or the frontmost app, whatever pid
            // rides along.
            (
                "click",
                json!({"scope": "desktop", "pid": OTHER, "x": 100, "y": 100}),
            ),
            (
                "type_text",
                json!({"target": {"kind": "desktop", "display_id": "primary"}, "pid": OTHER, "text": "x"}),
            ),
        ] {
            let reason = refused(tool, arguments);
            assert!(reason.contains("not aimed at one app's window"), "{reason}");
            assert!(reason.contains("`pid`"), "{reason}");
        }
    }

    #[test]
    fn launching_riwork_while_it_runs_is_refused() {
        for arguments in [
            json!({"bundle_id": "dev.riwork.shell"}),
            json!({"bundle_id": "DEV.RIWORK.SHELL", "creates_new_application_instance": true}),
            json!({"name": "RiWork"}),
            json!({"name": " riwork.app "}),
        ] {
            assert!(refused("launch_app", arguments).contains("fixture RiWork"));
        }
        // A bundle id wins over the name, as the driver reads them.
        assert!(allowed(
            "launch_app",
            json!({"bundle_id": "com.apple.calculator", "name": "RiWork"})
        ));
    }

    #[test]
    fn nothing_is_refused_while_no_riwork_app_runs() {
        for (tool, arguments) in [
            ("click", json!({"pid": RIWORK, "x": 1, "y": 1})),
            ("type_text", json!({"scope": "desktop", "text": "x"})),
            ("launch_app", json!({"bundle_id": "dev.riwork.shell"})),
        ] {
            assert_eq!(
                verdict(tool, &arguments, Vec::new),
                Verdict::Allow,
                "{tool}"
            );
        }
    }

    #[test]
    fn only_exactly_one_turns_the_guard_off() {
        use std::ffi::OsStr;
        assert!(allows_riwork_input(Some(OsStr::new("1"))));
        for value in ["0", "true", "yes", " 1", ""] {
            assert!(!allows_riwork_input(Some(OsStr::new(value))), "{value:?}");
        }
        assert!(!allows_riwork_input(None));
    }
}
