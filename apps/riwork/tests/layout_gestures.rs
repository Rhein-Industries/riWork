//! Real `Headless` DockArea output crosses the production layout-model seam.
//! Every callback is counted and installs only the model's returned projection.
//! Rust-rendered content is rebuilt with split containers; JS's keyed renderer
//! cache and mount-once contract are verified by the production TSX identity suite.
use nsh_base::dock::{
    DockArea, DockAreaApi, DockAreaLayout as NativeArea, DockLayout, DockNode as NativeNode,
    DockPanel, DockTab, NodeId, PanelId, dock_area,
};
use nsh_core::{
    Size,
    element::Root,
    geometry::{Axis as NativeAxis, point},
    platform::{Idiom, Os},
    prelude::*,
};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;
use riwork_core::layouts::{Axis, Layout, ProjectLayout, SavedPane, SavedTab};
use riwork_nsh_workspace::layout::{DockAreaLayout, DockAxis, DockNode, Extent, LayoutModel};
use std::collections::{BTreeMap, HashSet};
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicU64, Ordering},
};

const AREA: Extent = Extent {
    width: 800.0,
    height: 600.0,
};

fn from_native(node: &NativeNode) -> DockNode {
    match node {
        NativeNode::Tabs {
            id,
            panels,
            active_index,
        } => DockNode::Tabs {
            id: id.map(NodeId::as_u64),
            panels: panels.iter().map(ToString::to_string).collect(),
            active_index: *active_index,
        },
        NativeNode::Split {
            id,
            axis,
            children,
            sizes,
        } => DockNode::Split {
            id: id.map(NodeId::as_u64),
            axis: if *axis == NativeAxis::Horizontal {
                DockAxis::Horizontal
            } else {
                DockAxis::Vertical
            },
            children: children.iter().map(from_native).collect(),
            sizes: sizes.iter().map(|x| x.map(|x| x.0)).collect(),
        },
    }
}
fn collect_ids(node: &NativeNode, ids: &mut BTreeMap<u64, NodeId>) {
    if let Some(id) = node.id() {
        ids.insert(id.as_u64(), id);
    }
    if let NativeNode::Split { children, .. } = node {
        for child in children {
            collect_ids(child, ids);
        }
    }
}
fn to_native(node: &DockNode, ids: &BTreeMap<u64, NodeId>) -> NativeNode {
    match node {
        DockNode::Tabs {
            id,
            panels,
            active_index,
        } => NativeNode::Tabs {
            id: id.and_then(|id| ids.get(&id).copied()),
            panels: panels.iter().cloned().map(Into::into).collect(),
            active_index: *active_index,
        },
        DockNode::Split {
            axis,
            children,
            sizes,
            ..
        } => NativeNode::Split {
            id: None,
            axis: if *axis == DockAxis::Horizontal {
                NativeAxis::Horizontal
            } else {
                NativeAxis::Vertical
            },
            children: children.iter().map(|node| to_native(node, ids)).collect(),
            sizes: sizes.iter().map(|s| s.map(px)).collect(),
        },
    }
}

struct Probe {
    model: LayoutModel,
    extent: Extent,
    ids: BTreeMap<u64, NodeId>,
    raw: Vec<DockAreaLayout>,
    applies: usize,
    projections: usize,
    diagnostics: Vec<String>,
    removed: Vec<String>,
    pane_sizes: BTreeMap<String, Size<Points>>,
    frames: BTreeMap<nsh_core::NodeId, nsh_core::Bounds<Points>>,
    parents: BTreeMap<nsh_core::NodeId, nsh_core::NodeId>,
    labels: BTreeMap<nsh_core::NodeId, String>,
    test_ids: BTreeMap<nsh_core::NodeId, String>,
    suffix_pressed: Vec<u64>,
}
static PROBES: OnceLock<Mutex<BTreeMap<u64, Weak<Mutex<Probe>>>>> = OnceLock::new();
static NEXT_PROBE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, PartialEq, Props)]
struct FixtureProps {
    probe_id: u64,
    defer: bool,
    suffix: bool,
    reveal_controls: bool,
}

