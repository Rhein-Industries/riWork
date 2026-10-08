//! Private login fingerprints. Raw account identifiers and credentials never leave here.
use super::{driver::DriverConfig, model::Provider};
use serde_json::Value;
use std::{ffi::OsString, fs, path::Path};

#[link(name = "System")]
unsafe extern "C" {
    fn CC_SHA256(data: *const std::ffi::c_void, len: u32, digest: *mut u8) -> *mut u8;
}

pub(super) fn hash(identity: &str) -> String {
    let mut digest = [0u8; 32];
    // Account identifiers and credential files are bounded well below u32::MAX.
    let length = u32::try_from(identity.len()).expect("account identity too large");
    unsafe {
        CC_SHA256(identity.as_ptr().cast(), length, digest.as_mut_ptr());
    }
    format!(
        "sha256:{}",
        digest[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

pub(super) fn valid(identity: &str) -> bool {
    identity
        .strip_prefix("sha256:")
        .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Contains only hashes. Aliases are evidence for one-way migration, never new scopes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Identity {
    pub scope: String,
    pub account_id: Option<String>,
    pub email: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl From<String> for Identity {
    fn from(scope: String) -> Self {
        Self {
            scope,
            account_id: None,
            email: None,
            aliases: vec![],
        }
    }
}

impl Identity {
    pub fn valid(&self) -> bool {
        valid(&self.scope)
            && self.account_id.as_deref().is_none_or(valid)
            && self.email.as_deref().is_none_or(valid)
            && self.aliases.iter().all(|s| valid(s))
            && self
                .account_id
                .as_ref()
                .or(self.email.as_ref())
                .is_none_or(|scope| scope == &self.scope)
    }
    #[cfg(test)]
    pub(super) fn fixture(scope: String) -> Self {
        Self {
            account_id: Some(scope.clone()),
            email: None,
            aliases: vec![],
            scope,
        }
    }
    pub(super) fn rank(&self) -> u8 {
        if self.account_id.is_some() {
            2
        } else if self.email.is_some() {
            1
        } else {
            0
        }
    }
}

fn fields(provider: Provider, value: &Value) -> (Option<String>, Option<String>, Vec<String>) {
    let ids: &[&str] = match provider {
        Provider::Claude => &[
            "/oauthAccount/accountUuid",
            "/account/uuid",
            "/account/accountUuid",
            "/account/id",
            "/accountUuid",
            "/account_uuid",
            "/claudeAiOauth/accountUuid",
        ],
        Provider::Codex => &[
            "/account/id",
            "/account/accountId",
            "/tokens/account_id",
            "/accountId",
        ],
    };
    let emails = [
        "/oauthAccount/emailAddress",
        "/account/email",
        "/account/emailAddress",
        "/email",
        "/claudeAiOauth/email",
    ];
    let fingerprint = |path: &str| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(|s| hash(&format!("{provider:?}:{}", s.trim().to_lowercase())))
    };
    let id = ids.iter().find_map(|path| fingerprint(path));
    let email = emails.iter().find_map(|path| fingerprint(path));
    let aliases = ids
        .iter()
        .chain(emails.iter())
        .filter_map(|path| fingerprint(path))
        .collect();
    (id, email, aliases)
}

/// The only identity derivation. Live frames and credentials are considered together.
/// A known id survives an email-only response for the same login, but never a conflicting id/email.
pub(super) fn canonical(
    config: &DriverConfig,
    reported: Option<&Value>,
    previous: Option<&Identity>,
) -> Option<Identity> {
    let provider = config.provider;
    let documents = credentials(config);
    let (live_id, live_email, live_aliases) =
        reported.map(|v| fields(provider, v)).unwrap_or_default();
    let file_fields: Vec<_> = documents.iter().map(|v| fields(provider, v)).collect();
    let file_id = file_fields.iter().find_map(|(id, _, _)| id.clone());
    let file_email = file_fields
        .iter()
        .filter(|(id, _, _)| {
            !id.as_ref()
                .zip(file_id.as_ref())
                .is_some_and(|(a, b)| a != b)
        })
        .find_map(|(_, email, _)| email.clone());
    let conflicting_files = live_id
        .as_ref()
        .zip(file_id.as_ref())
        .is_some_and(|(a, b)| a != b)
        || live_email
            .as_ref()
            .zip(file_email.as_ref())
            .is_some_and(|(a, b)| a != b);
    let mut id = live_id.or_else(|| if conflicting_files { None } else { file_id });
    let email = live_email.or_else(|| if conflicting_files { None } else { file_email });
    let same_previous = previous.is_some_and(|old| {
        !id.as_ref()
            .zip(old.account_id.as_ref())
            .is_some_and(|(a, b)| a != b)
            && !email
                .as_ref()
                .zip(old.email.as_ref())
                .is_some_and(|(a, b)| a != b)
            && (id
                .as_ref()
                .zip(old.account_id.as_ref())
                .is_some_and(|(a, b)| a == b)
                || email
                    .as_ref()
                    .zip(old.email.as_ref())
                    .is_some_and(|(a, b)| a == b))
    });
    if same_previous && id.is_none() {
        id = previous.and_then(|old| old.account_id.clone());
    }
    // A verified email -> id association is durable even through logout/restart.
    let saved = env(config, "RIWORK_HOME")
        .map(|home| known(Path::new(&home), provider))
        .unwrap_or_default();
    if id.is_none() {
        let mut ids: Vec<_> = saved
            .iter()
            .filter(|old| {
                email.as_ref().is_some_and(|email| {
                    old.email.as_ref() == Some(email) || old.aliases.contains(email)
                })
            })
            .filter_map(|old| old.account_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        if ids.len() == 1 {
            id = ids.pop();
        }
    }
    // Organization UUID is a last resort when no account id or email is supplied.
    if id.is_none() && email.is_none() && provider == Provider::Claude {
        id = reported.into_iter().chain(documents.iter()).find_map(|v| {
            [
                "/oauthAccount/organizationUuid",
                "/organization/uuid",
                "/organizationUuid",
            ]
            .iter()
            .find_map(|path| {
                v.pointer(path)
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| hash(&format!("{provider:?}:{}", s.trim().to_lowercase())))
            })
        });
    }
    let scope = id.clone().or_else(|| email.clone())?;
    let mut aliases = live_aliases;
    if !conflicting_files {
        for (file_id, file_email, old) in &file_fields {
            if !file_id
                .as_ref()
                .zip(id.as_ref())
                .is_some_and(|(a, b)| a != b)
                && !file_email
                    .as_ref()
                    .zip(email.as_ref())
                    .is_some_and(|(a, b)| a != b)
            {
                aliases.extend(old.clone());
            }
        }
        // Reproduce token/API-key fingerprints ONLY to migrate this login's old keys.
        for (value, (file_id, file_email, _)) in documents.iter().zip(&file_fields) {
            if file_id
                .as_ref()
                .zip(id.as_ref())
                .is_some_and(|(a, b)| a != b)
                || file_email
                    .as_ref()
                    .zip(email.as_ref())
                    .is_some_and(|(a, b)| a != b)
            {
                continue;
            }
            for path in [
                "/claudeAiOauth/refreshToken",
                "/claudeAiOauth/accessToken",
                "/OPENAI_API_KEY",
            ] {
                if let Some(token) = value
                    .pointer(path)
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                {
                    aliases.push(hash(&format!("{provider:?}:credential:{token}")));
                }
            }
        }
    }
    if same_previous {
        if let Some(old) = previous {
            aliases.push(old.scope.clone());
            aliases.extend(old.aliases.clone());
        }
    }
    for old in saved.iter().filter(|old| old.scope == scope) {
        aliases.extend(old.aliases.clone());
    }
    aliases.push(scope.clone());
    aliases.sort();
    aliases.dedup();
    Some(Identity {
        scope,
        account_id: id,
        email: email.or_else(|| {
            if same_previous {
                previous.and_then(|old| old.email.clone())
            } else {
                None
            }
        }),
        aliases,
    })
}

fn env(config: &DriverConfig, name: &str) -> Option<OsString> {
    if config.env_remove.iter().any(|key| key == name) {
        return None;
    }
    if let Some((_, value)) = config.env.iter().rev().find(|(key, _)| key == name) {
        return Some(value.clone());
    }
    if cfg!(test) {
        return None;
    }
    std::env::var_os(name)
}

fn json(path: &Path) -> Option<Value> {
    // Never read unbounded files or interact with a keychain or running provider.
    if fs::metadata(path).ok()?.len() > 1024 * 1024 {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn credentials(config: &DriverConfig) -> Vec<Value> {
    let home = env(config, "HOME").map(std::path::PathBuf::from);
    let directory = match config.provider {
        Provider::Claude => env(config, "CLAUDE_CONFIG_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| home.as_ref().map(|p| p.join(".claude"))),
        Provider::Codex => env(config, "CODEX_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| home.as_ref().map(|p| p.join(".codex"))),
    };
    let Some(directory) = directory else {
        return vec![];
    };
    let paths = match config.provider {
        Provider::Claude => vec![
            Some(directory.join(".credentials.json")),
            Some(directory.join(".claude.json")),
            home.filter(|p| directory == p.join(".claude"))
                .map(|p| p.join(".claude.json")),
        ],
        Provider::Codex => vec![Some(directory.join("auth.json"))],
    };
    paths
        .into_iter()
        .flatten()
        .filter_map(|path| json(&path))
        .collect()
}

fn records(dir: &Path) -> (Option<Identity>, Vec<Identity>) {
    let Some(value) = fs::read(dir.join("account-identity.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return (None, vec![]);
    };
    let decode = |value: Value| {
        let identity = if let Some(scope) = value.as_str() {
            Some(Identity::from(scope.to_owned()))
        } else {
            serde_json::from_value::<Identity>(value).ok()
        };
        identity.filter(Identity::valid)
    };
    if value.get("active").is_some() {
        let active = decode(value["active"].clone());
        let known = value["known"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| decode(value.clone()))
            .filter(|id| id.rank() > 0)
            .collect();
        (active, known)
    } else {
        let active = decode(value);
        let known = active.iter().filter(|id| id.rank() > 0).cloned().collect();
        (active, known)
    }
}

pub(super) fn read(dir: &Path) -> Option<Identity> {
    records(dir).0
}

/// Preserve verified account associations across logout and source changes.
/// Active may be null, but history is hashes only and can never grant a login scope.
pub(super) fn record(dir: &Path, identity: Option<&Identity>) -> Value {
    let (_, mut known) = records(dir);
    if let Some(identity) = identity.filter(|id| id.rank() > 0) {
        if let Some(old) = known.iter_mut().find(|old| old.scope == identity.scope) {
            let mut aliases = old.aliases.clone();
            aliases.extend(identity.aliases.clone());
            aliases.sort();
            aliases.dedup();
            *old = identity.clone();
            old.aliases = aliases;
        } else {
            known.push(identity.clone());
        }
    }
    serde_json::json!({"active":identity,"known":known})
}

pub(super) fn known(home: &Path, provider: Provider) -> Vec<Identity> {
    let Ok(entries) = fs::read_dir(super::log::chats_dir(home)) else {
        return vec![];
    };
    entries
        .flatten()
        .flat_map(|entry| {
            let Some(info) = fs::read(entry.path().join("info.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<super::model::ChatInfo>(&bytes).ok())
            else {
                return vec![];
            };
            if info.provider != provider {
                return vec![];
            }
            records(&entry.path()).1
        })
        .collect()
}

/// Persisted aliases let stopped chats/snapshots use an id learned by another chat.
/// Ranking prevents id -> email migration, irrespective of discovery order.
pub(super) fn scope(identity: &Identity, known: &[Identity]) -> String {
    let mut candidates: Vec<_> = known
        .iter()
        .filter(|other| other.rank() > identity.rank() && other.aliases.contains(&identity.scope))
        .collect();
    let rank = candidates.iter().map(|other| other.rank()).max();
    candidates.retain(|other| Some(other.rank()) == rank);
    let mut scopes: Vec<_> = candidates.iter().map(|other| other.scope.clone()).collect();
    scopes.sort();
    scopes.dedup();
    if scopes.len() == 1 {
        scopes.pop().unwrap()
    } else {
        identity.scope.clone()
    }
}

pub(super) fn for_config(config: &DriverConfig) -> Option<Identity> {
    let previous = env(config, "RIWORK_HOME").and_then(|home| {
        env(config, "RIWORK_CHAT_ID")
            .and_then(|id| {
                id.to_str()
                    .and_then(|id| super::log::chat_dir(Path::new(&home), id))
            })
            .and_then(|dir| read(&dir))
    });
    canonical(config, None, previous.as_ref())
}

pub(super) fn persisted_scope(home: &Path, provider: Provider, identity: &Identity) -> String {
    scope(identity, &known(home, provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::testkit::Fake;
    use serde_json::json;

    fn write(config: &DriverConfig, value: Value) {
        let (variable, name) = if config.provider == Provider::Codex {
            ("CODEX_HOME", "auth.json")
        } else {
            ("CLAUDE_CONFIG_DIR", ".credentials.json")
        };
        let directory = std::path::PathBuf::from(env(config, variable).unwrap());
        fs::write(directory.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    }

    #[test]
    fn codex_id_wins_in_both_source_orders_and_survives_email_only_replies() {
        let fake = Fake::new(&[]);
        let config = fake.config(Provider::Codex);
        write(&config, json!({"tokens":{"account_id":"account-a"}}));
        let email = json!({"account":{"email":"Private@Example.test"}});
        for live_first in [false, true] {
            let first = canonical(&config, live_first.then_some(&email), None).unwrap();
            let later = canonical(&config, Some(&email), Some(&first)).unwrap();
            assert_eq!(first.scope, hash("Codex:account-a"));
            assert_eq!(first.scope, later.scope);
            write(&config, json!({"account":{"email":"private@example.test"}}));
            let without_id = canonical(&config, Some(&email), Some(&later)).unwrap();
            assert_eq!(without_id.scope, later.scope);
            write(&config, json!({"tokens":{"account_id":"account-a"}}));
        }
    }

    #[test]
    fn claude_account_fields_survive_rotation_and_different_accounts_do_not_match() {
        let fake = Fake::new(&[]);
        let config = fake.config(Provider::Claude);
        let credentials = |id, token| json!({"oauthAccount":{"accountUuid":id,"emailAddress":format!("{id}@example.test")},"claudeAiOauth":{"refreshToken":token,"accessToken":token}});
        write(&config, credentials("a", "old-token"));
        let old = canonical(&config, None, None).unwrap();
        write(&config, credentials("a", "new-token"));
        let fresh = canonical(&config, None, None).unwrap();
        assert_eq!(old.scope, fresh.scope);
        assert_eq!(
            canonical(
                &config,
                Some(&json!({"account":{"email":"a@example.test"}})),
                Some(&old)
            )
            .unwrap()
            .scope,
            old.scope
        );
        write(&config, credentials("b", "other-token"));
        assert_ne!(
            canonical(&config, None, Some(&old)).unwrap().scope,
            old.scope
        );
        write(
            &config,
            json!({"claudeAiOauth":{"refreshToken":"only-a-token"}}),
        );
        assert!(canonical(&config, None, None).is_none());
    }

    #[test]
    fn conflicting_live_account_never_inherits_other_accounts_aliases() {
        let fake = Fake::new(&[]);
        let config = fake.config(Provider::Codex);
        write(
            &config,
            json!({"tokens":{"account_id":"a"},"account":{"email":"a@example.test"}}),
        );
        let old = canonical(&config, None, None).unwrap();
        let new = canonical(
            &config,
            Some(&json!({"account":{"id":"b","email":"b@example.test"}})),
            Some(&old),
        )
        .unwrap();
        assert_ne!(old.scope, new.scope);
        assert!(!new.aliases.contains(&old.scope));
        assert!(!new.aliases.contains(&hash("Codex:a@example.test")));
    }
}
