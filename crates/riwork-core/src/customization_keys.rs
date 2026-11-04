//! Public application commands and single-chord keyboard overrides.
//! Editing/chat shortcuts keep their context-specific owners.
use std::collections::BTreeMap;

pub type Bindings = BTreeMap<String, Option<String>>;
pub const RESET_CHORD: &str = "ctrl+alt+shift+escape";

/// Repeated commands have alternate defaults; an override replaces all of them.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("openSettings", "mod+,"),
    ("openProjectSettings", "mod+alt+a"),
    ("openSchedules", "mod+shift+s"),
    ("openFiles", "mod+shift+e"),
    ("openSessions", "mod+alt+s"),
    ("openPreview", "mod+shift+p"),
    ("gatherTabs", "mod+shift+m"),
    ("defaultLayout", "mod+alt+l"),
    ("newTab", "mod+t"),
    ("splitRight", "mod+d"),
    ("splitDown", "mod+shift+d"),
    ("closeTab", "mod+w"),
    ("closePane", "mod+shift+w"),
    ("openProjects", "mod+b"),
    ("toggleFocusMode", "mod+shift+f"),
    ("newProjectWindow", "mod+shift+n"),
    ("openProjectOrchestrator", "mod+alt+o"),
    ("selectTab1", "mod+1"),
    ("selectTab2", "mod+2"),
    ("selectTab3", "mod+3"),
    ("selectTab4", "mod+4"),
    ("selectTab5", "mod+5"),
    ("selectTab6", "mod+6"),
    ("selectTab7", "mod+7"),
    ("selectTab8", "mod+8"),
    ("selectTab9", "mod+9"),
    ("nextTab", "ctrl+tab"),
    ("previousTab", "ctrl+shift+tab"),
    ("nextTab", "mod+shift+]"),
    ("previousTab", "mod+shift+["),
    ("openGlobalDashboard", "mod+alt+g"),
    ("openGlobalChat", "mod+shift+o"),
    ("quit", "mod+q"),
    ("biggerText", "mod+="),
    ("biggerText", "mod+shift+="),
    ("smallerText", "mod+-"),
    ("actualSize", "mod+0"),
    ("newChat", "mod+alt+shift+c"),
    ("createProject", "mod+n"),
];
const UNBOUND: &[&str] = &[
    "newShell",
    "newSubshell",
    "openVim",
    "appPreview",
    "openTasks",
    "openUsage",
    "openAddins",
    "openCustomization",
    "focusCentered",
    "openGlobalOrchestrator",
];
const ACTION_ONLY: &[&str] = &[
    "resetCustomization",
    "selectTab",
    "selectPane",
    "switchProject",
];
const RESERVED: &[&str] = &[
    RESET_CHORD,
    "mod+c",
    "mod+x",
    "mod+v",
    "mod+a",
    "mod+z",
    "mod+shift+z",
    "mod+f",
    "mod+g",
    "mod+shift+g",
    "mod+.",
    "ctrl+alt+d",
    "mod+shift+v",
    "mod+alt+right",
    "mod+alt+left",
    "mod+shift+c",
    "mod+shift+l",
    "mod+shift+u",
    "shift+f10",
];

pub fn canonical_command(command: &str) -> Option<&'static str> {
    DEFAULTS
        .iter()
        .map(|(name, _)| *name)
        .chain(UNBOUND.iter().copied())
        .chain(ACTION_ONLY.iter().copied())
        .find(|name| *name == command)
}
pub fn validate_command(command: &str) -> Result<(), String> {
    canonical_command(command)
        .map(|_| ())
        .ok_or_else(|| format!("Unknown customization command {command:?}"))
}
/// Commands executable by the workspace frame dispatcher. Application-only
/// commands are handled by the native host and cannot be frame buttons.
pub fn validate_frame_command(command: &str) -> Result<(), String> {
    validate_command(command)?;
    if matches!(
        command,
        "quit"
            | "openGlobalDashboard"
            | "openGlobalChat"
            | "biggerText"
            | "smallerText"
            | "actualSize"
    ) {
        return Err(format!(
            "Command {command:?} is only available as a native application shortcut"
        ));
    }
    if matches!(command, "selectTab" | "selectPane" | "switchProject") {
        return Err(format!(
            "Command {command:?} requires a target and cannot be used as a command button"
        ));
    }
    Ok(())
}
pub fn defaults() -> Vec<(&'static str, &'static str)> {
    DEFAULTS.to_vec()
}