#[component]
fn Fixture(props: FixtureProps) -> impl IntoElement {
    let probe = PROBES.get().unwrap().lock().unwrap()[&props.probe_id]
        .upgrade()
        .unwrap();
    let initial = {
        let mut state = probe.lock().unwrap();
        let extent = state.extent;
        let dock = state.model.dock(extent);
        NativeArea::new(to_native(&dock.center, &state.ids))
    };
    let extent = probe.lock().unwrap().extent;
    let layout = signal(initial);
    let size_probe = probe.clone();
    let remove_probe = probe.clone();
    let suffix_probe = probe.clone();
    let defer = props.defer;
    let area = component_ref::<DockArea>();
    let reveal_last = signal(false);
    view().w(px(extent.width)).h(px(extent.height)).child(
        dock_area()
            .component_ref(&area)
            .layout(move || layout.get())
            .compact(false)
            .divider_size(px(5.0))
            .tab_bar_class("flex-row h-6")
            .handle_accessibility_label("Pane divider")
            .when(props.suffix, move |dock| {
                dock.render_tab_bar_suffix(move |id: &u64| {
                    let id = *id;
                    let pressed = suffix_probe.clone();
                    view()
                        .test_id(&format!("pane-menu-{id}"))
                        .w(px(28.0))
                        .h(px(24.0))
                        .on_press(move || {
                            pressed.lock().unwrap().suffix_pressed.push(id);
                            if props.reveal_controls {
                                reveal_last.set(!reveal_last.get());
                                if let Some(api) = area.api::<DockAreaApi>() {
                                    api.select_panel(
                                        if reveal_last.get() {
                                            "shell:extra-7"
                                        } else {
                                            "shell:a"
                                        }
                                        .into(),
                                    );
                                }
                            }
                        })
                        .child(text("Menu"))
                })
            })
            .render_tab(|tab: &DockTab| {
                view()
                    .test_id(&format!("tab-{}", tab.panel))
                    .w(px(80.0))
                    .h(px(24.0))
                    .child(text(format!("Tab {}", tab.panel)))
            })
            .render_panel(move |panel: &DockPanel| {
                let id = panel.id.to_string();
                let size_probe = size_probe.clone();
                view()
                    .flex_1()
                    .child(text(format!("Panel {}", panel.id)))
                    .on_layout(move |bounds: &nsh_core::Bounds<Points>| {
                        size_probe
                            .lock()
                            .unwrap()
                            .pane_sizes
                            .insert(id.clone(), bounds.size);
                    })
            })
            .on_panel_remove(move |id: &PanelId| {
                remove_probe.lock().unwrap().removed.push(id.to_string())
            })
            .on_layout_change(move |next: &NativeArea| {
                let projected = {
                    let mut state = probe.lock().unwrap();
                    collect_ids(&next.center, &mut state.ids);
                    let reported = DockAreaLayout::new(from_native(&next.center));
                    state.raw.push(reported.clone());
                    if defer {
                        next.clone()
                    } else {
                        state.applies += 1;
                        let projected = state
                            .model
                            .apply_dock(reported, extent)
                            .expect("real DockArea gesture accepted");
                        state.projections += 1;
                        if let Some(diagnostic) = state.model.diagnostic().map(str::to_owned) {
                            state.diagnostics.push(diagnostic);
                        }
                        NativeArea::new(to_native(&projected.center, &state.ids))
                    }
                };
                layout.set(projected);
            })
            .size_full(),
    )
}

