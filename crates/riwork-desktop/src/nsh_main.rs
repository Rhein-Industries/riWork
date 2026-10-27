//! One executable: the existing headless CLI first, then the AppKit NSH host.
#[cfg(target_os = "macos")]
mod nsh_host;
#[cfg(target_os = "macos")]
mod nsh_menus;
#[cfg(target_os = "macos")]
mod nsh_native_stub;
#[cfg(unix)]
mod nsh_runtime;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match riwork_cli::cli::run_cli(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => exit(error),
    }
    // Only GUI launches detach from the invoking shell/chat identity.
    unsafe {
        std::env::remove_var("RIWORK_CHAT_ID");
        std::env::remove_var("RIWORK_SHELL_ID");
    }
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
