//! Connection-bound viewport ownership; the root CLI retains the crash lease.
use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

pub struct Viewport {
    pub(crate) cli: PathBuf,
    pub(crate) device: String,
    pub(crate) connection: String,
    pub(crate) selected: Option<(String, u32, u32)>,
    /// The shell this connection last gave up on. Its client may not have heard of that,
    /// and then lets go of it in its own time, however long after: this connection pinned
    /// it, so that must keep working, as it does for a shell that died while pinned.
    pub(crate) given_up: Option<String>,
}
impl Viewport {
    pub fn new(cli: PathBuf, device: String) -> Self {
        Self {
            cli,
            device,
            connection: uuid::Uuid::new_v4().to_string(),
            selected: None,
            given_up: None,
        }
    }
    /// Stops holding the size of the selected shell. Whatever is left of the lease
    /// lapses by itself, so nothing needs to work for this to be done.
    pub(crate) fn give_up(&mut self) {
        self.given_up = self.selected.take().map(|(shell, _, _)| shell);
    }
    pub(crate) fn args(&self, method: &str, shell: &str) -> Vec<String> {
        vec![
            "shell".into(),
            method.into(),
            shell.into(),
            "--owner".into(),
            self.device.clone(),
            "--lease".into(),
            self.connection.clone(),
        ]
    }
}
impl Drop for Viewport {
    fn drop(&mut self) {
        if let Some((shell, _, _)) = self.selected.take() {
            // Runs on transport errors, cancellation/revocation and shutdown.
            // UUID ownership prevents this old connection clearing a new one.
            // A failed cleanup is recovered by the root CLI's 12-second lease.
            if let Ok(mut child) = Command::new(&self.cli)
                .args(self.args("resize-clear", &shell))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                // Reap while the connector stays alive; shutdown still leaves
                // this cleanup child and the independent watchdog able to finish.
                let _ = std::thread::Builder::new()
                    .name("viewport-cleanup".into())
                    .spawn(move || {
                        let _ = child.wait();
                    });
            }
        }
    }
}
