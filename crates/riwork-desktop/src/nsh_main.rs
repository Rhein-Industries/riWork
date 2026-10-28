//! One executable: the existing headless CLI first, then the AppKit NSH host.
#[cfg(target_os = "macos")]
mod nsh_bundle;
#[cfg(target_os = "macos")]
mod nsh_chrome;
#[cfg(target_os = "macos")]
mod nsh_export;
#[cfg(target_os = "macos")]
mod nsh_host;
#[cfg(target_os = "macos")]
mod nsh_menus;
#[cfg(target_os = "macos")]
mod nsh_notifications;
#[cfg(unix)]
mod nsh_runtime;
mod nsh_startup;
#[cfg(all(target_os = "macos", feature = "terminal-fixture"))]
mod nsh_terminal_fixture;

fn main() {
    #[cfg(target_os = "macos")]
    if std::env::var_os("GHOSTTY_RESOURCES_DIR").is_none() {
        // Before any worker/runtime starts: set_var is safe only in this
        // single-threaded startup boundary, never during theme updates.
        unsafe {
            std::env::set_var(
                "GHOSTTY_RESOURCES_DIR",
                riwork_nsh_terminal::resources_dir(),
            );
        }
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match riwork_cli::cli::run_cli(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(_) if nsh_startup::missing_folder_request(&args) => {}
        Err(error) => exit(error),
    }
    nsh_startup::scrub_gui_child_ids();
    #[cfg(target_os = "macos")]
    if let Err(error) = nsh_host::run(args) {
        exit(error);
    }
    #[cfg(not(target_os = "macos"))]
    exit("The NSH desktop host requires macOS".into());
}

fn exit(error: String) -> ! {
    if !error.is_empty() {
        eprintln!("riwork: {error}");
    }
    std::process::exit(2)
}
