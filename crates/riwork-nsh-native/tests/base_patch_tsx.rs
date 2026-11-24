//! The vendored `nsh-base` patch as TSX sees it: a fixture compiled by the real NSH CLI runs
//! on QuickJS with the patched `nsh/base`, headless on a Mac. The handlers the app relies on
//! get what `apps/riwork/DockArea.ts` and `SettingsFocus.tsx` declare: `onDragChange` one
//! object with `dragging`, Button's and Switch's `onFocus` / `onBlur` under the keyboard.
//! Each of them fails silently when the patch and the TSX disagree (the workspace freezes on
//! a drag and never thaws; Settings loses its focus scope), so this runs them end to end.
//! No window, home or session: `cargo test -p riwork-nsh-native --test base_patch_tsx --
//! --ignored` (needs the NSH CLI, found as `addin_build::find_nsh` does).
use std::{
    process::Command,
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use nsh_core::{
    Size,
    geometry::point,
    platform::{Idiom, Os},
    prelude::*,
};
use nsh_engine::{Bundle, EngineConfig, HostChannel};
use nsh_testing::Headless;
use serde_json::{Value, json};

const APP: &str = r#"
import { Text, View, host, mount } from "nsh";
import { Button, DockArea, Switch } from "nsh/base";

// The CLI's stock declarations lack the patch's props; nothing here is type checked.
const Dock = DockArea as any, FocusButton = Button as any, FocusSwitch = Switch as any;
const say = (event: string, data: unknown = null) => host.post("probe", { event, data });
const layout = { center: { type: "split", axis: "horizontal", sizes: [400, null], children: [
  { type: "tabs", panels: ["a", "b"], activeIndex: 0 },
  { type: "tabs", panels: ["c"], activeIndex: 0 },
] } };

mount(() => <View style={{ width: 800, height: 600 }}>
  <FocusButton onFocus={() => say("button focus")} onBlur={() => say("button blur")}><Text>Button</Text></FocusButton>
  <FocusSwitch checked={false} onFocus={() => say("switch focus")} onBlur={() => say("switch blur")}><Text>Switch</Text></FocusSwitch>
  <Dock style={{ flex: 1 }} defaultLayout={layout} compact={false} dividerSize={5} tabBarClass="flex-row h-6"
    renderTab={(tab: any) => <View testID={`tab-${tab.panel}`} style={{ width: 80, height: 24 }}><Text>{`Tab ${tab.panel}`}</Text></View>}
    renderPanel={(panel: any) => <View style={{ flex: 1 }}><Text>{`Panel ${panel.id}`}</Text></View>}
    onDragChange={(event: unknown) => say("drag", event)} />
</View>);
"#;

/// What the fixture posted since the last call, once it has posted `count` events.
fn posted(events: &Receiver<Value>, count: usize) -> Vec<Value> {
    let mut seen = Vec::new();
    while seen.len() < count {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(event) => seen.push(event),
            Err(_) => break,
        }
    }
    // Anything more than was expected shows too.
    seen.extend(events.try_iter());
    seen
}

#[test]
#[ignore = "runs the NSH CLI"]
fn tsx_handlers_get_the_patched_dock_and_focus_events() {
    let root = tempfile::tempdir().unwrap();
    let app_dir = root.path().join("app");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(app_dir.join("App.tsx"), APP).unwrap();
    let config = json!({"name": "Base patch probe", "id": "dev.riwork.test.base-patch",
        "capabilities": {"host": ["probe"]}});
    std::fs::write(app_dir.join("nsh.json"), config.to_string()).unwrap();
    let nsh = riwork_engine::addin_build::find_nsh(&root.path().join("home")).unwrap();
    let out = root.path().join("out");
    let build = Command::new(&nsh)
        .arg("build")
        .arg(&app_dir)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let host = HostChannel::new();
    let (tx, events) = mpsc::channel();
    host.handle("probe", move |mut call| {
        let _ = tx.send(call.take_data());
    });
    let mut config = EngineConfig {
        viewport: Size::new(px(800.0), px(600.0)),
        ..EngineConfig::new(Bundle::read(&out.join("bundle.js")).unwrap())
    };
    config.ui_sets = vec![nsh_base::registry()];
    config.host = host;
    config.environment.os = Os::MacOs;
    config.environment.idiom = Idiom::Desktop;
    let mut app = Headless::start(config).expect("the engine starts");
    app.wait_for("Tab a").unwrap();
    assert!(posted(&events, 0).is_empty());

    // From the middle of tab a into the middle of c's group: it merges there.
    app.drag("id:tab-a", point(px(560.0), px(288.0))).unwrap();
    assert_eq!(
        posted(&events, 2),
        [
            json!({"event": "drag", "data": {"dragging": true}}),
            json!({"event": "drag", "data": {"dragging": false}}),
        ]
    );

    // The button and the switch come first in the Tab order.
    app.press_key("tab").unwrap();
    assert_eq!(posted(&events, 1), [json!({"event": "button focus", "data": null})]);
    app.press_key("tab").unwrap();
    assert_eq!(
        posted(&events, 2),
        [
            json!({"event": "button blur", "data": null}),
            json!({"event": "switch focus", "data": null}),
        ]
    );
    app.press_key("tab").unwrap();
    assert_eq!(posted(&events, 1), [json!({"event": "switch blur", "data": null})]);
}
