//! Acceptance-only writer; never resolves a home from the user's environment.
//! The runtime API cannot issue a restore ticket without launching a trusted
//! replacement. p1-restore therefore uses the §3.2 two-window fallback, recording
//! the exact RuntimeManager-published windows for host-unit/nsh-test restore.
#[path = "../../../apps/riwork/tests/support/fixtures.rs"]
mod fixtures;
#[path = "../../../apps/riwork/tests/support/mod.rs"]
mod legacy_support;
use riwork_core::{
    global_window::{DashboardFilter, GlobalView, GlobalWindowState, GlobalWindowStore},
    layouts::{Extent, LayoutStore, SavedTab},
    settings::SettingsStore,
    store::Store,
    theme::ThemeChoice,
};
use riwork_engine::{
    project_tabs::{Kind, Session, Status, TabStore, Update},
    runtime::{RuntimeManager, RuntimeWindow, WindowGeometry, WindowKind, WindowMode},
};
use riwork_nsh_workspace::layout::LayoutModel;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Component, PathBuf},
};

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut scenario = None;
    let mut out = None;
    let mut theme: Option<ThemeChoice> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scenario" => scenario = args.next(),
            "--out" => out = args.next().map(PathBuf::from),
            "--theme" => {
                let value = args
                    .next()
                    .ok_or("--theme requires a serialized ThemeChoice")?;
                theme = Some(
                    serde_json::from_value(json!(value))
                        .map_err(|error| format!("invalid --theme: {error}"))?,
                );
            }
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    let scenario = scenario.ok_or("--scenario p1-layouts|p1-restore|p1-basic is required")?;
    if !matches!(scenario.as_str(), "p1-layouts" | "p1-restore" | "p1-basic") {
        return Err("unknown scenario".into());
    }
    let home = out.ok_or("--out /tmp/fresh-fixture/home is required")?;
    if !home.is_absolute()
        || !(home.starts_with("/tmp") || home.starts_with("/private/tmp"))
        || home.components().any(|part| part == Component::ParentDir)
        || home.exists()
    {
        return Err("--out must be a new absolute fixture home below /tmp".into());
    }
    let parent = home.parent().ok_or("missing fixture root")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let root = parent.canonicalize().map_err(|e| e.to_string())?;
    let temp = PathBuf::from("/tmp")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !root.starts_with(temp) {
        return Err("fixture root must resolve inside /tmp".into());
    }
    let home = root.join(home.file_name().ok_or("missing home name")?);
    let store = Store::open(&home)?;
    SettingsStore::open(&home)?.update(|settings| {
        if let Some(theme) = theme {
            settings.theme = theme;
        }
    })?;
    let mut entries = Vec::new();
    let mut sessions = Vec::new();
    let mut chats = Vec::new();
    let mut windows = Vec::new();
    let mut create_errors = serde_json::Map::new();
    let aliases: std::collections::BTreeMap<String, String> = (1..=3)
        .map(|n| format!("Alpha-S{n}"))
        .chain((1..=24).map(|n| format!("Busy-S{n:02}")))
        .chain(
            [
                "Alpha-chat",
                "Alpha-worker",
                "Alpha-dead",
                "Alpha-cli",
                "Alpha-later",
                "global-orchestrator",
                "Beta-orchestrator",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .map(|name| {
            let id = fixture_uuid(&name);
            (name, id)
        })
        .collect();
    let specs: Vec<(&str, &str)> = if scenario == "p1-layouts" {
        fixtures::NAMES
            .iter()
            .map(|name| (*name, *name))
            .chain([
                ("Alpha", "L-NAV"),
                ("Beta", "L-NESTED"),
                ("Gamma", "L-EMPTY"),
                ("Busy", "L-NESTED"),
            ])
            .collect()
    } else {
        vec![
            ("Alpha", "L-NAV"),
            ("Beta", "L-NESTED"),
            ("Gamma", "L-EMPTY"),
            ("Busy", "L-NESTED"),
        ]
    };
    for (index, (name, fixture)) in specs.iter().enumerate() {
        let path = root.join("repos").join(name.to_lowercase());
        fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        let project = store.add_project(&path, Some(name))?;
        let mut loaded = if *name == "Gamma" {
            riwork_nsh_workspace::persistence::default_project_layout()
        } else {
            fixtures::seed(&home, &project.id, fixture)
        };
        if *name == "Alpha" || *name == "Busy" {
            if *name == "Busy" {
                loaded = riwork_nsh_workspace::persistence::default_project_layout();
            }
            let ids: Vec<_> = if *name == "Busy" {
                (1..=24).map(|n| format!("Busy-S{n:02}")).collect()
            } else {
                (1..=3).map(|n| format!("Alpha-S{n}")).collect()
            };
            let pane = loaded.panes.get_mut(&2).unwrap();
            pane.tabs = ids
                .iter()
                .map(|id| SavedTab::Shell {
                    shell_id: aliases[id].clone(),
                })
                .collect();
            if *name == "Alpha" {
                pane.tabs.push(SavedTab::Chat {
                    chat_id: aliases["Alpha-chat"].clone(),
                });
            }
            pane.active_tab_key = Some(format!("shell:{}", aliases[&ids[0]]));
            loaded.normalize()?;
            LayoutStore::open(&home)?.save_chat_state(&project.id, &loaded, &[], None)?;
        }
        riwork_nsh_workspace::persistence::initialize_navigation(&mut loaded)?;
        for tab in loaded.panes.values().flat_map(|pane| &pane.tabs) {
            if let SavedTab::Shell { shell_id } = tab {
                sessions.push(json!({"id":shell_id,"project_id":if shell_id == "global-orchestrator" {None} else {Some(&project.id)},
                    "kind":if shell_id == "global-orchestrator" {"orchestrator"} else {"project"},
                    "cwd":path,"alive":true,"user_opened":true,"title":aliases.iter().find_map(|(name,id)|(id==shell_id).then_some(name.strip_prefix("Busy-").unwrap_or(name))).unwrap_or(shell_id)}));
            }
            if let SavedTab::Chat { chat_id } = tab {
                let alpha_chat = chat_id == &aliases["Alpha-chat"];
                chats.push(json!({"id":chat_id,"project_id":if chat_id == "global-chat" { None } else { Some(&project.id) },"title":if alpha_chat {"Chat 2"} else {chat_id.as_str()},"status":if alpha_chat {"waiting"} else {"idle"}}));
            }
        }
        if matches!(*name, "Alpha" | "Busy") {
            let tabs = TabStore::at(&home, &project.id)?;
            let mut inventory = loaded
                .panes
                .values()
                .flat_map(|p| &p.tabs)
                .filter_map(|tab| {
                    let kind = match tab {
                        SavedTab::Shell { .. } => Kind::Shell,
                        SavedTab::Chat { .. } => Kind::Chat,
                        _ => return None,
                    };
                    let key = tab.key();
                    let alpha_chat = key == format!("chat:{}", aliases["Alpha-chat"]);
                    Some(Session {
                        key: key.clone(),
                        kind,
                        title: if alpha_chat {
                            "Chat 2".into()
                        } else {
                            aliases
                                .iter()
                                .find_map(|(name, id)| {
                                    (id == key.split_once(':').unwrap().1).then_some(
                                        name.strip_prefix("Busy-").unwrap_or(name).to_owned(),
                                    )
                                })
                                .unwrap_or_else(|| key.clone())
                        },
                        title_priority: 0,
                        parent_id: None,
                        status: if alpha_chat {
                            Status::Waiting
                        } else {
                            Status::Idle
                        },
                        orchestrator: false,
                        worker: false,
                        created: 1,
                    })
                })
                .collect::<Vec<_>>();
            if *name == "Alpha" {
                inventory.push(Session {
                    key: format!("shell:{}", aliases["Alpha-worker"]),
                    kind: Kind::Shell,
                    title: "Alpha worker".into(),
                    title_priority: 0,
                    parent_id: Some(aliases["Alpha-S1"].clone()),
                    status: Status::Working,
                    orchestrator: false,
                    worker: true,
                    created: 2,
                });
                sessions.push(json!({"id":aliases["Alpha-worker"],"project_id":project.id,"kind":"project","cwd":path,"alive":true,"user_opened":false,"parent_id":aliases["Alpha-S1"],"title":"Alpha worker","status":"working"}));
                sessions.push(json!({"id":aliases["Alpha-dead"],"project_id":project.id,"kind":"project","cwd":path,"alive":false,"user_opened":true,"title":"Ended"}));
                sessions.push(json!({"id":aliases["Alpha-cli"],"project_id":project.id,"kind":"project","cwd":path,"alive":true,"user_opened":true,"title":"CLI shell"}));
                sessions.push(json!({"id":aliases["Alpha-later"],"project_id":project.id,"kind":"project","cwd":path,"alive":true,"user_opened":true,"title":"Later shell","appear_after_ms":300}));
                sessions.push(json!({"id":aliases["global-orchestrator"],"project_id":null,"kind":"orchestrator","cwd":path,"alive":true,"user_opened":true,"title":"Global orchestrator"}));
            }
            tabs.reconcile(&inventory)?;
            for session in &inventory {
                tabs.update(&Update::Unhide {
                    key: session.key.clone(),
                })?;
            }
            // Reconciliation discovers metadata in identity order. Explicit
            // membership moves establish the fixture's drawn order.
            for session in &inventory {
                tabs.update(&Update::Move {
                    key: session.key.clone(),
                    before: None,
                })?;
            }
            if *name == "Alpha" {
                tabs.update(&Update::Hide {
                    key: format!("shell:{}", aliases["Alpha-worker"]),
                })?;
            }
        }
        if *name == "Beta" {
            create_errors.insert(project.id.clone(), json!("Fixture shell creation failed"));
            fs::create_dir_all(home.join("project-tabs")).map_err(|e| e.to_string())?;
            // Deliberately damaged fixture (same unreadable-JSON form as core).
            fs::write(
                home.join("project-tabs")
                    .join(format!("{}.json", project.id)),
                b"{",
            )
            .map_err(|e| e.to_string())?;
            sessions.push(json!({"id":aliases["Beta-orchestrator"],"project_id":project.id,"kind":"orchestrator","cwd":path,"alive":true,"user_opened":true,"title":"Beta orchestrator"}));
        }
        let saved = loaded.clone();
        if *fixture == "L-GLOBAL-TABS" {
            riwork_nsh_workspace::persistence::prune_global_tabs(
                &mut loaded,
                &["global-chat".to_owned()].into_iter().collect(),
                &["global-orchestrator".to_owned()].into_iter().collect(),
            );
        }
        let mut model = LayoutModel::new(loaded.clone())?;
        if *fixture == "L-UNKNOWN" {
            model.mark_unknown_panes(&[2].into_iter().collect());
        }
        let projection = model.dock(Extent {
            width: 1280.0,
            height: 800.0,
        });
        entries.push(json!({"name":name,"projectId":project.id,"home":home,"root":path,
            "expected":loaded,"saved":saved,"projection":projection,"goldenProjection":serde_json::to_string_pretty(&projection).unwrap(),
            "tabs":loaded.panes.values().flat_map(|p| p.tabs.iter().map(SavedTab::key)).collect::<Vec<_>>(),
            "activeTabs":loaded.panes.values().filter_map(|p|p.active_tab_key.clone()).collect::<Vec<_>>() }));
        if scenario == "p1-restore" && index < 2 {
            windows.push(RuntimeWindow {
                project_id: Some(project.id),
                path,
                bounds: Some(WindowGeometry {
                    x: 100.0 + index as f32 * 22.0,
                    y: 100.0 + index as f32 * 22.0,
                    width: 1000.0,
                    height: 700.0,
                }),
                mode: if index == 1 {
                    WindowMode::Maximized
                } else {
                    WindowMode::Windowed
                },
                layout: Some(loaded),
                focus_mode: index == 1,
                focus_centered: false,
                kind: WindowKind::Project,
                global: None,
            });
        }
    }
    let mut damaged = Vec::new();
    if scenario == "p1-layouts" {
        for name in ["D-NEWER", "D-CORRUPT", "D-ENTRY"] {
            let bad_home = root.join("damaged").join(name).join("home");
            let bad_root = root.join("repos").join(name.to_lowercase());
            fs::create_dir_all(&bad_root).map_err(|e| e.to_string())?;
            let project = Store::open(&bad_home)?.add_project(&bad_root, Some(name))?;
            fixtures::damaged(&bad_home, &project.id, name);
            damaged
                .push(json!({"name":name,"home":bad_home,"root":bad_root,"projectId":project.id}));
        }
    }
    let runtime = if scenario == "p1-restore" {
        let manager = RuntimeManager::at(root.join("run"))?;
        let mut registration = manager.register(home.clone())?;
        registration.publish_windows(windows.clone())?;
        let published = manager
            .instances()?
            .into_iter()
            .find(|i| i.pid == std::process::id())
            .ok_or("published runtime missing")?;
        // An evidence artifact from the public API, not a fabricated restore ticket.
        fs::write(
            root.join("run/p1-restore-windows.json"),
            serde_json::to_vec_pretty(&published.windows).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        let mut global = GlobalWindowState {
            view: GlobalView::Chat,
            ..Default::default()
        };
        global.set_filter(DashboardFilter::NeedsYou);
        GlobalWindowStore::at(&home).save(&global)?;
        json!({"ticket":null,"fallback":"RuntimeManager requires a live requester and trusted replacement launch; two project windows only (§3.2)","windows":published.windows})
    } else {
        Value::Null
    };
    let fake = json!({"sessions":sessions,"chats":chats,"global_chat_ids":["global-chat"],"create_errors":create_errors,"close_calls":[]});
    fs::write(
        home.join("fake-sessions.json"),
        serde_json::to_vec_pretty(&fake).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    legacy_support::seed(&home, true);
    let manifest = json!({"scenario":scenario,"home":home,"aliases":aliases,"fixtures":entries,"damaged":damaged,"runtime":runtime});
    fs::write(
        home.join("fixtures.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({"scenario": scenario,"home":home,"manifest":home.join("fixtures.json"),"fixtureCount":manifest["fixtures"].as_array().unwrap().len()})
    );
    Ok(())
}

fn fixture_uuid(label: &str) -> String {
    let hash = label.bytes().fold(2166136261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16777619)
    });
    format!("10000000-0000-4000-8000-{hash:012x}")
}
