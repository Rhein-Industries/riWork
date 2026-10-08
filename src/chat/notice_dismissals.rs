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
struct Entry {
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
    level: super::model::NoticeLevel,
}

// Files from the first provider-wide implementation had no account or severity.
#[derive(Deserialize)]
struct LegacyEntry {
    provider: Provider,
    kind: String,
    resets_at: Option<u64>,
    item_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredEntry {
    Current(Entry),
    Legacy(LegacyEntry),
}

fn prefix(provider: Provider, account: Option<&str>) -> String {
    let provider = match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    };
    format!("{provider}:{}|", account.unwrap_or("default"))
}

fn occurrence_key(
    provider: Provider,
    account: Option<&str>,
    kind: &str,
    reset: Option<u64>,
    id: &str,
) -> String {
    let occurrence = match reset {
        Some(reset) => format!("@{reset}"),
        None => format!("#{id}"),
    };
    format!("{}{kind}{occurrence}", prefix(provider, account))
}

fn severity(level: super::model::NoticeLevel) -> u8 {
    match level {
        super::model::NoticeLevel::Info => 0,
        super::model::NoticeLevel::Warning => 1,
        super::model::NoticeLevel::Error => 2,
    }
}

impl Entry {
    fn for_item(provider: Provider, account: Option<&str>, item: &Item) -> Option<Self> {
        let ItemBody::Notice {
            kind: Some(kind),
            resets_at,
            level,
            ..
        } = &item.body
        else {
            return None;
        };
        let account = Some(account?);
        sticky_notice(Some(kind)).then(|| Self {
            key: occurrence_key(provider, account, kind, *resets_at, &item.id),
            resets_at: *resets_at,
            level: *level,
        })
    }

    fn active(&self, now: u64) -> bool {
        self.resets_at.is_none_or(|reset| reset > now)
    }
}

fn read_entries(path: &Path) -> Result<Vec<Entry>, String> {
    let stored: Vec<StoredEntry> = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("Cannot decode {}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("Cannot read {}: {e}", path.display())),
    };
    Ok(stored
        .into_iter()
        .map(|entry| match entry {
            StoredEntry::Current(entry) => entry,
            // Conservatively treat an unrecorded severity as warning so an old close
            // can never suppress a newly blocking error. Legacy scope is default.
            StoredEntry::Legacy(old) => Entry {
                key: occurrence_key(
                    old.provider,
                    None,
                    &old.kind,
                    old.resets_at,
                    old.item_id.as_deref().unwrap_or(""),
                ),
                resets_at: old.resets_at,
                level: super::model::NoticeLevel::Warning,
            },
        })
        .collect())
}

/// Snapshots remain read-only, including when the host is not running.
pub(super) fn snapshot_keys(
    home: &Path,
    info: Option<&super::model::ChatInfo>,
) -> Result<Vec<String>, String> {
    let Some(info) = info else {
        return Ok(Vec::new());
    };
    let fingerprint =
        super::log::chat_dir(home, &info.id).and_then(|dir| super::account_identity::read(&dir));
    let Some(identity) = fingerprint.filter(|identity| identity.rank() > 0) else {
        return Ok(Vec::new());
    };
    let account = super::account_identity::persisted_scope(home, info.provider, &identity);
    let prefix = prefix(info.provider, Some(&account));
    let entries = read_entries(&super::log::chats_dir(home).join("notice-dismissals.json"))?;
    let mut keys: Vec<_> = entries
        .into_iter()
        .filter(|e| e.active(now()) && e.key.starts_with(&prefix))
        .map(|e| e.key)
        .collect();
    keys.sort();
    keys.dedup();
    Ok(keys)
}

pub(super) struct Dismissals {
    path: PathBuf,
    entries: Vec<Entry>,
    identities: Vec<(Provider, super::account_identity::Identity)>,
}

impl Dismissals {
    pub fn open(home: &Path) -> Result<Self, String> {
        let path = super::log::chats_dir(home).join("notice-dismissals.json");
        let entries = read_entries(&path)?;
        let identities = [Provider::Claude, Provider::Codex]
            .into_iter()
            .flat_map(|provider| {
                super::account_identity::known(home, provider)
                    .into_iter()
                    .map(move |identity| (provider, identity))
            })
            .collect();
        let mut store = Self {
            path,
            entries,
            identities,
        };
        // Startup does not discard default entries before login discovery can migrate them.
        let entries = store
            .entries
            .iter()
            .filter(|e| e.active(now()))
            .cloned()
            .collect::<Vec<_>>();
        if entries != store.entries {
            store.save(entries)?;
        }
        Ok(store)
    }

