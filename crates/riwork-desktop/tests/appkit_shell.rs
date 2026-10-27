#[cfg(target_os = "macos")]
#[path = "support/appkit_shell_macos.rs"]
mod appkit_shell_macos;

fn main() {
    #[cfg(target_os = "macos")]
    appkit_shell_macos::run();
    #[cfg(not(target_os = "macos"))]
    panic!("The AppKit acceptance route requires macOS");
}
