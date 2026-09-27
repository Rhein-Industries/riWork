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
}
impl Viewport {
    pub fn new(cli: PathBuf, device: String) -> Self {
        Self {
            cli,
            device,
            connection: uuid::Uuid::new_v4().to_string(),
            selected: None,
        }
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
