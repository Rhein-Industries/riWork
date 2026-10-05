//! Files dropped on a terminal, or copied and pasted into it with ⌘V: their paths are pasted
//! into it, as Ghostty and Terminal do.
//!
//! The terminal's native view takes no part in hit testing and is not registered for drags, so
//! a drag from Finder reaches the window's GPUI view, which hands the dropped files to GPUI as
//! `ExternalPaths`. The terminal of a local shell takes them (see `terminal_link_layer`): each
//! path is shell-escaped the way Ghostty escapes it, the paths are joined by spaces, and the text
//! is pasted into the shell's tmux pane (`paste-buffer -p`), bracketed when the program asked for
//! bracketed paste. A CLI agent sees a paste and attaches a dropped picture:
//!
//! - Claude Code splits a paste at each space before a `/`, unescapes every part and attaches
//!   each part that names a picture (png, jpeg, gif, webp); the other parts stay text.
//! - Grok does the same with its own parser.
//! - Codex attaches a paste only when the whole paste is one picture's path, so it is given one
//!   paste per path, with a space after each path that is not a picture (Codex puts one after
//!   an attached picture itself).
//!
//! ⌘V in such a terminal goes to Ghostty's paste, which reads only text from the pasteboard: a
//! file copied in Finder would paste its bare name, and a copied picture with no file (a
//! screenshot taken with ⌃⇧⌘4) nothing. So RiWork takes ⌘V when the pasteboard holds files and
//! pastes their paths as for a drop. A picture alone becomes Ctrl+V for an agent, the key with
//! which Claude Code, Codex and Grok read a picture from the pasteboard themselves; a shell is
//! given nothing, as in Ghostty. Text on the pasteboard is left to Ghostty.
//!
//! A terminal of another Mac's shell takes nothing: a path on this Mac means nothing there.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui::{
    AnyElement, Bounds, ClipboardEntry, Context, DragMoveEvent, ExternalPaths, Pixels, Window,
    canvas, prelude::*,
};

use crate::{
    PaneId, TabId, Workspace,
    session_input::Input,
    sessions::HarnessKind,
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

/// What a terminal's foreground program does with a paste of paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    /// A shell, or any program that is not one of the agents: it gets what Ghostty would paste.
    Shell,
    Claude,
    Codex,
    Grok,
}

/// The agent a process name stands for. Claude Code's process is named after its version
/// (`2.1.289`); Grok's after its build (`grok-1.0.46-mac`).
fn agent(name: &str) -> Option<Program> {
    let name = name.trim().trim_start_matches('-');
    let name = name.rsplit('/').next().unwrap_or(name);
    match name {
        "claude" => Some(Program::Claude),
        "codex" => Some(Program::Codex),
        "grok" => Some(Program::Grok),
        _ if name.starts_with("grok-") => Some(Program::Grok),
        _ if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit() || c == '.') => {
            Some(Program::Claude)
        }
        _ => None,
    }
}

fn shell(name: &str) -> bool {
    let name = name.trim().trim_start_matches('-');
    let name = name.rsplit('/').next().unwrap_or(name);
    matches!(
        name,
        "zsh" | "bash" | "sh" | "fish" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "login"
    )
}

/// The program in front in a pane whose tmux `pane_current_command` is `command`, in a shell
/// started for `harness`. `foreground` names the processes in front on the pane's terminal; it
/// is asked only when `command` does not settle it, as for Codex installed by npm, which runs
/// as `node` with the real `codex` beside it. An agent RiWork started may also run under
/// RiWork's launcher, where the harness tells. A shell in front means the agent is gone.
pub fn program(
    harness: Option<HarnessKind>,
    command: &str,
    foreground: impl FnOnce() -> Vec<String>,
) -> Program {
    if let Some(program) = agent(command) {
        return program;
    }
    if shell(command) {
        return Program::Shell;
    }
    if let Some(program) = foreground().iter().find_map(|name| agent(name)) {
        return program;
    }
    match harness {
        Some(HarnessKind::Claude) => Program::Claude,
        Some(HarnessKind::Codex) => Program::Codex,
        Some(HarnessKind::Grok) => Program::Grok,
        None => Program::Shell,
    }
}

/// The names of the processes in the foreground of terminal `tty` (`/dev/ttys001`).
pub(crate) fn foreground_names(tty: &str) -> Vec<String> {
    let Some(tty) = tty.strip_prefix("/dev/").filter(|tty| !tty.is_empty()) else {
        return Vec::new();
    };
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-t", tty, "-o", "stat=,comm="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().split_once(char::is_whitespace))
        .filter(|(stat, _)| stat.contains('+'))
        .map(|(_, name)| name.trim().to_owned())
        .collect()
}