/// A restricted, shared subset of NSH's key syntax, also representable in AppKit menus.
pub fn normalize_chord(chord: &str) -> Result<String, String> {
    let fail = || {
        format!("Invalid shortcut {chord:?}: use modifiers plus one key, for example mod+shift+k")
    };
    let text = chord.trim().to_ascii_lowercase();
    let mut parts: Vec<_> = text.split('+').collect();
    let key = parts
        .pop()
        .filter(|part| !part.is_empty())
        .ok_or_else(fail)?;
    let key = match key {
        "esc" => "escape",
        "return" => "enter",
        "spacebar" => "space",
        value => value,
    };
    let function = key
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=12).contains(&n));
    let named = matches!(
        key,
        "escape"
            | "enter"
            | "tab"
            | "space"
            | "backspace"
            | "delete"
            | "left"
            | "right"
            | "up"
            | "down"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
    );
    let printable = key.len() == 1 && key.as_bytes()[0].is_ascii_graphic();
    if !function && !named && !printable {
        return Err(fail());
    }
    let mut mods = [false; 4];
    for part in parts {
        let index = match part {
            "mod" | "cmd" | "command" | "meta" | "super" => 0,
            "ctrl" | "control" => 1,
            "alt" | "option" => 2,
            "shift" => 3,
            _ => return Err(fail()),
        };
        if mods[index] {
            return Err(fail());
        }
        mods[index] = true;
    }
    // Plain typing/navigation stays with the focused control; function keys are safe alone.
    if !function && !mods[0] && !mods[1] && !mods[2] {
        return Err(fail());
    }
    let mut out: Vec<_> = ["mod", "ctrl", "alt", "shift"]
        .into_iter()
        .enumerate()
        .filter_map(|(i, name)| mods[i].then_some(name))
        .collect();
    out.push(key);
    Ok(out.join("+"))
}

pub fn normalize_bindings(bindings: &Bindings) -> Result<Bindings, String> {
    let mut normalized = Bindings::new();
    for (command, binding) in bindings {
        validate_command(command)?;
        if ACTION_ONLY.contains(&command.as_str()) {
            return Err(format!("{command} cannot be rebound"));
        }
        let binding = binding.as_deref().map(normalize_chord).transpose()?;
        if binding
            .as_deref()
            .is_some_and(|key| RESERVED.contains(&key))
        {
            return Err(format!(
                "Shortcut {} belongs to editing, chat, or customization recovery",
                binding.as_deref().unwrap()
            ));
        }
        normalized.insert(command.clone(), binding);
    }
    let mut used = BTreeMap::new();
    for (command, chord) in effective_bindings(&normalized) {
        if let Some(previous) = used.insert(chord.clone(), command.clone()) {
            if previous != command {
                return Err(format!(
                    "Shortcut {chord} is assigned to both {previous} and {command}; unbind or move the other command"
                ));
            }
        }
    }
    Ok(normalized)
}
pub fn validate_bindings(bindings: &Bindings) -> Result<(), String> {
    normalize_bindings(bindings).map(|_| ())
}

/// Caller supplies normalized, validated overrides.
pub fn effective_bindings(bindings: &Bindings) -> Vec<(String, String)> {
    DEFAULTS
        .iter()
        .filter(|(command, _)| !bindings.contains_key(*command))
        .map(|(command, chord)| (command.to_string(), chord.to_string()))
        .chain(bindings.iter().filter_map(|(command, chord)| {
            chord.as_ref().map(|chord| (command.clone(), chord.clone()))
        }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replaces_all_aliases_and_unbinds() {
        let keys = normalize_bindings(&Bindings::from([
            ("nextTab".into(), Some("Shift+CMD+k".into())),
            ("quit".into(), None),
        ]))
        .unwrap();
        let result = effective_bindings(&keys);
        assert_eq!(
            result
                .iter()
                .filter(|(command, _)| command == "nextTab")
                .collect::<Vec<_>>(),
            [&("nextTab".into(), "mod+shift+k".into())]
        );
        assert!(!result.iter().any(|(command, _)| command == "quit"));
    }
    #[test]
    fn rejects_conflicts_reserved_keys_and_typing() {
        for key in [
            "mod+c",
            RESET_CHORD,
            "mod+b",
            "mod+shift+=",
            "k",
            "shift+k",
            "mod+mod+k",
            "mod+f25",
        ] {
            assert!(
                normalize_bindings(&Bindings::from([("newTab".into(), Some(key.into()))])).is_err(),
                "{key}"
            );
        }
        assert!(
            normalize_bindings(&Bindings::from([
                ("newTab".into(), Some("mod+b".into())),
                ("openProjects".into(), None)
            ]))
            .is_ok()
        );
        assert!(
            normalize_bindings(&Bindings::from([("resetCustomization".into(), None)])).is_err()
        );
        assert!(normalize_bindings(&Bindings::from([("openCodex".into(), None)])).is_err());
    }
    #[test]
    fn frame_actions_exclude_native_only_commands() {
        for command in [
            "newShell",
            "newChat",
            "openFiles",
            "openTasks",
            "resetCustomization",
        ] {
            assert!(validate_frame_command(command).is_ok());
        }
        for command in [
            "quit",
            "openGlobalDashboard",
            "openGlobalChat",
            "biggerText",
            "smallerText",
            "actualSize",
            "selectTab",
            "selectPane",
            "switchProject",
        ] {
            assert!(validate_command(command).is_ok());
            assert!(validate_frame_command(command).is_err());
        }
    }
}
