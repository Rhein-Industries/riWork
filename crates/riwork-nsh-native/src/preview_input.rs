//! Pure input admission and coordinate policy for `riwork.preview`.
//!
//! The platform adapter supplies coordinates relative to the surface, native
//! focus/window changes, and committed (not marked) text. No process, window,
//! clipboard, or global state is accessed here.
use riwork_core::app_preview::{PreviewInput, PreviewMode, PreviewMouseButton};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SurfaceEvent {
    Layout {
        width: f32,
        height: f32,
    },
    Focus {
        focused: bool,
    },
    PointerDown {
        x: f32,
        y: f32,
        button: PreviewMouseButton,
    },
    PointerUp {
        x: f32,
        y: f32,
        button: PreviewMouseButton,
    },
    PointerMove {
        x: f32,
        y: f32,
    },
    Scroll {
        x: f32,
        y: f32,
        delta_x: f32,
        delta_y: f32,
        precise: bool,
    },
    Key {
        key: String,
        text_key: bool,
        #[serde(default)]
        paste: Option<String>,
    },
    KeyUp {
        key: String,
        text_key: bool,
    },
    Text {
        text: String,
    },
    Window {
        visible: bool,
        active: bool,
    },
    Release,
}

#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub inputs: Vec<PreviewInput>,
    /// Only an admitted press within the actual frame may take native focus.
    pub request_focus: bool,
}

pub struct Policy {
    mode: PreviewMode,
    frame: Option<(u32, u32)>,
    viewport: (f32, f32),
    tab_visible: bool,
    capture_visible: bool,
    window_visible: bool,
    window_active: bool,
    running: bool,
    local_focus: bool,
    remote_focus: bool,
    buttons: Vec<PreviewMouseButton>,
    keys: Vec<String>,
    last_pointer: Option<(f32, f32)>,
    last_resize: Option<(u32, u32)>,
}

impl Policy {
    pub fn new(mode: PreviewMode) -> Self {
        Self {
            mode,
            frame: None,
            viewport: (0., 0.),
            tab_visible: false,
            capture_visible: false,
            window_visible: false,
            window_active: false,
            running: false,
            local_focus: false,
            remote_focus: false,
            buttons: Vec::new(),
            keys: Vec::new(),
            last_pointer: None,
            last_resize: None,
        }
    }

    pub fn focused(&self) -> bool {
        self.remote_focus
    }

    pub fn accepts_input(&self) -> bool {
        self.tab_visible
            && self.capture_visible
            && self.window_visible
            && self.window_active
            && self.running
            && self
                .frame
                .is_some_and(|(width, height)| width > 0 && height > 0)
    }

    /// Update the session state. The owner explicitly calls `reset` before a
    /// new generation even if its frame dimensions happen to be unchanged.
    pub fn configure(
        &mut self,
        mode: PreviewMode,
        frame: Option<(u32, u32)>,
        tab_visible: bool,
        capture_visible: bool,
        running: bool,
    ) -> Outcome {
        let mut result = Outcome::default();
        let valid_frame = frame.filter(|(width, height)| *width > 0 && *height > 0);
        // Release using the old geometry before losing it or changing mode.
        if mode != self.mode
            || valid_frame.is_none()
            || !tab_visible
            || !capture_visible
            || !running
        {
            self.release_remote(&mut result.inputs);
        }
        if mode != self.mode || !running {
            self.local_focus = false;
            self.last_resize = None;
        }
        self.mode = mode;
        self.frame = valid_frame;
        self.tab_visible = tab_visible;
        self.capture_visible = capture_visible;
        self.running = running;
        self.reconcile(&mut result.inputs);
        result
    }

