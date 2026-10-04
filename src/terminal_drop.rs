//! Files dropped on a terminal: their paths are pasted into it, as Ghostty and Terminal do.
//!
//! The terminal's native view takes no part in hit testing and is not registered for drags, so
//! a drag from Finder reaches the window's GPUI view, which hands the dropped files to GPUI as
//! `ExternalPaths`. The terminal of a local shell takes them (see `terminal_link_layer`): each
//! path is shell-escaped the way Ghostty escapes it, the paths are joined by spaces, and the text
//! is pasted into the shell's tmux pane (`paste-buffer -p`), bracketed when the program asked for
//! bracketed paste. Ghostty's own drop does the same through its paste path, so a CLI such as
//! Claude Code or Codex sees a paste and can attach a dropped picture.
//!
//! A terminal of another Mac's shell takes nothing: a path on this Mac means nothing there.

use std::{cell::RefCell, path::PathBuf, rc::Rc};

use gpui::{
    AnyElement, Bounds, Context, DragMoveEvent, ExternalPaths, Pixels, Window, canvas, prelude::*,
};

use crate::{
    PaneId, TabId, Workspace,
    terminal_links::{Overlay, Strip},
    theme,
};

/// The characters Ghostty puts a backslash before in a dropped path (`Ghostty.Shell.escape`).
const ESCAPED: &str = "\\ ()[]{}<>\"'`!#$&;|*?\t";

/// How thick the outline around the terminal under a drag is, in points.
const OUTLINE: f64 = 2.0;

