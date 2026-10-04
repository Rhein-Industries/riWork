//! Keeps `appearance.json` (see `appearance_file`) current for the phone
//! companion. The colors depend on the theme choice, on Ghostty's parsed
//! configuration and on the RiWork terminal colors option, so this watches the
//! two globals that hold them instead of polling: the Ghostty config poll, the
//! settings page and other windows' settings changes all end in one of them.

use crate::{
    appearance_file::{self, Published},
    settings::Settings,
    theme::Appearance,
};
use gpui::{App, Global};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// The colors this process last put in the file, or found there already.
#[derive(Default)]
struct Sent(Option<Published>);

impl Global for Sent {}

/// Publishes now, then whenever the appearance or the settings change. Call
/// once, after the `Appearance` and `Settings` globals exist.
pub fn start(home: PathBuf, cx: &mut App) {
    publish(&home, cx);
    let appearance_home = home.clone();
    cx.observe_global::<Appearance>(move |cx| publish(&appearance_home, cx))
        .detach();
    cx.observe_global::<Settings>(move |cx| publish(&home, cx))
        .detach();
}

fn publish(home: &Path, cx: &mut App) {
    let (Some(settings), Some(appearance)) =
        (cx.try_global::<Settings>(), cx.try_global::<Appearance>())
    else {
        return;
    };
    let last = cx.try_global::<Sent>().and_then(|sent| sent.0.as_ref());
    let Some(snapshot) = next_snapshot(settings, appearance, last) else {
        return;
    };
    // Unchanged colors are not rewritten, so a restart, or another window or
    // app process publishing the same colors, leaves the file alone.
    match appearance_file::publish(home, &snapshot, unix_now()) {
        Ok(_) => cx.set_global(Sent(Some(snapshot))),
        // Retried at the next change.
        Err(error) => eprintln!("riwork appearance: {error}"),
    }
}

/// What to publish now, or None when there is nothing new to say.
fn next_snapshot(
    settings: &Settings,
    appearance: &Appearance,
    last: Option<&Published>,
) -> Option<Published> {
    // A new theme choice reaches the settings before the appearance is resolved
    // for it. Wait for the appearance instead of publishing the old colors.
    if appearance.selected != settings.theme {
        return None;
    }
    let snapshot = appearance.published(settings.terminal_colors_forced());
    if last.is_some_and(|last| last.same_colors(&snapshot)) {
        return None;
    }
    Some(snapshot)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeChoice;
    use gpui_libghostty::{TerminalColor, TerminalTheme};
    use std::fs;

    fn settings(theme: ThemeChoice, use_riwork_colors: bool) -> Settings {
        Settings {
            theme,
            use_riwork_colors,
            ..Settings::default()
        }
    }

    fn following_ghostty(theme: TerminalTheme) -> Appearance {
        let mut appearance = Appearance::resolve(ThemeChoice::RiWork, false);
        appearance.selected = ThemeChoice::Ghostty;
        appearance.terminal = None;
        appearance.ghostty = Some(theme);
        appearance
    }

    #[test]
    fn a_change_of_theme_option_or_ghostty_color_is_published_once() {
        let riwork = Appearance::resolve(ThemeChoice::RiWork, false);
        let first = next_snapshot(&settings(ThemeChoice::RiWork, false), &riwork, None).unwrap();
        assert!(first.dark);
        // Nothing new: the same colors again, in any window.
        assert_eq!(
            next_snapshot(&settings(ThemeChoice::RiWork, false), &riwork, Some(&first)),
            None
        );
        // The terminal colors option does not matter to a selected theme's terminals.
        assert_eq!(
            next_snapshot(&settings(ThemeChoice::RiWork, true), &riwork, Some(&first)),
            None
        );

        // A theme choice, once its appearance is resolved.
        let gruvbox = Appearance::resolve(ThemeChoice::GruvboxLight, false);
        let light = next_snapshot(
            &settings(ThemeChoice::GruvboxLight, false),
            &gruvbox,
            Some(&first),
        )
        .unwrap();
        assert!(!light.dark);
        assert_eq!(light.palette.bg, crate::appearance_file::Rgb(0xfbf1c7));

        // The settings moved on and the appearance has not caught up: wait.
        assert_eq!(
            next_snapshot(
                &settings(ThemeChoice::GruvboxLight, false),
                &riwork,
                Some(&light)
            ),
            None
        );
        assert_eq!(
            next_snapshot(&settings(ThemeChoice::RiWork, false), &gruvbox, None),
            None
        );

        // Following Ghostty: the option switches the terminal colors, and an edit
        // to any of its 16 colors, even one the app UI does not use, is a change.
        let mut theme = crate::theme::riwork_terminal_theme();
        theme.background = TerminalColor::new(0x28, 0x2c, 0x34);
        let ghostty = following_ghostty(theme);
        let own = next_snapshot(&settings(ThemeChoice::Ghostty, false), &ghostty, None).unwrap();
        let forced =
            next_snapshot(&settings(ThemeChoice::Ghostty, true), &ghostty, Some(&own)).unwrap();
        assert_ne!(own.terminal, forced.terminal);
        assert_eq!(own.palette, forced.palette);
        theme.palette[8] = TerminalColor::new(1, 2, 3);
        let edited = following_ghostty(theme);
        let after =
            next_snapshot(&settings(ThemeChoice::Ghostty, false), &edited, Some(&own)).unwrap();
        assert_eq!(after.terminal.unwrap().palette[8].hex(), "#010203");
    }

    #[test]
    fn a_sequence_of_changes_rewrites_the_file_only_when_the_colors_change() {
        let home =
            std::env::temp_dir().join(format!("riwork-appearance-sync-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let mut last: Option<Published> = None;
        let mut writes = Vec::new();
        let steps = [
            (ThemeChoice::RiWork, false),
            (ThemeChoice::RiWork, true),
            (ThemeChoice::Catppuccin, true),
            (ThemeChoice::Catppuccin, false),
            (ThemeChoice::GruvboxLight, false),
            (ThemeChoice::RiWork, false),
        ];
        for (index, (theme, option)) in steps.into_iter().enumerate() {
            let appearance = Appearance::resolve(theme, false);
            if let Some(snapshot) =
                next_snapshot(&settings(theme, option), &appearance, last.as_ref())
            {
                let wrote =
                    appearance_file::publish(&home, &snapshot, 1_000 + index as u64).unwrap();
                writes.push(wrote);
                last = Some(snapshot);
            }
        }
        // RiWork, Catppuccin, Gruvbox and RiWork again; the option changed nothing.
        assert_eq!(writes, [true, true, true, true]);
        let published = appearance_file::read(&home).unwrap();
        assert_eq!(published.updated_at, 1_005);
        assert!(published.dark);

        // A second process starting on the same colors finds them published.
        let again = Appearance::resolve(ThemeChoice::RiWork, false).published(false);
        assert!(!appearance_file::publish(&home, &again, 9_999).unwrap());
        assert_eq!(appearance_file::read(&home).unwrap().updated_at, 1_005);
        fs::remove_dir_all(home).unwrap();
    }
}
