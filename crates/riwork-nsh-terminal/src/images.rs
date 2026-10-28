//! Clipboard images belong to the terminal tab, rather than its replaceable surface.
use base64::Engine;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const LIMIT: usize = 24 * 1024 * 1024;
// Crash leftovers are removed only with proof that their owner PID is gone.
// Unknown process status, symlinks and foreign directory names are left alone.
pub(crate) fn cleanup_stale(home: &Path) {
    cleanup_stale_with(home, |pid| {
        std::process::Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "pid="])
            .output()
            .is_ok_and(|output| output.status.code() == Some(1) && output.stdout.is_empty())
    });
}
fn cleanup_stale_with(home: &Path, dead: impl Fn(u32) -> bool) {
    let Ok(entries) = fs::read_dir(home.join("tmp/terminal-images")) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some((pid, uuid)) = name.to_str().and_then(|name| name.split_once('-')) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        if pid != std::process::id() && uuid::Uuid::parse_str(uuid).is_ok() && dead(pid) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}
pub(crate) struct Images {
    root: PathBuf,
}
impl Images {
    pub(crate) fn new(home: &Path) -> Self {
        Self {
            root: home.join("tmp/terminal-images").join(format!(
                "{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            )),
        }
    }
    pub(crate) fn stage(&self, encoded: &str) -> Result<PathBuf, String> {
        if encoded.len() > LIMIT * 4 / 3 + 4 {
            return Err("Clipboard image exceeds 24 MiB".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "Clipboard image is not valid PNG data")?;
        if bytes.len() > LIMIT || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("Clipboard image is not valid PNG data or exceeds 24 MiB".into());
        }
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let path = self
            .root
            .join(format!("clipboard-{}.png", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| e.to_string())?;
        if let Err(error) = file.write_all(&bytes) {
            let _ = fs::remove_file(&path);
            return Err(error.to_string());
        }
        Ok(path)
    }
}
impl Drop for Images {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_paths_survive_surface_replacement_and_are_removed_with_the_tab() {
        let home = std::env::temp_dir().join(format!("nsh-images-{}", uuid::Uuid::new_v4()));
        let path;
        {
            let images = Images::new(&home);
            path = images
                .stage(
                    &base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture"),
                )
                .unwrap();
            assert!(path.starts_with(&home));
            assert_eq!(fs::read(&path).unwrap(), b"\x89PNG\r\n\x1a\nfixture");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        assert!(!path.exists());
        let _ = fs::remove_dir_all(home);
    }
    #[test]
    fn invalid_and_oversized_image_data_never_creates_a_directory() {
        let home = std::env::temp_dir().join(format!("nsh-images-{}", uuid::Uuid::new_v4()));
        let images = Images::new(&home);
        for data in ["invalid".to_owned(), "A".repeat(LIMIT * 4 / 3 + 8)] {
            assert!(images.stage(&data).is_err());
        }
        assert!(!home.exists());
    }
    #[test]
    fn stale_cleanup_requires_a_dead_owner_and_leaves_unrelated_paths() {
        let home = std::env::temp_dir().join(format!("nsh-images-{}", uuid::Uuid::new_v4()));
        let root = home.join("tmp/terminal-images");
        let dead = root.join(format!("1-{}", uuid::Uuid::new_v4()));
        let live = root.join(format!("2-{}", uuid::Uuid::new_v4()));
        let unrelated = root.join("other-directory");
        for path in [&dead, &live, &unrelated] {
            fs::create_dir_all(path).unwrap();
        }
        cleanup_stale_with(&home, |pid| pid == 1);
        assert!(!dead.exists());
        assert!(live.exists());
        assert!(unrelated.exists());
        let _ = fs::remove_dir_all(home);
    }
}
