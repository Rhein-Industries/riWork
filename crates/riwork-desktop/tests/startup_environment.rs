//! A separate single-thread process: no env mutation races with Rust tests.
#[path = "../src/nsh_startup.rs"]
mod nsh_startup;

fn main() {
    unsafe {
        std::env::set_var("RIWORK_CHAT_ID", "fixture-chat");
        std::env::set_var("RIWORK_SHELL_ID", "fixture-shell");
    }
    nsh_startup::scrub_gui_child_ids();
    assert!(std::env::var_os("RIWORK_CHAT_ID").is_none());
    assert!(std::env::var_os("RIWORK_SHELL_ID").is_none());
    println!("test startup::tests::gui_start_scrubs_child_ids ... ok");
    println!("test result: ok. 1 passed; 0 failed; 0 ignored");
}
