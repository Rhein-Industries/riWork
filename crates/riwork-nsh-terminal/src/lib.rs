//! The frozen `riwork.terminal` component. Ghostty is a pinned C library;
//! session identity and all tmux semantics remain in riwork-engine.
mod config;
#[cfg(target_os = "macos")]
mod macos;
mod policy;
#[cfg(target_os = "macos")]
pub use macos::{pending_count, register};
/// The hash-verified private resources shipped alongside the pinned C library.
/// The host sets this before creating any threads; native config parsing then
/// resolves Ghostty's named themes without loading a system installation.
#[cfg(target_os = "macos")]
pub fn resources_dir() -> &'static str {
    env!("RIWORK_GHOSTTY_RESOURCES_DIR")
}
#[cfg(all(target_os = "macos", feature = "terminal-fixture"))]
pub use macos::{
    fixture_heartbeat, fixture_input, fixture_probe, fixture_snapshot, fixture_status,
};
