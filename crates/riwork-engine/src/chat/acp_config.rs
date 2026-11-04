//! Host-local ACP executable configuration. Arguments are literal argv, never shell code.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.command.as_os_str().is_empty() || !self.command.is_absolute() {
            return Err("ACP command must be an absolute executable path".into());
        }
        if self.command.as_os_str().as_encoded_bytes().contains(&0)
            || self.command.as_os_str().as_encoded_bytes().len() > 4096
            || self.args.len() > 128
            || self
                .args
                .iter()
                .any(|a| a.contains('\0') || a.len() > 32_768)
            || self.args.iter().map(String::len).sum::<usize>() > 64 * 1024
        {
            return Err("Invalid or oversized ACP executable arguments".into());
        }
        Ok(())
    }
}

fn read(path: &Path) -> Result<Config, String> {
    let file = fs::File::open(path)
        .map_err(|e| format!("Read ACP configuration {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 128 * 1024 {
        return Err("ACP configuration exceeds 128 KiB".into());
    }
    let config: Config =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid ACP configuration: {e}"))?;
    config.validate()?;
    Ok(config)
}

fn write(path: &Path, config: &Config) -> Result<(), String> {
    config.validate()?;
    let parent = path.parent().ok_or("ACP configuration has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!(".acp-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn load(home: &Path) -> Result<Config, String> {
    read(&home.join("acp.json")).map_err(|e| {
        format!("{e}. Configure with somtus chat acp configure --command /absolute/path --arg ARG")
    })
}

pub fn save(home: &Path, config: &Config) -> Result<(), String> {
    write(&home.join("acp.json"), config)
}

pub(super) fn bound(home: &Path, id: &str) -> Option<Config> {
    read(&super::log::chat_dir(home, id)?.join("acp-launch.json")).ok()
}

/// A persisted chat always resumes with its original executable and argv.
pub(super) fn for_chat(home: &Path, id: &str, resuming: bool) -> Result<Config, String> {
    let dir = super::log::chat_dir(home, id).ok_or("Invalid ACP chat identity")?;
    let binding = dir.join("acp-launch.json");
    if binding.exists() {
        return read(&binding);
    }
    if resuming {
        return Err(
            "ACP session has no saved executable binding; refusing to resume under another agent"
                .into(),
        );
    }
    let config = load(home)?;
    write(&binding, &config)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_arguments_and_pinned_session_survive_default_changes() {
        let home = std::env::temp_dir().join(format!("somtus-acp-config-{}", uuid::Uuid::new_v4()));
        let first = Config {
            command: "/bin/echo".into(),
            args: vec!["$(touch /tmp/never)".into(), "a b".into()],
        };
        save(&home, &first).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(for_chat(&home, &id, false).unwrap(), first);
        save(
            &home,
            &Config {
                command: "/bin/false".into(),
                args: vec![],
            },
        )
        .unwrap();
        assert_eq!(for_chat(&home, &id, true).unwrap(), first);
        assert!(for_chat(&home, &uuid::Uuid::new_v4().to_string(), true).is_err());
        assert!(
            Config {
                command: "echo bad".into(),
                args: vec![]
            }
            .validate()
            .is_err()
        );
        fs::remove_dir_all(home).unwrap();
    }
}
