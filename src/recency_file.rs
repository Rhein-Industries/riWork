//! `project-recency.json`: when each project's files last changed, as the
//! Projects panel orders "Last edited", published under the RiWork data
//! directory so the CLI, and through it the phone companion, can sort by it.
//!
//! Only the app can know: the dates come from the scan and the file-change
//! events that live in its process (`project_recency`), and the CLI neither
//! scans nor watches. A project the app has not scanned yet, or that has never
//! had a file, has no entry. The dates are as of the last time the app ran; an
//! edit made while it was closed shows once it runs again.
//!
//! Kept free of GPUI, like `appearance_file`, so a test can compile it alone.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
use uuid::Uuid;

pub const FILE_NAME: &str = "project-recency.json";
/// A larger file is invalid. A real one is about 60 bytes per project.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;
pub const VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Published {
    pub v: u8,
    /// Unix seconds of the last change to the dates.
    pub updated_at: u64,
    /// Project id to the Unix second of its newest edit.
    pub projects: BTreeMap<String, u64>,
}

/// The published dates, or None when the file is missing, not a regular file,
/// too large, of another version or not a valid document. Dates of zero are
/// "unknown" and left out.
pub fn read(home: &Path) -> Option<Published> {
    let path = home.join(FILE_NAME);
    // A named pipe would block the open, so look before opening.
    let metadata = fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(&path)
        .ok()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let mut published: Published = serde_json::from_slice(&bytes).ok()?;
    if published.v != VERSION {
        return None;
    }
    published.projects.retain(|_, edited| *edited > 0);
    Some(published)
}

/// Publishes `edits` for the projects in `known`, stamped `now`, unless the file
/// already says the same. A known project the scan has no date for this time
/// keeps the date last published, so a root that was slow to scan does not make
/// a project drop out of the phone's order and back; a project that is no
/// longer known is dropped. Any number of app processes may call this: readers
/// see the old file or the whole new one. Returns whether it wrote.
pub fn publish<'a>(
    home: &Path,
    edits: &BTreeMap<String, u64>,
    known: impl IntoIterator<Item = &'a str>,
    now: u64,
) -> Result<bool, String> {
    let current = read(home);
    let mut projects = BTreeMap::new();
    for id in known {
        let date = edits
            .get(id)
            .copied()
            .filter(|edited| *edited > 0)
            .or_else(|| current.as_ref()?.projects.get(id).copied());
        if let Some(date) = date {
            projects.insert(id.to_owned(), date);
        }
    }
    if current.is_some_and(|current| current.projects == projects) {
        return Ok(false);
    }
    let document = Published {
        v: VERSION,
        updated_at: now,
        projects,
    };
    let mut data = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("Cannot encode project recency: {error}"))?;
    data.push(b'\n');
    if data.len() as u64 > MAX_BYTES {
        return Err("Project recency is too large to publish".into());
    }
    write_private_file(home, &data)?;
    Ok(true)
}

/// A new private (0600) file renamed into place, so readers see the old file or
/// the whole new one, never a partial write. The file is derived state that the
/// next scan publishes again, so it is not synced.
fn write_private_file(home: &Path, data: &[u8]) -> Result<(), String> {
    let path = home.join(FILE_NAME);
    let temporary = home.join(format!(".project-recency-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("Cannot create {}: {error}", temporary.display()))?;
        file.write_all(data)
            .map_err(|error| format!("Cannot write project recency: {error}"))?;
        fs::rename(&temporary, &path)
            .map_err(|error| format!("Cannot replace {}: {error}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("riwork-recency-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn edits(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs
            .iter()
            .map(|(id, at)| ((*id).to_owned(), *at))
            .collect()
    }

    #[test]
    fn dates_round_trip_through_a_private_atomically_written_file() {
        let home = home();
        assert!(read(&home).is_none());
        assert!(publish(&home, &edits(&[("a", 100), ("b", 200)]), ["a", "b"], 1_000).unwrap());
        let published = read(&home).unwrap();
        assert_eq!(published.v, 1);
        assert_eq!(published.updated_at, 1_000);
        assert_eq!(published.projects, edits(&[("a", 100), ("b", 200)]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(home.join(FILE_NAME))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // No temporary file is left behind.
        let leftovers: Vec<_> = fs::read_dir(&home)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn an_unchanged_map_is_not_written_again() {
        let home = home();
        publish(&home, &edits(&[("a", 100)]), ["a"], 1_000).unwrap();
        assert!(!publish(&home, &edits(&[("a", 100)]), ["a"], 2_000).unwrap());
        assert_eq!(read(&home).unwrap().updated_at, 1_000);
        assert!(publish(&home, &edits(&[("a", 150)]), ["a"], 3_000).unwrap());
        let published = read(&home).unwrap();
        assert_eq!(
            (published.updated_at, published.projects["a"]),
            (3_000, 150)
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_project_missing_from_one_scan_keeps_its_date_and_a_removed_one_goes() {
        let home = home();
        publish(&home, &edits(&[("a", 100), ("b", 200)]), ["a", "b"], 1_000).unwrap();
        // The scan of "b" did not finish this time; "a" moved back (its newest file was deleted).
        publish(&home, &edits(&[("a", 90)]), ["a", "b"], 2_000).unwrap();
        assert_eq!(
            read(&home).unwrap().projects,
            edits(&[("a", 90), ("b", 200)])
        );
        // "b" is no longer a project; "c" is new and unscanned.
        publish(&home, &edits(&[("a", 90)]), ["a", "c"], 3_000).unwrap();
        assert_eq!(read(&home).unwrap().projects, edits(&[("a", 90)]));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn invalid_files_read_as_nothing() {
        let home = home();
        let path = home.join(FILE_NAME);
        for text in [
            "",
            "not json",
            "[]",
            r#"{"v":2,"updated_at":1,"projects":{}}"#,
            r#"{"v":1,"updated_at":1,"projects":{"a":"soon"}}"#,
            r#"{"v":1,"updated_at":1,"projects":{"a":-5}}"#,
        ] {
            fs::write(&path, text).unwrap();
            assert!(read(&home).is_none(), "{text}");
        }
        // Zero means unknown.
        fs::write(&path, r#"{"v":1,"updated_at":1,"projects":{"a":0,"b":7}}"#).unwrap();
        assert_eq!(read(&home).unwrap().projects, edits(&[("b", 7)]));
        // A directory or an oversized file is not read.
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(read(&home).is_none());
        fs::remove_dir(&path).unwrap();
        fs::write(&path, vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert!(read(&home).is_none());
        let _ = fs::remove_dir_all(home);
    }
}
