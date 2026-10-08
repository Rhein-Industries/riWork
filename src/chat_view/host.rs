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

    /// History following resolves only an address; explicit delivery keeps normal ensure.
    pub(super) fn follow_existing(home: PathBuf) -> Ensure {
        Arc::new(move || Ok(socket_path(&home)))
    }

    /// Makes sure the host runs and says where it listens.
    pub fn ensure(&self) -> Result<PathBuf, String> {
        (self.ensure)()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_follow_only_resolves_an_address_without_ensuring_a_host() {
        let home = PathBuf::from("/synthetic-only/never-create-or-connect");
        let follow = HostConfig::follow_existing(home.clone());
        assert_eq!(follow().unwrap(), home.join("run/chat.sock"));
    }
}