#[derive(Default)]
struct Options {
    single_source: bool,
    empty_target: bool,
    source_main: bool,
    target_locked: bool,
    defer: bool,
    extent: Option<Extent>,
    stacked: bool,
    suffix: bool,
    extra_tabs: usize,
    reveal_controls: bool,
}
fn start(options: Options) -> (Headless, Arc<Mutex<Probe>>, u64, u64) {
    let extent = options.extent.unwrap_or(AREA);
    let mut source = DockLayout::tabs().panel("shell:a");
    if !options.single_source {
        source = source.panel("shell:b");
    }
    for index in 0..options.extra_tabs {
        source = source.panel(format!("shell:extra-{index}"));
    }
    let native = NativeArea::new(
        DockLayout::h_split()
            .child(source, Some(px(400.0)))
            .child(DockLayout::tabs().panel("shell:c"), None),
    );
    let mut native_ids = BTreeMap::new();
    collect_ids(&native.center, &mut native_ids);
    fn groups(node: &NativeNode, out: &mut Vec<(u64, Vec<String>)>) {
        match node {
            NativeNode::Tabs { id, panels, .. } => out.push((
                id.unwrap().as_u64(),
                panels.iter().map(ToString::to_string).collect(),
            )),
            NativeNode::Split { children, .. } => {
                for child in children {
                    groups(child, out);
                }
            }
        }
    }
    let mut groups_out = Vec::new();
    groups(&native.center, &mut groups_out);
    let left = groups_out[0].0;
    let right = groups_out[1].0;
    let mut project:ProjectLayout=serde_json::from_value(serde_json::json!({"layout":{"pane":left},"active_pane":left,"locked_panes":[],"panels_initialized":true})).unwrap();
    project.layout = Layout::Split {
        axis: if options.stacked {
            Axis::Stacked
        } else {
            Axis::SideBySide
        },
        ratio: 400.0 / 795.0,
        first: Box::new(Layout::Pane(left)),
        second: Box::new(Layout::Pane(right)),
    };
    for (id, keys) in groups_out {
        project.panes.insert(
            id,
            SavedPane {
                tabs: if options.empty_target && id == right {
                    vec![]
                } else {
                    keys.iter()
                        .map(|key| SavedTab::Shell {
                            shell_id: key.trim_start_matches("shell:").into(),
                        })
                        .collect()
                },
                ..SavedPane::default()
            },
        );
    }
    if options.source_main {
        project.main_pane = Some(left);
    }
    if options.target_locked {
        project.locked_panes = Some(HashSet::from([right]));
    }
    let model = LayoutModel::new(project).unwrap();
    let probe = Arc::new(Mutex::new(Probe {
        model,
        extent,
        ids: native_ids,
        raw: vec![],
        applies: 0,
        projections: 0,
        diagnostics: vec![],
        removed: vec![],
        pane_sizes: BTreeMap::new(),
        frames: BTreeMap::new(),
        parents: BTreeMap::new(),
        labels: BTreeMap::new(),
        test_ids: BTreeMap::new(),
        suffix_pressed: Vec::new(),
    }));
    let probe_id = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
    PROBES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(probe_id, Arc::downgrade(&probe));
    let mut config = EngineConfig {
        viewport: Size::new(px(extent.width + 200.0), px(extent.height + 100.0)),
        clock: nsh_engine::Clock::manual(std::time::SystemTime::UNIX_EPOCH),
        ..EngineConfig::new(Root::new::<Fixture>(move || FixtureProps {
            probe_id,
            defer: options.defer,
            suffix: options.suffix,
            reveal_controls: options.reveal_controls,
        }))
    };
    config.environment.os = Os::MacOs;
    config.environment.idiom = Idiom::Desktop;
    let mut app = Headless::start(config).unwrap();
    let observe_probe = probe.clone();
    app.observe(move |_, batch| {
        let mut state = observe_probe.lock().unwrap();
        for item in &batch.items {
            match item {
                nsh_core::MountItem::Frame { id, frame } => {
                    state.frames.insert(*id, *frame);
                }
                nsh_core::MountItem::Insert { parent, child, .. } => {
                    state.parents.insert(*child, *parent);
                }
                nsh_core::MountItem::ViewProps { id, props } => {
                    if let Some(label) = &props.accessibility.label {
                        state.labels.insert(*id, label.to_string());
                    }
                    if let Some(test_id) = &props.test_id {
                        state.test_ids.insert(*id, test_id.to_string());
                    }
                }
                nsh_core::MountItem::AccessibilityLabel {
                    id,
                    label: Some(label),
                } => {
                    state.labels.insert(*id, label.to_string());
                }
                nsh_core::MountItem::Delete { id } => {
                    state.frames.remove(id);
                    state.parents.remove(id);
                    state.labels.remove(id);
                    state.test_ids.remove(id);
                }
                _ => {}
            }
        }
    });
    app.wait_for("Tab shell:a").unwrap();
    (app, probe, left, right)
}
fn assert_one_projection(state: &Probe) {
    assert!(!state.raw.is_empty());
    assert_eq!(state.applies, state.raw.len());
    assert_eq!(state.projections, state.applies);
    assert!(state.diagnostics.is_empty(), "{:?}", state.diagnostics);
}

