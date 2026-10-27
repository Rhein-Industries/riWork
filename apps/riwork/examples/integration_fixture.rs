//! Two Store projects with core-written layouts, entirely inside a fresh /tmp root.
#[path = "../tests/support/mod.rs"]
mod support;

use riwork_core::{layouts::LayoutStore, store::Store};
use std::path::PathBuf;

fn main() -> Result<(), String> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: integration_fixture /tmp/fresh-root")?,
    );
    let scratch = std::fs::canonicalize("/tmp").map_err(|error| error.to_string())?;
    if !root.is_absolute()
        || root
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return Err("fixture root must be an absolute /tmp path".into());
    }
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    if !root.starts_with(scratch) || root.join("home").exists() {
        return Err("fixture root must be inside /tmp with no existing home".into());
    }
    let home = root.join("home");
    let store = Store::open(&home)?;
    let layouts = LayoutStore::open(&home)?;
    // Acceptance can seed legacy published state through the production writer,
    // then prove the Phase 1 host leaves it byte-for-byte and mtime unchanged.
    if std::env::args_os().skip(2).any(|arg| arg == "--appearance") {
        use riwork_core::appearance_file::{self, PaletteColors, Published, Rgb};
        let palette = PaletteColors {
            bg: Rgb(0x101820),
            panel: Rgb(0x18232b),
            panel_active: Rgb(0x253640),
            divider: Rgb(0x334650),
            cyan: Rgb(0x75d5e8),
            magenta: Rgb(0xdb94db),
            gold: Rgb(0xe4c07a),
            text: Rgb(0xd3e1e6),
            muted: Rgb(0x8eabb5),
        };
        appearance_file::publish(&home, &Published::new(true, palette, None), 1)?;
    }
    let mut projects = Vec::new();
    for name in ["Alpha", "Beta"] {
        let path = root.join("repos").join(name);
        std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
        let project = store.add_project(&path, Some(name))?;
        let mut layout = support::fixture();
        layout.selected_task_id = Some(format!("{name}-fixture-task"));
        layouts.save_chat_state(&project.id, &layout, &[], None)?;
        projects.push(project);
    }
    println!(
        "{}",
        serde_json::json!({"root": root, "home": home, "projects": projects})
    );
    Ok(())
}
