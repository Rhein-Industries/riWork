//! The frozen `riwork.terminal` component. Ghostty is a pinned C library;
//! session identity and all tmux semantics remain in riwork-engine.
mod config;
mod images;
#[cfg(target_os = "macos")]
mod macos;
mod policy;
#[cfg(target_os = "macos")]
pub use macos::{initialize_theme_projection, pending_count, register};
/// Packaged resources, or the private pinned resources for an unbundled build.
/// The host sets this before creating any threads; native config parsing then
/// resolves Ghostty's named themes without loading a system installation.
#[cfg(target_os = "macos")]
pub fn resources_dir() -> &'static str {
    static RESOURCES: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RESOURCES.get_or_init(|| {
        if let Ok(executable) = std::env::current_exe()
            && let Some(directory) = executable.parent()
        {
            if directory.file_name().is_some_and(|name| name == "MacOS")
                && let Some(contents) = directory.parent()
                && contents.file_name().is_some_and(|name| name == "Contents")
            {
                return contents.join("Resources/ghostty").to_string_lossy().into_owned();
            }
            let bundled = directory.join("RiWork.app/Contents/Resources/ghostty");
            if bundled.is_dir() {
                return bundled.to_string_lossy().into_owned();
            }
        }
        env!("RIWORK_GHOSTTY_RESOURCES_DIR").to_owned()
    }).as_str()
}
#[cfg(all(target_os = "macos", feature = "terminal-fixture"))]
pub use macos::{
    fixture_command, fixture_heartbeat, fixture_input, fixture_policy, fixture_probe,
    fixture_snapshot, fixture_status,
};