#[test]
fn gesture_active_index() {
    let (mut app, probe, left, _) = start(Options::default());
    app.press("id:tab-shell:b").unwrap();
    let state = probe.lock().unwrap();
    assert_one_projection(&state);
    assert_eq!(state.raw.len(), 1);
    assert_eq!(state.model.project().active_pane, left);
    assert_eq!(
        state.model.project().panes[&left].active_tab_key.as_deref(),
        Some("shell:b")
    );
    assert!(!app.shows("Panel shell:a"));
    assert!(app.shows("Panel shell:b"));
}

#[test]
fn gesture_sizes() {
    let (mut app, probe, left, _) = start(Options::default());
    app.drag("label:Pane divider", point(px(100.0), px(0.0)))
        .unwrap();
    let state = probe.lock().unwrap();
    assert_one_projection(&state);
    let width = state.model.project().layout.pane_extents(AREA)[&left].width;
    assert!((width - 500.0).abs() <= 0.5, "width {width}");
}

#[test]
fn a_dragged_ratio_is_clamped_to_0_1_0_9() {
    let (mut app, probe, _, _) = start(Options::default());
    app.drag("label:Pane divider", point(px(1000.0), px(0.0)))
        .unwrap();
    let state = probe.lock().unwrap();
    assert_one_projection(&state);
    let ratio = state.model.project().layout.ratio_at(&[]).unwrap();
    assert!((0.1..=0.9).contains(&ratio));
}

#[test]
fn gesture_reorder_in_group() {
    let (mut app, probe, left, _) = start(Options::default());
    app.drag("id:tab-shell:b", point(px(-100.0), px(0.0)))
        .unwrap();
    let state = probe.lock().unwrap();
    assert_one_projection(&state);
    assert_eq!(state.raw.len(), 1);
    assert_eq!(
        state.model.project().panes[&left]
            .tabs
            .iter()
            .map(SavedTab::key)
            .collect::<Vec<_>>(),
        ["shell:b", "shell:a"]
    );
}

#[test]
fn gesture_move_to_group() {
    let (mut app, probe, left, right) = start(Options::default());
    app.press("id:tab-shell:b").unwrap();
    app.drag("id:tab-shell:a", point(px(560.0), px(288.0)))
        .unwrap();
    let state = probe.lock().unwrap();
    assert_one_projection(&state);
    assert_eq!(
        state.model.project().panes[&left].active_tab_key.as_deref(),
        Some("shell:b")
    );
    assert_eq!(
        state.model.project().panes[&right]
            .tabs
            .iter()
            .map(SavedTab::key)
            .collect::<Vec<_>>(),
        ["shell:c", "shell:a"]
    );
    assert_eq!(state.model.project().active_pane, right);
    assert!(state.removed.is_empty());
}

#[test]
fn gesture_move_to_edge() {
    for (dx, dy, axis, new_first) in [
        (380.0, 288.0, Axis::SideBySide, true),
        (720.0, 288.0, Axis::SideBySide, false),
        (560.0, 30.0, Axis::Stacked, true),
        (560.0, 550.0, Axis::Stacked, false),
    ] {
        let (mut app, probe, _, right) = start(Options::default());
        app.drag("id:tab-shell:a", point(px(dx), px(dy))).unwrap();
        let state = probe.lock().unwrap();
        assert_one_projection(&state);
        assert_eq!(state.model.project().panes.len(), 3, "edge {dx} {dy}");
        let new = state.model.project().active_pane;
        let Layout::Split { second, .. } = &state.model.project().layout else {
            panic!()
        };
        let Layout::Split {
            axis: actual,
            ratio,
            first,
            second,
        } = &**second
        else {
            panic!("edge did not split target")
        };
        assert_eq!(*actual, axis);
        assert_eq!(
            ((**first).clone(), (**second).clone()),
            (
                Layout::Pane(if new_first { new } else { right }),
                Layout::Pane(if new_first { right } else { new })
            )
        );
        assert!(
            (*ratio - 0.5).abs() < 0.03,
            "reported split fraction {ratio}"
        );
        assert!(state.removed.is_empty());
    }
}

