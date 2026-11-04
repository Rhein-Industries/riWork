//! Agent-friendly access to the same customization files and validation as the UI.
use super::{ensure_empty, print_json, take_single, write_stdout};
use riwork_core::{
    customization::{CustomizationSnapshot, CustomizationStore},
    customization_keys,
};
use serde_json::json;

const HELP: &str = "\
somtus customization show [--json]       Show effective layout, keys, and errors
somtus customization list [--json]       List valid UI layout files
somtus customization select NAME         Select a validated layout (default resets layout only)
somtus customization init                Create missing starter files and guide
somtus customization reset               Restore default UI and archive keyboard overrides
somtus customization commands [--json]   List keyboard defaults and recovery shortcut

Files live in RIWORK_HOME/ui (normally ~/.local/share/riwork/ui). Edit config.json,
keys.json, and layouts/NAME.json with an editor or agent. Running Somtus windows
reload valid edits automatically. A bad edit keeps their last valid configuration;
this independent CLI process reports its own safe fallback and the validation error.
The built-in default cannot be overwritten. Reset only changes UI customization;
workspace layouts, running sessions, and add-in permissions are preserved.
";

pub(super) fn command(mut args: Vec<String>, json_output: bool) -> Result<(), String> {
    let action = if args.is_empty() {
        "show".to_owned()
    } else {
        args.remove(0)
    };
    if matches!(action.as_str(), "help" | "--help" | "-h") {
        ensure_empty(&args)?;
        print!("{HELP}");
        return Ok(());
    }
    if action == "commands" {
        ensure_empty(&args)?;
        let commands = customization_keys::defaults();
        if json_output {
            print_json(
                &json!({"defaults":commands.into_iter().map(|(command,chord)|json!({"command":command,"chord":chord})).collect::<Vec<_>>(),"resetChord":customization_keys::RESET_CHORD}),
            )?;
        } else {
            for (command, chord) in commands {
                println!("{command}\t{chord}");
            }
            println!(
                "resetCustomization\t{} (reserved)",
                customization_keys::RESET_CHORD
            );
        }
        return Ok(());
    }
    let home = riwork_core::paths::riwork_home().map_err(|error| error.to_string())?;
    let mut store = CustomizationStore::open(home)?;
    let snapshot = run(&mut store, &action, args)?;
    if json_output {
        print_json(&snapshot)?;
    } else if action == "list" {
        for layout in &snapshot.layouts {
            println!(
                "{}\t{}{}",
                layout.id,
                layout.name,
                if layout.id == snapshot.selected {
                    " (selected)"
                } else {
                    ""
                }
            );
        }
    } else {
        println!(
            "Layout: {}\nDirectory: {}",
            snapshot.selected, snapshot.directory
        );
        if !snapshot.keys.is_empty() {
            println!("Keyboard overrides: {}", snapshot.keys.len());
        }
    }
    if let Some(error) = snapshot.error {
        if json_output {
            return Err("UI customization has invalid files; see error in JSON output".into());
        }
        return Err(error);
    }
    Ok(())
}
fn run(
    store: &mut CustomizationStore,
    action: &str,
    args: Vec<String>,
) -> Result<CustomizationSnapshot, String> {
    if action == "select" {
        return store.select(&take_single(args, "customization select NAME")?);
    }
    ensure_empty(&args)?;
    match action {
        "show" | "list" => Ok(store.snapshot()),
        "init" => store.initialize_files(),
        "reset" => store.reset(),
        _ => Err(format!(
            "Unknown customization action {action:?}. Run somtus customization help."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn customization_cli_rejects_invalid_selection_without_changing_saved_ui() {
        let directory =
            std::env::temp_dir().join(format!("somtus-ui-cli-{}", uuid::Uuid::new_v4()));
        let mut store = CustomizationStore::open(&directory).unwrap();
        run(&mut store, "init", vec![]).unwrap();
        assert_eq!(
            run(&mut store, "select", vec!["sidebar".into()])
                .unwrap()
                .selected,
            "sidebar"
        );
        assert!(run(&mut store, "select", vec![]).is_err());
        assert!(run(&mut store, "select", vec!["../bad".into()]).is_err());
        assert!(run(&mut store, "reset", vec!["extra".into()]).is_err());
        assert_eq!(run(&mut store, "show", vec![]).unwrap().selected, "sidebar");
        assert_eq!(
            run(&mut store, "reset", vec![]).unwrap().selected,
            "default"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