/// `text` with a backslash before every character a shell would read specially.
pub fn shell_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if ESCAPED.contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// What a drop of `paths` types: each path escaped, separated by spaces, with nothing after the
/// last, as Ghostty does. `None` for no paths.
pub fn dropped_text(paths: &[PathBuf]) -> Option<String> {
    (!paths.is_empty()).then(|| {
        paths
            .iter()
            .map(|path| shell_escape(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// A thin outline just inside `bounds` (window points), as four strips.
fn outline(bounds: Bounds<Pixels>) -> Vec<Strip> {
    let x = f64::from(bounds.origin.x.as_f32());
    let y = f64::from(bounds.origin.y.as_f32());
    let width = f64::from(bounds.size.width.as_f32());
    let height = f64::from(bounds.size.height.as_f32());
    if width < OUTLINE * 2.0 || height < OUTLINE * 2.0 {
        return Vec::new();
    }
    let strip = |x, y, width, height| Strip {
        x,
        y,
        width,
        height,
    };
    vec![
        strip(x, y, width, OUTLINE),
        strip(x, y + height - OUTLINE, width, OUTLINE),
        strip(x, y + OUTLINE, OUTLINE, height - OUTLINE * 2.0),
        strip(
            x + width - OUTLINE,
            y + OUTLINE,
            OUTLINE,
            height - OUTLINE * 2.0,
        ),
    ]
}

/// The terminal a drag of files is over, and the layers that outline it.
#[derive(Default)]
pub struct DropState {
    /// The tab under the drag and its bounds in the window.
    target: Option<(TabId, Bounds<Pixels>)>,
    overlay: Rc<RefCell<Overlay>>,
}

impl Workspace {
    /// A drag of files moved; `tab_id`'s terminal, laid out at `event.bounds`, is under it or not.
    pub(crate) fn terminal_drag_moved(
        &mut self,
        tab_id: TabId,
        event: &DragMoveEvent<ExternalPaths>,
        cx: &mut Context<Self>,
    ) {
        let over = event.bounds.contains(&event.event.position);
        let current = self.terminal_drop.target.map(|(tab, _)| tab);
        if over {
            let target = Some((tab_id, event.bounds));
            if self.terminal_drop.target != target {
                self.terminal_drop.target = target;
                cx.notify();
            }
        } else if current == Some(tab_id) {
            self.terminal_drop.target = None;
            cx.notify();
        }
    }

    /// Files were dropped on `tab_id`'s terminal: paste their paths into its shell and give it
    /// the keys.
    pub(crate) fn terminal_dropped(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_drop.target = None;
        cx.notify();
        let Some(text) = dropped_text(paths.paths()) else {
            return;
        };
        let Some(shell_id) = self
            .panes
            .get(&pane_id)
            .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == tab_id))
            .and_then(|tab| tab.shell_id())
            .map(str::to_owned)
        else {
            return;
        };
        self.select_pane(pane_id, window, cx);
        let sessions = self.sessions.clone();
        // tmux is a process call; keep it off the UI thread.
        std::thread::spawn(move || {
            if let Err(error) = sessions.paste(&shell_id, &text) {
                eprintln!("riwork: dropped paths not pasted into {shell_id}: {error}");
            }
        });
    }

    /// Forget the terminal under a drag once no drag is going on.
    pub(crate) fn settle_terminal_drop(&mut self, cx: &Context<Self>) {
        if !cx.has_active_drag() {
            self.terminal_drop.target = None;
        }
    }

    /// The window's last element: outlines the terminal under a drag of files, above the native
    /// terminals, or takes the outline away.
    pub(crate) fn terminal_drop_outline(&self, cx: &Context<Self>) -> AnyElement {
        // A dialog over the window hides the terminals; the outline would show through it.
        let strips = (!self.modal_open())
            .then_some(self.terminal_drop.target)
            .flatten()
            .map(|(_, bounds)| outline(bounds))
            .unwrap_or_default();
        let overlay = self.terminal_drop.overlay.clone();
        let color = theme::palette(cx).cyan;
        canvas(
            |_, _, _| {},
            move |_, _, window, _| overlay.borrow_mut().show(window, &strips, color),
        )
        .absolute()
        .top_0()
        .left_0()
        .size_0()
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    #[test]
    fn escapes_what_ghostty_escapes() {
        assert_eq!(
            shell_escape("/Users/me/Desktop/Screenshot 2026-10-04 at 10.00.00.png"),
            "/Users/me/Desktop/Screenshot\\ 2026-10-04\\ at\\ 10.00.00.png"
        );
        assert_eq!(
            shell_escape("a\\b c(d)[e]{f}<g>\"h'i`j!k#l$m&n;o|p*q?r\ts"),
            "a\\\\b\\ c\\(d\\)\\[e\\]\\{f\\}\\<g\\>\\\"h\\'i\\`j\\!k\\#l\\$m\\&n\\;o\\|p\\*q\\?r\\\ts"
        );
        // Ghostty leaves these alone.
        assert_eq!(
            shell_escape("/a/b-c_d.e,f=g~h%i^j+k@l:m"),
            "/a/b-c_d.e,f=g~h%i^j+k@l:m"
        );
        assert_eq!(shell_escape("/tmp/Bild ü.png"), "/tmp/Bild\\ ü.png");
    }

    #[test]
    fn several_paths_are_joined_by_one_space_with_none_after() {
        let paths = [PathBuf::from("/tmp/a b.png"), PathBuf::from("/tmp/c")];
        assert_eq!(
            dropped_text(&paths).as_deref(),
            Some("/tmp/a\\ b.png /tmp/c")
        );
        assert_eq!(dropped_text(&[]), None);
    }

    #[test]
    fn the_outline_runs_just_inside_the_bounds() {
        let bounds = Bounds {
            origin: point(px(10.0), px(20.0)),
            size: size(px(100.0), px(50.0)),
        };
        let strips = outline(bounds);
        assert_eq!(strips.len(), 4);
        for strip in &strips {
            assert!(strip.x >= 10.0 && strip.x + strip.width <= 110.0);
            assert!(strip.y >= 20.0 && strip.y + strip.height <= 70.0);
        }
        assert!(
            outline(Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(3.0), px(50.0)),
            })
            .is_empty()
        );
    }
}