    pub fn handle(&mut self, event: SurfaceEvent) -> Outcome {
        let mut result = Outcome::default();
        match event {
            SurfaceEvent::Layout { width, height } => {
                if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
                    self.release_remote(&mut result.inputs);
                }
                self.viewport = (width, height);
                self.resize(&mut result.inputs);
            }
            SurfaceEvent::Window { visible, active } => {
                self.window_visible = visible;
                self.window_active = active;
                self.reconcile(&mut result.inputs);
            }
            SurfaceEvent::Focus { focused } => {
                self.local_focus = focused;
                self.reconcile(&mut result.inputs);
            }
            SurfaceEvent::Release => return self.release(),
            SurfaceEvent::PointerDown { x, y, button } if self.accepts_input() => {
                if let Some((mapped_x, mapped_y)) = self.map((x, y), false) {
                    self.local_focus = true;
                    self.ensure_remote_focus(&mut result.inputs);
                    self.last_pointer = Some((x, y));
                    result.request_focus = true;
                    if self.mode == PreviewMode::WindowCapture {
                        result.inputs.push(PreviewInput::Click {
                            x: mapped_x,
                            y: mapped_y,
                            button,
                        });
                    } else {
                        if !self.buttons.contains(&button) {
                            self.buttons.push(button);
                        }
                        result.inputs.push(PreviewInput::PointerDown {
                            x: mapped_x,
                            y: mapped_y,
                            button,
                        });
                    }
                }
            }
            SurfaceEvent::PointerUp { x, y, button } => {
                if let Some(index) = self.buttons.iter().position(|held| *held == button) {
                    // Up-outside is always delivered for an admitted press.
                    // A prior gate loss already drained this list.
                    if let Some((x, y)) = self.map((x, y), true).or_else(|| self.release_point()) {
                        self.buttons.remove(index);
                        result.inputs.push(PreviewInput::PointerUp { x, y, button });
                    }
                }
            }
            SurfaceEvent::PointerMove { x, y }
                if self.accepts_input() && self.mode != PreviewMode::WindowCapture =>
            {
                if let Some((mapped_x, mapped_y)) = self.map((x, y), !self.buttons.is_empty()) {
                    self.last_pointer = Some((x, y));
                    result.inputs.push(PreviewInput::PointerMove {
                        x: mapped_x,
                        y: mapped_y,
                    });
                }
            }
            SurfaceEvent::Scroll {
                x,
                y,
                delta_x,
                delta_y,
                precise,
            } if self.accepts_input() => {
                let unit = if precise { 1. } else { 20. };
                let (delta_x, delta_y) = (delta_x * unit, delta_y * unit);
                if delta_x.is_finite() && delta_y.is_finite() {
                    if let Some((x, y)) = self.map((x, y), false) {
                        result.inputs.push(PreviewInput::Scroll {
                            x,
                            y,
                            delta_x,
                            delta_y,
                        });
                    }
                }
            }
            SurfaceEvent::Key {
                key,
                text_key,
                paste,
            } if self.accepts_input() && self.local_focus => {
                if self.mode != PreviewMode::WindowCapture {
                    if let Some(text) = paste {
                        result.inputs.push(PreviewInput::Text { text });
                        return result;
                    }
                }
                if !text_key && !key.is_empty() {
                    if self.mode != PreviewMode::WindowCapture && !self.keys.contains(&key) {
                        self.keys.push(key.clone());
                    }
                    result.inputs.push(PreviewInput::Key { key });
                }
            }
            SurfaceEvent::KeyUp { key, text_key }
                if self.accepts_input() && self.local_focus && !text_key =>
            {
                if let Some(index) = self.keys.iter().position(|held| *held == key) {
                    self.keys.remove(index);
                    result.inputs.push(PreviewInput::KeyUp { key });
                }
            }
            SurfaceEvent::Text { text }
                if self.accepts_input() && self.local_focus && !text.is_empty() =>
            {
                result.inputs.push(PreviewInput::Text { text });
            }
            _ => {}
        }
        result
    }

    /// Explicit unmount/stop/reset releases are idempotent and cannot restore
    /// focus merely because a subsequent snapshot leaves the gates open.
    pub fn release(&mut self) -> Outcome {
        let mut result = Outcome::default();
        self.release_remote(&mut result.inputs);
        self.local_focus = false;
        result
    }

    pub fn reset(&mut self) -> Outcome {
        let result = self.release();
        self.frame = None;
        self.last_resize = None;
        result
    }

    fn map(&self, point: (f32, f32), clamp: bool) -> Option<(f32, f32)> {
        map_point(self.viewport, self.frame?, point, clamp)
    }

    fn release_point(&self) -> Option<(f32, f32)> {
        self.map(self.last_pointer?, true)
    }

    fn release_remote(&mut self, inputs: &mut Vec<PreviewInput>) {
        if let Some((x, y)) = self.release_point() {
            inputs.extend(
                self.buttons
                    .drain(..)
                    .map(|button| PreviewInput::PointerUp { x, y, button }),
            );
        } else {
            // Geometry is retained until release; this only occurs before an
            // admitted down, or after a malformed platform layout callback.
            self.buttons.clear();
        }
        inputs.extend(self.keys.drain(..).map(|key| PreviewInput::KeyUp { key }));
        if self.remote_focus {
            inputs.push(PreviewInput::Focus { focused: false });
            self.remote_focus = false;
        }
    }

    fn ensure_remote_focus(&mut self, inputs: &mut Vec<PreviewInput>) {
        if !self.remote_focus {
            self.remote_focus = true;
            inputs.push(PreviewInput::Focus { focused: true });
        }
    }

    fn reconcile(&mut self, inputs: &mut Vec<PreviewInput>) {
        if self.accepts_input() && self.local_focus {
            self.ensure_remote_focus(inputs);
        } else {
            self.release_remote(inputs);
        }
        self.resize(inputs);
    }

    fn resize(&mut self, inputs: &mut Vec<PreviewInput>) {
        if self.mode == PreviewMode::Cooperative
            && self.tab_visible
            && self.capture_visible
            && self.window_visible
            && self.window_active
            && self.running
        {
            if let Some(size) = bounded_viewport(self.viewport.0, self.viewport.1) {
                if self.last_resize != Some(size) {
                    self.last_resize = Some(size);
                    inputs.push(PreviewInput::Resize {
                        width: size.0,
                        height: size.1,
                    });
                }
            }
        }
    }
}