/// Whether Codex would likely take `path` for a picture: it reads the file, RiWork guesses from
/// the extension, which only decides whether a space follows.
fn picture(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"]
                .contains(&extension.to_ascii_lowercase().as_str())
        })
}

/// The pastes that put `paths` into `program`: the text Ghostty would paste, or for Codex one
/// paste per path (see the module comment).
pub fn path_input(program: Program, paths: &[PathBuf]) -> Vec<Input> {
    if program != Program::Codex {
        return dropped_text(paths).map(Input::Paste).into_iter().collect();
    }
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let mut text = shell_escape(&path.to_string_lossy());
            if index + 1 < paths.len() && !picture(path) {
                text.push(' ');
            }
            Input::Paste(text)
        })
        .collect()
}

/// What ⌘V finds on the pasteboard, as far as the terminal is concerned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Copied {
    /// Files, copied in Finder: their paths go in as for a drop.
    Files(Vec<PathBuf>),
    /// A picture and no text and no file: an agent reads it from the pasteboard on Ctrl+V.
    Picture,
    /// Text, or nothing: Ghostty's own paste.
    Text,
}

/// Sort what GPUI read from the pasteboard. GPUI gives files (with their names as text) before
/// text, and a picture only when there is neither.
pub fn copied(entries: &[ClipboardEntry]) -> Copied {
    let files: Vec<PathBuf> = entries
        .iter()
        .filter_map(|entry| match entry {
            ClipboardEntry::ExternalPaths(paths) => Some(paths.paths()),
            _ => None,
        })
        .flatten()
        .cloned()
        .collect();
    if !files.is_empty() {
        return Copied::Files(files);
    }
    let text = entries
        .iter()
        .any(|entry| matches!(entry, ClipboardEntry::String(string) if !string.text().is_empty()));
    let picture = entries
        .iter()
        .any(|entry| matches!(entry, ClipboardEntry::Image(_)));
    if picture && !text {
        Copied::Picture
    } else {
        Copied::Text
    }
}

