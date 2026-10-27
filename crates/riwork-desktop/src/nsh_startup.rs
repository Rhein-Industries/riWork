//! Process environment handling before the GUI starts any worker.
pub(crate) fn scrub_gui_child_ids() {
    // Main calls this before starting threads. CLI dispatch deliberately runs
    // first and preserves the invoking shell/chat identity.
    unsafe {
        std::env::remove_var("RIWORK_CHAT_ID");
        std::env::remove_var("RIWORK_SHELL_ID");
    }
}
