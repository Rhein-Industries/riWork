//! Reaching the chat host from a window.

use std::{
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::chat::client::socket_path;

use super::feed::Ensure;

/// Make sure the chat host (`riwork chat serve`) is running for the data directory `home`,
/// and return the socket it listens on. A host that answers is used as it is; otherwise
/// `riwork chat ensure` starts one (detached, so it outlives this window).
pub fn ensure_host(home: &Path) -> Result<PathBuf, String> {
    let socket = socket_path(home);
    if UnixStream::connect(&socket).is_ok() {
        return Ok(socket);
    }
    crate::chat::host::ensure_host(home)
}

/// How a chat tab reaches the host.
#[derive(Clone)]
pub struct HostConfig {
    pub(super) ensure: Ensure,
}

impl HostConfig {
    /// The host of the RiWork data directory `home`.
    pub fn for_home(home: PathBuf) -> Self {
        Self {
            ensure: Arc::new(move || ensure_host(&home)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_view::testing::FakeHost;

    #[test]
    fn a_running_host_is_found_at_its_socket_and_a_missing_one_is_explained() {
        let host = FakeHost::with_chats(&[]);
        // The fake listens where `socket_path` would put the real one.
        let home = host.socket.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(home.join("run")).unwrap();
        let socket = socket_path(&home);
        assert_eq!(socket, home.join("run").join("chat.sock"));
        assert!(
            ensure_host(&home)
                .unwrap_err()
                .starts_with("The chat host is not running")
        );
        std::os::unix::fs::symlink(&host.socket, &socket).unwrap();
        assert_eq!(ensure_host(&home).unwrap(), socket);
    }
}