    // Commit memory only after the atomic file replacement succeeds.
    fn save(&mut self, entries: Vec<Entry>) -> Result<(), String> {
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

    fn canonical_scope(&self, provider: Provider, scope: &str) -> String {
        let known = self
            .identities
            .iter()
            .filter(|(p, _)| *p == provider)
            .map(|(_, id)| id.clone())
            .collect::<Vec<_>>();
        let identity = known
            .iter()
            .find(|id| id.scope == scope)
            .cloned()
            .unwrap_or_else(|| scope.to_owned().into());
        super::account_identity::scope(&identity, &known)
    }

    fn scopes(&self, provider: Provider) -> Vec<String> {
        let mut scopes: Vec<_> = self
            .identities
            .iter()
            .filter(|(p, id)| *p == provider && id.rank() > 0)
            .map(|(_, id)| self.canonical_scope(provider, &id.scope))
            .collect();
        scopes.sort();
        scopes.dedup();
        scopes
    }

    /// Rewrites only verified aliases toward the canonical id/email. Never id -> email.
    pub fn migrate(
        &mut self,
        provider: Provider,
        identity: &super::account_identity::Identity,
        managed_alias: Option<&str>,
    ) -> Result<(), String> {
        if identity.rank() == 0 {
            return Ok(());
        }
        let home = self
            .path
            .parent()
            .and_then(Path::parent)
            .expect("dismissals home");
        for known in super::account_identity::known(home, provider)
            .into_iter()
            .chain(std::iter::once(identity.clone()))
        {
            if !self.identities.contains(&(provider, known.clone())) {
                self.identities.push((provider, known));
            }
        }
        self.rewrite(managed_alias.map(|alias| (provider, alias, identity.scope.as_str())))
    }

    fn rewrite(&mut self, managed_alias: Option<(Provider, &str, &str)>) -> Result<(), String> {
        let mut entries: Vec<Entry> = Vec::new();
        for old in &self.entries {
            let mut entry = old.clone();
            for provider in [Provider::Claude, Provider::Codex] {
                let start = prefix(provider, Some(""));
                let Some(rest) = entry.key.strip_prefix(start.trim_end_matches('|')) else {
                    continue;
                };
                let Some((old_scope, occurrence)) = rest.split_once('|') else {
                    continue;
                };
                let scope = if old_scope == "default" {
                    let scopes = self.scopes(provider);
                    if scopes.len() > 1 {
                        entry.key.clear();
                        break;
                    }
                    scopes
                        .first()
                        .cloned()
                        .unwrap_or_else(|| old_scope.to_owned())
                } else if let Some((_, _, target)) =
                    managed_alias.filter(|(p, alias, _)| *p == provider && *alias == old_scope)
                {
                    target.to_owned()
                } else {
                    self.canonical_scope(provider, old_scope)
                };
                entry.key = format!("{}{occurrence}", prefix(provider, Some(&scope)));
                break;
            }
            if entry.key.is_empty() {
                continue;
            }
            if let Some(existing) = entries
                .iter_mut()
                .find(|existing| existing.key == entry.key)
            {
                if severity(entry.level) > severity(existing.level) {
                    existing.level = entry.level;
                }
            } else {
                entries.push(entry);
            }
        }
        if entries != self.entries {
            self.save(entries)?;
        }
        Ok(())
    }

    pub fn prune(&mut self, now: u64) -> Result<(), String> {
        self.rewrite(None)?;
        let entries: Vec<_> = self
            .entries
            .iter()
            .filter(|key| {
                key.active(now)
                    && ![Provider::Claude, Provider::Codex].into_iter().any(|p| {
                        key.key.starts_with(&prefix(p, Some("default")))
                            && self.scopes(p).len() != 1
                    })
            })
            .cloned()
            .collect();
        if entries.len() != self.entries.len() {
            self.save(entries)?;
        }
        Ok(())
    }

    pub fn dismiss(
        &mut self,
        provider: Provider,
        account: Option<&str>,
        item: &Item,
        now: u64,
    ) -> Result<(), String> {
        let account =
            account.ok_or("provider account identity is unavailable for persistent dismissal")?;
        let account = self.canonical_scope(provider, account);
        let key =
            Entry::for_item(provider, Some(&account), item).ok_or("item is not a sticky notice")?;
        self.prune(now)?;
        if key.active(now)
            && !self
                .entries
                .iter()
                .any(|e| e.key == key.key && severity(e.level) >= severity(key.level))
        {
            let mut entries = self.entries.clone();
            entries.retain(|e| e.key != key.key);
            entries.push(key);
            self.save(entries)?;
        }
        Ok(())
    }

    /// Escalation ends a lower-severity dismissal for this account and occurrence.
    pub fn forget_worsened(
        &mut self,
        provider: Provider,
        account: Option<&str>,
        item: &Item,
    ) -> Result<(), String> {
        let account = account.map(|scope| self.canonical_scope(provider, scope));
        let Some(key) = Entry::for_item(provider, account.as_deref(), item) else {
            return Ok(());
        };
        let entries: Vec<_> = self
            .entries
            .iter()
            .filter(|e| e.key != key.key || severity(e.level) >= severity(key.level))
            .cloned()
            .collect();
        if entries.len() != self.entries.len() {
            self.save(entries)?;
        }
        Ok(())
    }

    /// The store is authoritative even when a driver resumes an old dismissed body.
    pub fn mark(
        &self,
        provider: Provider,
        account: Option<&str>,
        item: &mut Item,
        now: u64,
    ) -> bool {
        let account = account.map(|scope| self.canonical_scope(provider, scope));
        let key = Entry::for_item(provider, account.as_deref(), item);
        let value = key.is_some_and(|key| {
            key.active(now)
                && self
                    .entries
                    .iter()
                    .any(|e| e.key == key.key && severity(e.level) >= severity(key.level))
        });
        let ItemBody::Notice { dismissed, .. } = &mut item.body else {
            return false;
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

    pub(super) fn notice(id: &str, kind: &str, reset: Option<u64>) -> Item {
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
        store
            .dismiss(Provider::Claude, Some("test-account"), &first, reset - 10)
            .unwrap();
        assert!(store.mark(
            Provider::Claude,
            Some("test-account"),
            &mut first,
            reset - 1
        ));
        assert!(!store.mark(
            Provider::Claude,
            Some("test-account"),
            &mut notice("two", "rate_limit:seven_day", Some(reset + 1)),
            reset - 1
        ));
        assert!(!store.mark(
            Provider::Codex,
            Some("test-account"),
            &mut notice("two", "rate_limit:seven_day", Some(reset)),
            reset - 1
        ));
        assert!(store.mark(Provider::Claude, Some("test-account"), &mut first, reset));
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
        store
            .dismiss(Provider::Claude, Some("test-account"), &auth, reset)
            .unwrap();
        let reopened = Dismissals::open(&home).unwrap();
        assert!(reopened.mark(Provider::Claude, Some("test-account"), &mut auth, reset));
        assert!(!reopened.mark(
            Provider::Claude,
            Some("test-account"),
            &mut notice("auth-two", "auth_required", None),
            reset
        ));
        fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod legacy_scope_tests {
    use super::*;
    #[test]
    fn old_default_keys_never_match_an_unknown_or_identified_login() {
        let home = crate::chat::testing::short_home();
        fs::create_dir_all(home.join("chats")).unwrap();
        let reset = now() + 1000;
        fs::write(home.join("chats/notice-dismissals.json"), serde_json::to_vec(&serde_json::json!([{"key":format!("claude:default|rate_limit:seven_day@{reset}"),"level":"error","resets_at":reset}])).unwrap()).unwrap();
        let store = Dismissals::open(&home).unwrap();
        let mut item = super::tests::notice("new", "rate_limit:seven_day", Some(reset));
        for scope in [None, Some("sha256:ba7816bf8f01cfea414140de5dae2223")] {
            assert!(!store.mark(Provider::Claude, scope, &mut item, now()));
        }
        fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod migration_tests {
    use super::super::{
        account_identity::{canonical, hash},
        testkit::Fake,
    };
    use super::*;
    use serde_json::json;

    fn write_credentials(config: &super::super::driver::DriverConfig, value: serde_json::Value) {
        let variable = if config.provider == Provider::Claude {
            "CLAUDE_CONFIG_DIR"
        } else {
            "CODEX_HOME"
        };
        let directory = config
            .env
            .iter()
            .find(|(name, _)| name == variable)
            .unwrap()
            .1
            .clone();
        fs::write(
            PathBuf::from(directory).join(if config.provider == Provider::Claude {
                ".credentials.json"
            } else {
                "auth.json"
            }),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn email_to_id_migration_preserves_dismissal_and_never_reverses() {
        let home = crate::chat::testing::short_home();
        fs::create_dir_all(home.join("chats")).unwrap();
        let fake = Fake::new(&[]);
        let config = fake.config(Provider::Codex);
        write_credentials(&config, json!({"account":{"email":"a@example.test"}}));
        let email = canonical(&config, None, None).unwrap();
        let mut store = Dismissals::open(&home).unwrap();
        let mut item = super::tests::notice("one", "rate_limit:codex", Some(now() + 3600));
        store
            .dismiss(Provider::Codex, Some(&email.scope), &item, now())
            .unwrap();
        write_credentials(
            &config,
            json!({"tokens":{"account_id":"a"},"account":{"email":"a@example.test"}}),
        );
        let id = canonical(&config, None, Some(&email)).unwrap();
        store.migrate(Provider::Codex, &id, None).unwrap();
        assert!(store.mark(Provider::Codex, Some(&id.scope), &mut item, now()));
        let bytes = fs::read(&store.path).unwrap();
        store.migrate(Provider::Codex, &id, None).unwrap();
        store.migrate(Provider::Codex, &email, None).unwrap();
        assert_eq!(fs::read(&store.path).unwrap(), bytes);
        assert!(
            store.entries[0]
                .key
                .starts_with(&prefix(Provider::Codex, Some(&id.scope)))
        );
        item.body = ItemBody::notice(
            super::super::model::NoticeLevel::Warning,
            "notice",
            Some("rate_limit:codex"),
        );
        let reset = store.entries[0].resets_at;
        if let ItemBody::Notice { resets_at, .. } = &mut item.body {
            *resets_at = reset;
        }
        assert!(Dismissals::open(&home).unwrap().mark(
            Provider::Codex,
            Some(&id.scope),
            &mut item,
            now()
        ));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn current_login_token_and_default_keys_migrate_atomically_and_idempotently() {
        let home = crate::chat::testing::short_home();
        fs::create_dir_all(home.join("chats")).unwrap();
        let fake = Fake::new(&[]);
        let config = fake.config(Provider::Claude);
        write_credentials(
            &config,
            json!({"oauthAccount":{"accountUuid":"a","emailAddress":"a@example.test"},"claudeAiOauth":{"refreshToken":"old-token"}}),
        );
        let identity = canonical(&config, None, None).unwrap();
        let reset = now() + 3600;
        let path = home.join("chats/notice-dismissals.json");
        let token = hash("Claude:credential:old-token");
        fs::write(&path, serde_json::to_vec(&json!([
            {"key":format!("claude:{token}|rate_limit:seven_day@{reset}"),"level":"warning","resets_at":reset},
            {"key":format!("claude:default|rate_limit:seven_day@{reset}"),"level":"error","resets_at":reset}
        ])).unwrap()).unwrap();
        let mut store = Dismissals::open(&home).unwrap();
        store.migrate(Provider::Claude, &identity, None).unwrap();
        assert_eq!(store.entries.len(), 1);
        assert_eq!(
            store.entries[0].level,
            super::super::model::NoticeLevel::Error
        );
        assert_eq!(
            store.entries[0].key,
            format!("claude:{}|rate_limit:seven_day@{reset}", identity.scope)
        );
        let bytes = fs::read(&path).unwrap();
        store.migrate(Provider::Claude, &identity, None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!String::from_utf8(bytes).unwrap().contains("old-token"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn ambiguous_or_unidentified_default_keys_are_dropped_when_pruned() {
        for accounts in [0, 2] {
            let home = crate::chat::testing::short_home();
            fs::create_dir_all(home.join("chats")).unwrap();
            let fake = Fake::new(&[]);
            let config = fake.config(Provider::Claude);
            let mut store = Dismissals::open(&home).unwrap();
            for id in 0..accounts {
                write_credentials(
                    &config,
                    json!({"oauthAccount":{"accountUuid":format!("account-{id}")}}),
                );
                store
                    .migrate(
                        Provider::Claude,
                        &canonical(&config, None, None).unwrap(),
                        None,
                    )
                    .unwrap();
            }
            store.entries.push(Entry {
                key: "claude:default|auth_required#old".into(),
                resets_at: None,
                level: super::super::model::NoticeLevel::Error,
            });
            store.prune(now()).unwrap();
            assert!(store.entries.is_empty());
            fs::remove_dir_all(home).unwrap();
        }
    }
}
