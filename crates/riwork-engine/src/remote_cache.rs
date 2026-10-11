//! What the window keeps about other Macs between runs: the project lists their folders show,
//! and which project, if any, was selected on one.
//!
//! The lists let a folder show what it listed last while its host is still unreachable, dimmed;
//! the selection lets a restart open the same remote project. Nothing secret is kept: pairing
//! material lives in `riwork-remote`'s own registry. The file is private all the same, as the
//! names of someone's projects are theirs.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::remote_tree::RemoteProject;

const FILE_NAME: &str = "remote-ui.json";

/// How many projects of one host are kept; a host with more lists the rest once it answers.
const MAX_PROJECTS_PER_HOST: usize = 500;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    /// The window's project id when it was on another Mac's project (`remote:{host}:{id}`).
    pub selected: Option<String>,
    pub hosts: BTreeMap<String, Vec<RemoteProject>>,
}

/// Reads the file. Anything unreadable, missing or from another version is simply nothing
/// kept: it is a cache, and the hosts will list their projects again.
pub fn load(home: &Path) -> Kept {
    let Ok(data) = fs::read(home.join(FILE_NAME)) else {
        return Kept::default();
    };
    let Ok(document) = serde_json::from_slice::<Value>(&data) else {
        return Kept::default();
    };
    if document.get("v").and_then(Value::as_u64) != Some(1) {
        return Kept::default();
    }
    let selected = document
        .get("selected")
        .and_then(Value::as_str)
        .filter(|key| crate::remote_tree::parse_project_key(key).is_some())
        .map(str::to_owned);
    let hosts = document
        .get("hosts")
        .and_then(Value::as_object)
        .map(|hosts| {
            hosts
                .iter()
                .map(|(id, projects)| {
                    let projects = projects
                        .as_array()
                        .map(|projects| {
                            projects
                                .iter()
                                .filter_map(|project| {
                                    Some(RemoteProject {
                                        id: project.get("id")?.as_str()?.to_owned(),
                                        name: project.get("name")?.as_str()?.to_owned(),
                                        created_at: project
                                            .get("created_at")
                                            .and_then(Value::as_u64)
                                            .unwrap_or(0),
                                    })
                                })
                                .take(MAX_PROJECTS_PER_HOST)
                                .collect()
                        })
                        .unwrap_or_default();
                    (id.clone(), projects)
                })
                .collect()
        })
        .unwrap_or_default();
    Kept { selected, hosts }
}

/// Writes the file: a new private (0600) file renamed into place, so a reader sees the old
/// file or the whole new one. It is a cache, so it is not synced.
pub fn save(home: &Path, kept: &Kept) -> Result<(), String> {
    let hosts = kept
        .hosts
        .iter()
        .map(|(id, projects)| {
            let projects = projects
                .iter()
                .take(MAX_PROJECTS_PER_HOST)
                .map(|project| {
                    json!({
                        "id": project.id,
                        "name": project.name,
                        "created_at": project.created_at,
                    })
                })
                .collect::<Vec<_>>();
            (id.clone(), Value::Array(projects))
        })
        .collect::<serde_json::Map<_, _>>();
    let document = json!({"v": 1, "selected": kept.selected, "hosts": hosts});
    let mut data = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("Cannot encode {FILE_NAME}: {error}"))?;
    data.push(b'\n');
    let path = home.join(FILE_NAME);
    let temporary = home.join(format!(".remote-ui-{}.tmp", Uuid::new_v4()));
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
        file.write_all(&data)
            .map_err(|error| format!("Cannot write {FILE_NAME}: {error}"))?;
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
    use std::path::PathBuf;

    fn home() -> PathBuf {
        let path = std::env::temp_dir().join(format!("riwork-remote-ui-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn project(id: &str, name: &str) -> RemoteProject {
        RemoteProject {
            id: id.into(),
            name: name.into(),
            created_at: 7,
        }
    }

    #[test]
    fn lists_and_the_selection_round_trip_in_a_private_file() {
        let home = home();
        let kept = Kept {
            selected: Some("remote:h1:p1".into()),
            hosts: BTreeMap::from([(
                "h1".into(),
                vec![project("p1", "app"), project("p2", "web")],
            )]),
        };
        save(&home, &kept).unwrap();
        assert_eq!(load(&home), kept);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(home.join(FILE_NAME))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Nothing but the file is left behind.
        assert_eq!(fs::read_dir(&home).unwrap().count(), 1);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_missing_odd_or_foreign_file_keeps_nothing() {
        let home = home();
        assert_eq!(load(&home), Kept::default());
        for contents in [
            "",
            "not json",
            r#"{"v":2,"selected":"remote:h:p","hosts":{}}"#,
            r#"[1,2,3]"#,
        ] {
            fs::write(home.join(FILE_NAME), contents).unwrap();
            assert_eq!(load(&home), Kept::default(), "{contents}");
        }
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn only_a_remote_project_key_counts_as_a_selection() {
        let home = home();
        for selected in ["p1", "remote:h1", "", "remote::p1"] {
            let document = json!({"v": 1, "selected": selected, "hosts": {}});
            fs::write(home.join(FILE_NAME), document.to_string()).unwrap();
            assert_eq!(load(&home).selected, None, "{selected}");
        }
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn entries_without_an_id_or_name_are_skipped() {
        let home = home();
        let document = json!({"v": 1, "selected": null, "hosts": {"h1": [
            {"id": "p1", "name": "app"}, {"name": "no id"}, {"id": "p3"}, 7
        ]}});
        fs::write(home.join(FILE_NAME), document.to_string()).unwrap();
        assert_eq!(
            load(&home).hosts["h1"],
            vec![RemoteProject {
                id: "p1".into(),
                name: "app".into(),
                created_at: 0
            }]
        );
        fs::remove_dir_all(home).unwrap();
    }
}