/// What ⌘V with `copied` on the pasteboard gives `program`; `None` leaves ⌘V to Ghostty.
pub fn paste_input(copied: &Copied, program: Program) -> Option<Vec<Input>> {
    match copied {
        Copied::Files(paths) => Some(path_input(program, paths)),
        Copied::Picture if program == Program::Shell => Some(Vec::new()),
        Copied::Picture => Some(vec![Input::Key("C-v")]),
        Copied::Text => None,
    }
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
        if paths.paths().is_empty() {
            return;
        }
        let Some(shell_id) = self.local_shell_id(pane_id, tab_id) else {
            return;
        };
        self.select_pane(pane_id, window, cx);
        let paths = paths.paths().to_vec();
        self.give_shell(shell_id, "dropped paths", move |program| {
            path_input(program, &paths)
        });
    }

    /// ⌘V in `tab_id`'s terminal: files or a lone picture on the pasteboard are RiWork's to
    /// paste (see the module comment); anything else goes on to Ghostty's paste.
    pub(crate) fn terminal_paste(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        cx: &mut Context<Self>,
    ) {
        let copied = cx
            .read_from_clipboard()
            .map_or(Copied::Text, |item| copied(item.entries()));
        let shell_id = self.local_shell_id(pane_id, tab_id);
        let (Some(shell_id), false) = (shell_id, copied == Copied::Text) else {
            cx.propagate();
            return;
        };
        self.give_shell(shell_id, "pasted files", move |program| {
            paste_input(&copied, program).unwrap_or_default()
        });
    }

    fn local_shell_id(&self, pane_id: PaneId, tab_id: TabId) -> Option<String> {
        self.panes
            .get(&pane_id)
            .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == tab_id))
            .and_then(|tab| tab.shell_id())
            .map(str::to_owned)
    }

    /// Give `shell_id` what `input` chooses for its foreground program, off the UI thread:
    /// tmux is a process call.
    fn give_shell(
        &self,
        shell_id: String,
        what: &'static str,
        input: impl FnOnce(Program) -> Vec<Input> + Send + 'static,
    ) {
        let sessions = self.sessions.clone();
        std::thread::spawn(move || {
            let result = sessions.paste(&shell_id, |harness, command, tty| {
                input(program(harness, command, || foreground_names(tty)))
            });
            if let Err(error) = result {
                eprintln!("riwork: {what} not pasted into {shell_id}: {error}");
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
    fn the_program_in_front_decides_and_a_shell_in_front_means_the_agent_is_gone() {
        use HarnessKind::{Claude, Codex, Grok};
        let none = Vec::new;
        let unasked = || -> Vec<String> { panic!("the command settles it") };
        assert_eq!(program(None, "zsh", unasked), Program::Shell);
        assert_eq!(program(None, "-zsh\n", unasked), Program::Shell);
        assert_eq!(program(None, "vim", none), Program::Shell);
        assert_eq!(program(None, "claude", unasked), Program::Claude);
        // Claude Code's process carries its version as its name.
        assert_eq!(program(None, "2.1.289", unasked), Program::Claude);
        assert_eq!(program(None, "codex", unasked), Program::Codex);
        assert_eq!(program(None, "grok-1.0.46-mac", unasked), Program::Grok);
        // Codex from npm: node in front, the real binary beside it.
        let npm = || {
            vec![
                "node".to_owned(),
                "/usr/lib/node_modules/@openai/codex/vendor/bin/codex".to_owned(),
            ]
        };
        assert_eq!(program(None, "node", npm), Program::Codex);
        assert_eq!(
            program(None, "node", || vec!["node".to_owned()]),
            Program::Shell
        );
        // An agent RiWork started may run under a launcher.
        assert_eq!(program(Some(Codex), "node", none), Program::Codex);
        assert_eq!(program(Some(Claude), "riwork", none), Program::Claude);
        assert_eq!(program(Some(Grok), "riwork", none), Program::Grok);
        assert_eq!(program(Some(Codex), "zsh", unasked), Program::Shell);
        assert_eq!(program(Some(Claude), "codex", unasked), Program::Codex);
    }

    #[test]
    fn codex_gets_one_paste_per_path_and_the_others_ghosttys_text() {
        let paths = [
            PathBuf::from("/tmp/Screen Shot.png"),
            PathBuf::from("/tmp/notes file.txt"),
            PathBuf::from("/tmp/b.JPG"),
            PathBuf::from("/tmp/c"),
        ];
        let paste = |text: &str| Input::Paste(text.to_owned());
        assert_eq!(
            path_input(Program::Codex, &paths),
            [
                paste("/tmp/Screen\\ Shot.png"),
                paste("/tmp/notes\\ file.txt "),
                paste("/tmp/b.JPG"),
                paste("/tmp/c"),
            ]
        );
        let joined = [paste(
            "/tmp/Screen\\ Shot.png /tmp/notes\\ file.txt /tmp/b.JPG /tmp/c",
        )];
        for program in [Program::Shell, Program::Claude, Program::Grok] {
            assert_eq!(path_input(program, &paths), joined);
        }
        assert!(path_input(Program::Codex, &[]).is_empty());
        assert!(path_input(Program::Shell, &[]).is_empty());
    }

    #[test]
    fn the_pasteboard_is_sorted_files_first_then_text_then_a_lone_picture() {
        let text =
            |text: &str| gpui::ClipboardItem::new_string(text.to_owned()).entries()[0].clone();
        let files = ClipboardEntry::ExternalPaths(ExternalPaths(
            [PathBuf::from("/tmp/a b.png")].into_iter().collect(),
        ));
        let picture = ClipboardEntry::Image(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            vec![1, 2, 3],
        ));
        // Finder puts a copied file's name beside its URL.
        assert_eq!(
            copied(&[files, text("a b.png")]),
            Copied::Files(vec![PathBuf::from("/tmp/a b.png")])
        );
        assert_eq!(copied(&[text("hello")]), Copied::Text);
        assert_eq!(copied(&[picture.clone()]), Copied::Picture);
        assert_eq!(copied(&[picture, text("caption")]), Copied::Text);
        assert_eq!(copied(&[]), Copied::Text);
    }

    #[test]
    fn a_lone_picture_is_ctrl_v_for_an_agent_and_nothing_for_a_shell() {
        assert_eq!(
            paste_input(&Copied::Picture, Program::Claude),
            Some(vec![Input::Key("C-v")])
        );
        assert_eq!(
            paste_input(&Copied::Picture, Program::Codex),
            Some(vec![Input::Key("C-v")])
        );
        assert_eq!(
            paste_input(&Copied::Picture, Program::Grok),
            Some(vec![Input::Key("C-v")])
        );
        assert_eq!(
            paste_input(&Copied::Picture, Program::Shell),
            Some(Vec::new())
        );
        assert_eq!(paste_input(&Copied::Text, Program::Claude), None);
        assert_eq!(
            paste_input(
                &Copied::Files(vec![PathBuf::from("/tmp/x")]),
                Program::Shell
            ),
            Some(vec![Input::Paste("/tmp/x".into())])
        );
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