#[test]
fn native_edge_zones_change_between_34_and_36_percent_of_the_content() {
    // Source tab center is (40,12); target body is (405,24,395,576).
    // Sample either side of the upstream 35%/65% boundaries using real
    // pointer drags, including vertical boundaries away from the corners.
    for (x, y, splits) in [
        (0.34, 0.50, true),
        (0.36, 0.50, false),
        (0.64, 0.50, false),
        (0.66, 0.50, true),
        (0.50, 0.34, true),
        (0.50, 0.36, false),
        (0.50, 0.64, false),
        (0.50, 0.66, true),
    ] {
        let (mut app, probe, _, _) = start(Options::default());
        app.drag(
            "id:tab-shell:a",
            point(px(405.0 + 395.0 * x - 40.0), px(24.0 + 576.0 * y - 12.0)),
        )
        .unwrap();
        let state = probe.lock().unwrap();
        assert_one_projection(&state);
        assert_eq!(
            state.model.project().panes.len(),
            if splits { 3 } else { 2 },
            "content fraction ({x}, {y})"
        );
        assert!(state.removed.is_empty());
    }
}

#[test]
fn gesture_group_disappeared_keeps_main_and_locked_neighbours() {
    for options in [
        Options {
            single_source: true,
            source_main: true,
            ..Options::default()
        },
        Options {
            single_source: true,
            target_locked: true,
            ..Options::default()
        },
    ] {
        let (mut app, probe, left, _) = start(options);
        app.drag("id:tab-shell:a", point(px(560.0), px(288.0)))
            .unwrap();
        let state = probe.lock().unwrap();
        assert_one_projection(&state);
        assert_eq!(state.model.project().panes.len(), 2);
        assert!(state.model.project().panes[&left].tabs.is_empty());
        assert!(app.shows(&format!("Tab empty:{left}")));
    }
}

#[test]
fn gesture_sentinel_next_to_real_panels_is_dropped() {
    let (mut app, probe, _, right) = start(Options {
        empty_target: true,
        ..Options::default()
    });
    app.drag("id:tab-shell:a", point(px(560.0), px(288.0)))
        .unwrap();
    let state = probe.lock().unwrap();
    assert_one_projection(&state);
    assert_eq!(
        state.model.project().panes[&right].tabs,
        vec![SavedTab::Shell {
            shell_id: "a".into()
        }]
    );
    assert!(!app.shows(&format!("Tab empty:{right}")));
}

#[test]
fn gesture_two_changes_fall_back_and_log() {
    let (mut app, probe, _, _) = start(Options {
        defer: true,
        ..Options::default()
    });
    app.press("id:tab-shell:b").unwrap();
    app.drag("label:Pane divider", point(px(100.0), px(0.0)))
        .unwrap();
    let mut state = probe.lock().unwrap();
    assert!(state.raw.len() >= 2);
    let reported = state.raw.last().unwrap().clone();
    state.model.apply_dock(reported, AREA).unwrap();
    assert!(
        state
            .model
            .diagnostic()
            .unwrap()
            .starts_with("layout.fallback:")
    );
    assert_eq!(
        state
            .model
            .project()
            .panes
            .values()
            .map(|pane| pane.tabs.len())
            .sum::<usize>(),
        3
    );
}