pub fn bounded_viewport(width: f32, height: f32) -> Option<(u32, u32)> {
    if !width.is_finite() || !height.is_finite() || width < 32. || height < 32. {
        return None;
    }
    let scale = (1920. / width).min(1200. / height).min(1.);
    Some((
        (width * scale).round().max(32.) as u32,
        (height * scale).round().max(32.) as u32,
    ))
}

/// Aspect-fit coordinates in frame pixels. Only captured drag/up may clamp
/// into the frame; presses and wheel events in the letterbox are ignored.
pub fn map_point(
    viewport: (f32, f32),
    frame: (u32, u32),
    point: (f32, f32),
    clamp: bool,
) -> Option<(f32, f32)> {
    let (width, height) = viewport;
    if frame.0 == 0
        || frame.1 == 0
        || width <= 0.
        || height <= 0.
        || [width, height, point.0, point.1]
            .iter()
            .any(|value| !value.is_finite())
    {
        return None;
    }
    let scale = (width / frame.0 as f32).min(height / frame.1 as f32);
    let origin_x = (width - frame.0 as f32 * scale) / 2.;
    let origin_y = (height - frame.1 as f32 * scale) / 2.;
    let x = (point.0 - origin_x) / scale;
    let y = (point.1 - origin_y) / scale;
    if !clamp && (x < 0. || y < 0. || x >= frame.0 as f32 || y >= frame.1 as f32) {
        return None;
    }
    Some((
        x.clamp(0., frame.0.saturating_sub(1) as f32),
        y.clamp(0., frame.1.saturating_sub(1) as f32),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(mode: PreviewMode) -> Policy {
        let mut policy = Policy::new(mode);
        policy.handle(SurfaceEvent::Layout {
            width: 800.,
            height: 800.,
        });
        policy.handle(SurfaceEvent::Window {
            visible: true,
            active: true,
        });
        policy.configure(mode, Some((800, 600)), true, true, true);
        policy
    }

    fn down(policy: &mut Policy, button: PreviewMouseButton) -> Outcome {
        policy.handle(SurfaceEvent::PointerDown {
            x: 400.,
            y: 400.,
            button,
        })
    }

    fn key(policy: &mut Policy, key: &str) -> Outcome {
        policy.handle(SurfaceEvent::Key {
            key: key.into(),
            text_key: false,
            paste: None,
        })
    }

    #[test]
    fn mapping_rejects_letterboxes_and_clamps_captured_drags() {
        assert_eq!(map_point((800., 800.), (800, 600), (0., 0.), false), None);
        assert_eq!(
            map_point((800., 800.), (800, 600), (0., 100.), false),
            Some((0., 0.))
        );
        assert_eq!(
            map_point((800., 800.), (800, 600), (800., 700.), false),
            None
        );
        assert_eq!(
            map_point((800., 800.), (800, 600), (900., 900.), true),
            Some((799., 599.))
        );
        assert_eq!(
            map_point((800., 800.), (800, 600), (-10., -10.), true),
            Some((0., 0.))
        );
        assert_eq!(map_point((800., 600.), (0, 600), (0., 0.), true), None);
        assert_eq!(
            map_point((f32::NAN, 600.), (800, 600), (0., 0.), true),
            None
        );
        assert_eq!(
            map_point((800., 600.), (800, 600), (f32::INFINITY, 0.), true),
            None
        );
    }

    #[test]
    fn resize_is_bounded_and_only_emitted_on_change_for_cooperative() {
        assert_eq!(bounded_viewport(3840., 2160.), Some((1920, 1080)));
        assert_eq!(bounded_viewport(1000., 2400.), Some((500, 1200)));
        assert_eq!(bounded_viewport(32., 100_000.), Some((32, 1200)));
        assert_eq!(bounded_viewport(31., 100.), None);
        assert_eq!(bounded_viewport(f32::NAN, 100.), None);
        let mut policy = ready(PreviewMode::Cooperative);
        assert!(
            policy
                .handle(SurfaceEvent::Layout {
                    width: 800.,
                    height: 800.
                })
                .inputs
                .is_empty()
        );
        assert_eq!(
            policy
                .handle(SurfaceEvent::Layout {
                    width: 3840.,
                    height: 2160.
                })
                .inputs,
            [PreviewInput::Resize {
                width: 1920,
                height: 1080
            }]
        );
        assert!(
            policy
                .handle(SurfaceEvent::Layout {
                    width: 3840.,
                    height: 2160.
                })
                .inputs
                .is_empty()
        );
        for mode in [PreviewMode::WindowCapture, PreviewMode::IosSimulator] {
            assert!(
                ready(mode)
                    .handle(SurfaceEvent::Layout {
                        width: 3840.,
                        height: 2160.
                    })
                    .inputs
                    .is_empty()
            );
        }
    }

    #[test]
    fn all_buttons_focus_once_and_release_outside_independently() {
        let mut policy = ready(PreviewMode::Cooperative);
        assert_eq!(
            policy.handle(SurfaceEvent::PointerDown {
                x: 400.,
                y: 20.,
                button: PreviewMouseButton::Left
            }),
            Outcome::default()
        );
        for (index, button) in [
            PreviewMouseButton::Left,
            PreviewMouseButton::Right,
            PreviewMouseButton::Middle,
        ]
        .into_iter()
        .enumerate()
        {
            let result = down(&mut policy, button);
            assert!(result.request_focus);
            let mut expected = vec![];
            if index == 0 {
                expected.push(PreviewInput::Focus { focused: true });
            }
            expected.push(PreviewInput::PointerDown {
                x: 400.,
                y: 300.,
                button,
            });
            assert_eq!(result.inputs, expected);
        }
        assert_eq!(
            policy
                .handle(SurfaceEvent::PointerMove { x: 900., y: 900. })
                .inputs,
            [PreviewInput::PointerMove { x: 799., y: 599. }]
        );
        for button in [
            PreviewMouseButton::Middle,
            PreviewMouseButton::Left,
            PreviewMouseButton::Right,
        ] {
            assert_eq!(
                policy
                    .handle(SurfaceEvent::PointerUp {
                        x: -1.,
                        y: -1.,
                        button
                    })
                    .inputs,
                [PreviewInput::PointerUp {
                    x: 0.,
                    y: 0.,
                    button
                }]
            );
            assert!(
                policy
                    .handle(SurfaceEvent::PointerUp {
                        x: -1.,
                        y: -1.,
                        button
                    })
                    .inputs
                    .is_empty()
            );
        }
        assert!(
            policy
                .handle(SurfaceEvent::PointerMove { x: -1., y: -1. })
                .inputs
                .is_empty()
        );
    }

    #[test]
    fn capture_clicks_do_not_create_pointer_or_key_holds() {
        let mut policy = ready(PreviewMode::WindowCapture);
        for button in [
            PreviewMouseButton::Left,
            PreviewMouseButton::Right,
            PreviewMouseButton::Middle,
        ] {
            assert!(
                down(&mut policy, button)
                    .inputs
                    .contains(&PreviewInput::Click {
                        x: 400.,
                        y: 300.,
                        button
                    })
            );
            assert!(
                policy
                    .handle(SurfaceEvent::PointerUp {
                        x: 900.,
                        y: 900.,
                        button
                    })
                    .inputs
                    .is_empty()
            );
        }
        assert!(
            policy
                .handle(SurfaceEvent::PointerMove { x: 400., y: 400. })
                .inputs
                .is_empty()
        );
        assert_eq!(
            policy
                .handle(SurfaceEvent::Key {
                    key: "cmd-v".into(),
                    text_key: false,
                    paste: Some("clipboard".into())
                })
                .inputs,
            [PreviewInput::Key {
                key: "cmd-v".into()
            }]
        );
        assert!(
            policy
                .handle(SurfaceEvent::KeyUp {
                    key: "cmd-v".into(),
                    text_key: false
                })
                .inputs
                .is_empty()
        );
        assert_eq!(
            policy.release().inputs,
            [PreviewInput::Focus { focused: false }]
        );
    }

    #[test]
    fn wheel_uses_twenty_pixels_per_line_and_preserves_precise_deltas() {
        let mut policy = ready(PreviewMode::Cooperative);
        for (precise, expected_x, expected_y) in [(false, 40., -60.), (true, 2., -3.)] {
            assert_eq!(
                policy
                    .handle(SurfaceEvent::Scroll {
                        x: 400.,
                        y: 400.,
                        delta_x: 2.,
                        delta_y: -3.,
                        precise
                    })
                    .inputs,
                [PreviewInput::Scroll {
                    x: 400.,
                    y: 300.,
                    delta_x: expected_x,
                    delta_y: expected_y
                }]
            );
        }
        assert!(
            policy
                .handle(SurfaceEvent::Scroll {
                    x: 400.,
                    y: 20.,
                    delta_x: 2.,
                    delta_y: -3.,
                    precise: false
                })
                .inputs
                .is_empty()
        );
        assert!(
            policy
                .handle(SurfaceEvent::Scroll {
                    x: 400.,
                    y: 400.,
                    delta_x: f32::INFINITY,
                    delta_y: 0.,
                    precise: true
                })
                .inputs
                .is_empty()
        );
    }

    #[test]
    fn text_comes_only_from_commit_and_shortcuts_receive_up_or_release() {
        let mut policy = ready(PreviewMode::Cooperative);
        assert!(key(&mut policy, "enter").inputs.is_empty());
        assert!(
            policy
                .handle(SurfaceEvent::Text {
                    text: "unfocused".into()
                })
                .inputs
                .is_empty()
        );
        policy.handle(SurfaceEvent::Focus { focused: true });
        assert!(
            policy
                .handle(SurfaceEvent::Key {
                    key: "a".into(),
                    text_key: true,
                    paste: None
                })
                .inputs
                .is_empty()
        );
        assert!(
            policy
                .handle(SurfaceEvent::KeyUp {
                    key: "a".into(),
                    text_key: true
                })
                .inputs
                .is_empty()
        );
        assert_eq!(
            policy
                .handle(SurfaceEvent::Text {
                    text: "héllo 日本語 👋".into()
                })
                .inputs,
            [PreviewInput::Text {
                text: "héllo 日本語 👋".into()
            }]
        );
        for name in ["cmd-a", "cmd-c", "shift-tab", "enter"] {
            assert_eq!(
                key(&mut policy, name).inputs,
                [PreviewInput::Key { key: name.into() }]
            );
            // Repeats are forwarded, but only one up is owed.
            assert_eq!(
                key(&mut policy, name).inputs,
                [PreviewInput::Key { key: name.into() }]
            );
            assert_eq!(
                policy
                    .handle(SurfaceEvent::KeyUp {
                        key: name.into(),
                        text_key: false
                    })
                    .inputs,
                [PreviewInput::KeyUp { key: name.into() }]
            );
        }
        key(&mut policy, "left");
        key(&mut policy, "shift-tab");
        assert_eq!(
            policy.release().inputs,
            [
                PreviewInput::KeyUp { key: "left".into() },
                PreviewInput::KeyUp {
                    key: "shift-tab".into()
                },
                PreviewInput::Focus { focused: false }
            ]
        );
        assert!(policy.release().inputs.is_empty());
    }

    #[test]
    fn paste_commits_host_clipboard_without_a_key_or_unmatched_up() {
        let mut policy = ready(PreviewMode::Cooperative);
        policy.handle(SurfaceEvent::Focus { focused: true });
        for text in ["host clipboard 👋", ""] {
            assert_eq!(
                policy
                    .handle(SurfaceEvent::Key {
                        key: "cmd-v".into(),
                        text_key: false,
                        paste: Some(text.into())
                    })
                    .inputs,
                [PreviewInput::Text { text: text.into() }]
            );
            assert!(
                policy
                    .handle(SurfaceEvent::KeyUp {
                        key: "cmd-v".into(),
                        text_key: false
                    })
                    .inputs
                    .is_empty()
            );
        }
        assert_eq!(
            key(&mut policy, "cmd-v").inputs,
            [PreviewInput::Key {
                key: "cmd-v".into()
            }]
        );
    }

    #[test]
    fn each_gate_and_native_blur_release_every_hold_once_before_rejecting_input() {
        for gate in [
            "tab", "capture", "window", "active", "running", "frame", "blur", "release", "reset",
            "mode",
        ] {
            let mut policy = ready(PreviewMode::Cooperative);
            down(&mut policy, PreviewMouseButton::Left);
            down(&mut policy, PreviewMouseButton::Middle);
            policy.handle(SurfaceEvent::PointerMove { x: 900., y: 900. });
            key(&mut policy, "cmd-a");
            let result = match gate {
                "window" => policy.handle(SurfaceEvent::Window {
                    visible: false,
                    active: true,
                }),
                "active" => policy.handle(SurfaceEvent::Window {
                    visible: true,
                    active: false,
                }),
                "blur" => policy.handle(SurfaceEvent::Focus { focused: false }),
                "release" => policy.release(),
                "reset" => policy.reset(),
                "mode" => policy.configure(
                    PreviewMode::WindowCapture,
                    Some((800, 600)),
                    true,
                    true,
                    true,
                ),
                _ => policy.configure(
                    PreviewMode::Cooperative,
                    (gate != "frame").then_some((800, 600)),
                    gate != "tab",
                    gate != "capture",
                    gate != "running",
                ),
            };
            assert_eq!(
                result.inputs,
                [
                    PreviewInput::PointerUp {
                        x: 799.,
                        y: 599.,
                        button: PreviewMouseButton::Left
                    },
                    PreviewInput::PointerUp {
                        x: 799.,
                        y: 599.,
                        button: PreviewMouseButton::Middle
                    },
                    PreviewInput::KeyUp {
                        key: "cmd-a".into()
                    },
                    PreviewInput::Focus { focused: false },
                ],
                "gate {gate}"
            );
            assert!(!policy.focused(), "gate {gate}");
            assert!(
                policy
                    .handle(SurfaceEvent::Text {
                        text: "must stay local".into()
                    })
                    .inputs
                    .is_empty(),
                "gate {gate}"
            );
            assert!(policy.release().inputs.is_empty(), "gate {gate}");
        }
    }

    #[test]
    fn control_visibility_cannot_bypass_hidden_or_inactive_input_gates() {
        let mut policy = ready(PreviewMode::Cooperative);
        policy.configure(
            PreviewMode::Cooperative,
            Some((800, 600)),
            false,
            true,
            true,
        );
        assert!(!policy.accepts_input());
        assert_eq!(
            down(&mut policy, PreviewMouseButton::Left),
            Outcome::default()
        );
        policy.configure(PreviewMode::Cooperative, Some((800, 600)), true, true, true);
        policy.handle(SurfaceEvent::Window {
            visible: true,
            active: false,
        });
        assert!(!policy.accepts_input());
        assert_eq!(
            down(&mut policy, PreviewMouseButton::Left),
            Outcome::default()
        );
    }

    #[test]
    fn activation_restores_only_the_native_focus_still_owned_by_this_surface() {
        let mut policy = ready(PreviewMode::Cooperative);
        policy.handle(SurfaceEvent::Focus { focused: true });
        assert_eq!(
            policy
                .handle(SurfaceEvent::Window {
                    visible: true,
                    active: false
                })
                .inputs,
            [PreviewInput::Focus { focused: false }]
        );
        assert_eq!(
            policy
                .handle(SurfaceEvent::Window {
                    visible: true,
                    active: true
                })
                .inputs,
            [PreviewInput::Focus { focused: true }]
        );
        policy.handle(SurfaceEvent::Window {
            visible: true,
            active: false,
        });
        policy.handle(SurfaceEvent::Focus { focused: false });
        assert!(
            policy
                .handle(SurfaceEvent::Window {
                    visible: true,
                    active: true
                })
                .inputs
                .is_empty()
        );
        policy.release();
        assert!(
            policy
                .configure(PreviewMode::Cooperative, Some((800, 600)), true, true, true)
                .inputs
                .is_empty()
        );
    }

    #[test]
    fn reset_forces_a_new_resize_and_refocus_for_the_next_generation() {
        let mut policy = ready(PreviewMode::Cooperative);
        down(&mut policy, PreviewMouseButton::Left);
        policy.reset();
        assert!(!policy.accepts_input());
        assert_eq!(
            policy
                .configure(PreviewMode::Cooperative, Some((800, 600)), true, true, true)
                .inputs,
            [PreviewInput::Resize {
                width: 800,
                height: 800
            }]
        );
        assert!(!policy.focused());
    }

    #[test]
    fn adapter_events_decode_without_an_implicit_paste_or_text_commit() {
        assert_eq!(
            serde_json::from_str::<SurfaceEvent>(
                r#"{"kind":"key","key":"cmd-v","text_key":false}"#
            )
            .unwrap(),
            SurfaceEvent::Key {
                key: "cmd-v".into(),
                text_key: false,
                paste: None
            }
        );
        assert_eq!(
            serde_json::from_str::<SurfaceEvent>(
                r#"{"kind":"pointer_down","x":1,"y":2,"button":"middle"}"#
            )
            .unwrap(),
            SurfaceEvent::PointerDown {
                x: 1.,
                y: 2.,
                button: PreviewMouseButton::Middle
            }
        );
    }
}
