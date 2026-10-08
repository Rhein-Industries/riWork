//! Account-wide sticky notice occurrences, owned by the single chat host.
use super::model::{Item, ItemBody, Provider, sticky_notice};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Key {
    provider: Provider,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item_id: Option<String>,
}

impl Key {
    fn for_item(provider: Provider, item: &Item) -> Option<Self> {
        let ItemBody::Notice {
            kind: Some(kind),
            resets_at,
            ..
        } = &item.body
        else {
            return None;
        };
        sticky_notice(Some(kind)).then(|| Self {
            provider,
            kind: kind.clone(),
            resets_at: *resets_at,
            item_id: resets_at.is_none().then(|| item.id.clone()),
        })
    }

    fn active(&self, now: u64) -> bool {
        self.resets_at.is_none_or(|reset| reset > now)
    }
}

pub(super) struct Dismissals {
    path: PathBuf,
    entries: Vec<Key>,
}

impl Dismissals {
    pub fn open(home: &Path) -> Result<Self, String> {
        let path = super::log::chats_dir(home).join("notice-dismissals.json");
        let entries = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("Cannot decode {}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("Cannot read {}: {e}", path.display())),
        };
        let mut store = Self { path, entries };
        store.prune(now())?;
        Ok(store)
    }

    // Commit memory only after the atomic file replacement succeeds.
    fn save(&mut self, entries: Vec<Key>) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&entries).map_err(|e| e.to_string())?;
        let dir = self.path.parent().expect("dismissals parent");
        let temporary = dir.join(format!(".notice-dismissals-{}.tmp", Uuid::new_v4()));
        let result = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)?;
            Ok(())
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Cannot write {}: {e}", self.path.display()));
        }
        self.entries = entries;
        Ok(())
    }

    pub fn prune(&mut self, now: u64) -> Result<(), String> {
        let entries: Vec<_> = self
            .entries
            .iter()
            .filter(|key| key.active(now))
            .cloned()
            .collect();
        if entries.len() != self.entries.len() {
            self.save(entries)?;
        }
        Ok(())
    }

    pub fn dismiss(&mut self, provider: Provider, item: &Item, now: u64) -> Result<(), String> {
        let key = Key::for_item(provider, item).ok_or("item is not a sticky notice")?;
        self.prune(now)?;
        if key.active(now) && !self.entries.contains(&key) {
            let mut entries = self.entries.clone();
            entries.push(key);
            self.save(entries)?;
        }
        Ok(())
    }

    /// The store is authoritative even when a driver resumes an old dismissed body.
    pub fn mark(&self, provider: Provider, item: &mut Item, now: u64) -> bool {
        let Some(key) = Key::for_item(provider, item) else {
            return false;
        };
        let value = key.active(now) && self.entries.contains(&key);
        let ItemBody::Notice { dismissed, .. } = &mut item.body else {
            unreachable!()
        };
        let changed = *dismissed != value;
        *dismissed = value;
        changed
    }
}

pub(super) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ItemStatus, NoticeLevel};

    fn notice(id: &str, kind: &str, reset: Option<u64>) -> Item {
        let mut body = ItemBody::notice(NoticeLevel::Warning, "notice", Some(kind));
        if let ItemBody::Notice { resets_at, .. } = &mut body {
            *resets_at = reset;
        }
        Item {
            id: id.into(),
            turn_id: None,
            status: ItemStatus::Completed,
            body,
            presentation: Default::default(),
        }
    }

    #[test]
    fn notice_dismissals_expire_and_new_occurrences_and_providers_are_independent() {
        let home = crate::chat::testing::short_home();
        fs::create_dir_all(home.join("chats")).unwrap();
        let mut store = Dismissals::open(&home).unwrap();
        let reset = now() + 1000;
        let mut first = notice("one", "rate_limit:seven_day", Some(reset));
        store.dismiss(Provider::Claude, &first, reset - 10).unwrap();
        assert!(store.mark(Provider::Claude, &mut first, reset - 1));
        assert!(!store.mark(
            Provider::Claude,
            &mut notice("two", "rate_limit:seven_day", Some(reset + 1)),
            reset - 1
        ));
        assert!(!store.mark(
            Provider::Codex,
            &mut notice("two", "rate_limit:seven_day", Some(reset)),
            reset - 1
        ));
        assert!(store.mark(Provider::Claude, &mut first, reset));
        assert!(matches!(
            first.body,
            ItemBody::Notice {
                dismissed: false,
                ..
            }
        ));
        store.prune(reset).unwrap();
        assert!(store.entries.is_empty());
        assert_eq!(fs::read_to_string(&store.path).unwrap(), "[]");
        let mut auth = notice("auth-one", "auth_required", None);
        store.dismiss(Provider::Claude, &auth, reset).unwrap();
        let reopened = Dismissals::open(&home).unwrap();
        assert!(reopened.mark(Provider::Claude, &mut auth, reset));
        assert!(!reopened.mark(
            Provider::Claude,
            &mut notice("auth-two", "auth_required", None),
            reset
        ));
        fs::remove_dir_all(home).unwrap();
    }
}