#[test]
fn five_point_dividers_and_rendered_pane_extents_match_core_at_three_extents() {
    for extent in [
        Extent {
            width: 1280.0,
            height: 800.0,
        },
        Extent {
            width: 1920.0,
            height: 1080.0,
        },
        Extent {
            width: 640.0,
            height: 400.0,
        },
    ] {
        for stacked in [false, true] {
            let (_app, probe, left, right) = start(Options {
                extent: Some(extent),
                stacked,
                ..Options::default()
            });
            let state = probe.lock().unwrap();
            let expected = state.model.project().layout.pane_extents(extent);
            for (id, key) in [(left, "shell:a"), (right, "shell:c")] {
                let size = state.pane_sizes[key];
                assert!(
                    (size.width.0 - expected[&id].width).abs() <= 0.5,
                    "{extent:?} stacked={stacked} {key}: width {:?} expected {:?}",
                    size,
                    expected[&id]
                );
                assert!(
                    (size.height.0 + 24.0 - expected[&id].height).abs() <= 0.5,
                    "{extent:?} stacked={stacked} {key}: height {:?} expected {:?}",
                    size,
                    expected[&id]
                );
            }
            let handles: Vec<_> = state
                .labels
                .iter()
                .filter(|(_, label)| label.as_str() == "Pane divider")
                .map(|(id, _)| *id)
                .collect();
            assert_eq!(handles.len(), 1);
            let indicators: Vec<_> = state
                .parents
                .iter()
                .filter(|(_, parent)| handles.contains(parent))
                .filter_map(|(id, _)| state.frames.get(id))
                .filter(|frame| frame.size.width.0 > 0.0 && frame.size.height.0 > 0.0)
                .collect();
            assert_eq!(indicators.len(), 1);
            let size = indicators[0].size;
            assert_eq!(if stacked { size.height.0 } else { size.width.0 }, 5.0);
            assert!(
                (if stacked {
                    size.width.0 - extent.width
                } else {
                    size.height.0 - extent.height
                })
                .abs()
                    <= 0.5
            );
        }
    }
}

#[test]
fn tab_bar_suffix_stays_visible_beside_overflowing_tabs_and_drag_still_moves() {
    let (mut app, probe, left, right) = start(Options {
        suffix: true,
        extra_tabs: 8,
        ..Options::default()
    });
    app.press(&format!("id:pane-menu-{left}")).unwrap();
    {
        let state = probe.lock().unwrap();
        assert_eq!(state.suffix_pressed, [left]);
        let id = *state
            .test_ids
            .iter()
            .find(|(_, id)| id.as_str() == format!("pane-menu-{left}"))
            .unwrap()
            .0;
        let mut x = 0.0;
        let mut cursor = Some(id);
        while let Some(id) = cursor {
            if let Some(frame) = state.frames.get(&id) {
                x += frame.origin.x.0;
            }
            cursor = state.parents.get(&id).copied();
        }
        assert!(
            (x + 28.0 - 400.0).abs() <= 0.5,
            "menu right edge {}",
            x + 28.0
        );
    }
    app.drag("id:tab-shell:a", point(px(560.0), px(288.0)))
        .unwrap();
    let state = probe.lock().unwrap();
    assert_one_projection(&state);
    assert!(
        state.model.project().panes[&right]
            .tabs
            .iter()
            .any(|tab| tab.key() == "shell:a")
    );
    assert!(state.removed.is_empty());
}

#[test]
fn selecting_a_clipped_native_tab_reveals_it_and_can_scroll_back() {
    let (mut app, probe, left, _) = start(Options {
        suffix: true,
        extra_tabs: 8,
        reveal_controls: true,
        defer: true,
        ..Options::default()
    });
    let scrolls = |app: &Headless| {
        app.screen()
            .commands()
            .iter()
            .filter_map(|(_, command)| {
                if let nsh_core::Command::ScrollTo {
                    x, animated: false, ..
                } = command
                {
                    Some(x.0)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    assert!(scrolls(&app).is_empty());
    app.press(&format!("id:pane-menu-{left}")).unwrap();
    // Native measurement: 10 * 80pt cells, 400 - 28pt viewport. The
    // minimum reveal offset is 428pt. Headless selectors include clipped
    // content, so assert the actual native scroll commands and reverse move.
    assert_eq!(scrolls(&app), [428.0]);
    app.press(&format!("id:pane-menu-{left}")).unwrap();
    assert_eq!(scrolls(&app), [428.0, 0.0]);
    let state = probe.lock().unwrap();
    assert_eq!(state.raw.len(), 2);
    assert!(state.removed.is_empty());
}
