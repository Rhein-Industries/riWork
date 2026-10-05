//! Files a paired phone sent to a shell or a chat (`file.upload` in the remote protocol).
//!
//! The remote connector (`remote/src/upload.rs`) writes each one to `RIWORK_HOME/uploads/TARGET/`,
//! where `TARGET` is the UUID of the shell or chat it was sent to, owner-only, under a name of its
//! own making, and removes them after a day, when their device is revoked and when the quota needs
//! room. The desktop removes a target's folder when the shell is closed or the chat deleted, so a
//! file never outlives what it was given to by much. Nothing here opens or runs them.

use std::path::{Path, PathBuf};

/// The folder in `RIWORK_HOME` that holds the inboxes. `remote/src/upload.rs` names it too.
pub const DIRECTORY: &str = "uploads";

/// The inbox of the shell or chat `target`, if `target` is a canonical UUID.
pub fn inbox(home: &Path, target: &str) -> Option<PathBuf> {
    let canonical = uuid::Uuid::parse_str(target).is_ok_and(|id| id.to_string() == target);
    canonical.then(|| home.join(DIRECTORY).join(target))
}

/// Remove the inbox of `target` and the files in it. Best effort: a shell is closed (or a chat
/// deleted) whether or not this succeeds, and the connector's sweep removes what is left.
pub fn remove(home: &Path, target: &str) {
    if let Some(inbox) = inbox(home, target)
        && real_dir(home)
        && real_dir(&home.join(DIRECTORY))
        && real_dir(&inbox)
    {
        let _ = std::fs::remove_dir_all(inbox);
    }
}

/// Cleanup must check ancestors as well as the leaf: remove_dir_all does not follow the leaf's symlink,
/// but would otherwise follow an intermediate uploads symlink.
fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_refuses_symlinked_inbox_roots_and_targets() {
        use std::os::unix::fs::symlink;
        let fixture =
            std::env::temp_dir().join(format!("riwork-inbox-containment-{}", uuid::Uuid::new_v4()));
        let home = fixture.join("home");
        let outside = fixture.join("outside");
        let target = "00000000-0000-4000-8000-0000000000aa";
        std::fs::create_dir_all(outside.join(target)).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let marker = outside.join(target).join("keep");
        std::fs::write(&marker, b"keep").unwrap();
        symlink(&outside, home.join(DIRECTORY)).unwrap();
        remove(&home, target);
        assert!(
            marker.is_file(),
            "cleanup must not traverse an uploads symlink"
        );
        std::fs::remove_file(home.join(DIRECTORY)).unwrap();
        std::fs::create_dir(home.join(DIRECTORY)).unwrap();
        symlink(outside.join(target), home.join(DIRECTORY).join(target)).unwrap();
        remove(&home, target);
        assert!(
            marker.is_file(),
            "cleanup must not traverse a target symlink"
        );
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn only_a_canonical_uuid_names_an_inbox_and_removing_takes_it_whole() {
        let home = std::env::temp_dir().join(format!("riwork-inbox-{}", uuid::Uuid::new_v4()));
        let target = "00000000-0000-4000-8000-0000000000aa";
        let inbox = inbox(&home, target).unwrap();
        assert_eq!(inbox, home.join("uploads").join(target));
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("photo-1a2b3c4d.jpg"), b"x").unwrap();
        // A sibling that is not this target's stays.
        let other = home
            .join("uploads")
            .join("00000000-0000-4000-8000-0000000000bb");
        std::fs::create_dir_all(&other).unwrap();
        for bad in [
            "..",
            "../x",
            "",
            "00000000-0000-4000-8000-0000000000AA",
            "x/..",
        ] {
            assert!(super::inbox(&home, bad).is_none(), "{bad}");
            remove(&home, bad);
        }
        assert!(home.join("uploads").is_dir());
        remove(&home, target);
        assert!(!inbox.exists());
        assert!(other.is_dir());
        remove(&home, target);
        let _ = std::fs::remove_dir_all(home);
    }
}
