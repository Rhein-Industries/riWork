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

pub(super) fn reported(provider: Provider, value: &Value) -> Option<String> {
    let paths: &[&str] = match provider {
        Provider::Claude => &[
            "/oauthAccount/accountUuid",
            "/account/uuid",
            "/account/accountUuid",
            "/account/id",
            "/accountUuid",
            "/account_uuid",
            "/oauthAccount/emailAddress",
            "/account/email",
            "/account/emailAddress",
            "/email",
        ],
        Provider::Codex => &[
            "/account/id",
            "/account/accountId",
            "/tokens/account_id",
            "/account/email",
            "/accountId",
            "/email",
        ],
    };
    paths.iter().find_map(|path| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                // Email casing is not an account change.
                hash(&format!("{provider:?}:{}", s.trim().to_lowercase()))
            })
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

pub(super) fn from_config(config: &DriverConfig) -> Option<String> {
    let home = env(config, "HOME").map(std::path::PathBuf::from);
    let directory = match config.provider {
        Provider::Claude => env(config, "CLAUDE_CONFIG_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| home.as_ref().map(|p| p.join(".claude"))),
        Provider::Codex => env(config, "CODEX_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| home.as_ref().map(|p| p.join(".codex"))),
    }?;
    match config.provider {
        Provider::Claude => {
            // oauthAccount survives token refreshes; prefer it to credential bytes.
            for path in [
                Some(directory.join(".claude.json")),
                home.filter(|p| directory == p.join(".claude"))
                    .map(|p| p.join(".claude.json")),
            ]
            .into_iter()
            .flatten()
            {
                if let Some(id) = json(&path).and_then(|v| reported(config.provider, &v)) {
                    return Some(id);
                }
            }
            let value = json(&directory.join(".credentials.json"))?;
            if let Some(id) = reported(config.provider, &value) {
                return Some(id);
            }
            let token = ["/claudeAiOauth/refreshToken", "/claudeAiOauth/accessToken"]
                .iter()
                .find_map(|path| {
                    value
                        .pointer(path)
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                })?;
            Some(hash(&format!("Claude:credential:{token}")))
        }
        Provider::Codex => {
            let value = json(&directory.join("auth.json"))?;
            if let Some(id) = reported(config.provider, &value) {
                return Some(id);
            }
            let key = value["OPENAI_API_KEY"]
                .as_str()
                .filter(|s| !s.trim().is_empty())?;
            Some(hash(&format!("Codex:credential:{key}")))
        }
    }
}

pub(super) fn read(dir: &Path) -> Option<String> {
    let identity: Option<String> =
        serde_json::from_slice(&fs::read(dir.join("account-identity.json")).ok()?).ok()?;
    identity.filter(|s| valid(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hashes_are_stable_private_and_change_with_login() {
        assert_eq!(hash("abc"), "sha256:ba7816bf8f01cfea414140de5dae2223");
        let first = reported(Provider::Claude, &serde_json::json!({"oauthAccount":{"accountUuid":"one","emailAddress":"secret@example.com"}})).unwrap();
        let second = reported(
            Provider::Claude,
            &serde_json::json!({"oauthAccount":{"accountUuid":"two"}}),
        )
        .unwrap();
        assert_ne!(first, second);
        assert!(valid(&first));
        assert!(!first.contains("secret"));
        assert!(
            reported(
                Provider::Claude,
                &serde_json::json!({"apiKeySource":"none"})
            )
            .is_none()
        );
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;
    use crate::chat::testkit::Fake;
    #[test]
    fn temp_credentials_change_login_scope_but_not_on_restart_or_token_refresh() {
        for provider in [Provider::Claude, Provider::Codex] {
            let fake = Fake::new(&[]);
            let config = fake.config(provider);
            let directory = std::path::PathBuf::from(
                env(
                    &config,
                    if provider == Provider::Claude {
                        "CLAUDE_CONFIG_DIR"
                    } else {
                        "CODEX_HOME"
                    },
                )
                .unwrap(),
            );
            let path = directory.join(if provider == Provider::Claude {
                ".credentials.json"
            } else {
                "auth.json"
            });
            let credentials = |account: &str, token: &str| match provider {
                Provider::Claude => {
                    serde_json::json!({"oauthAccount":{"accountUuid":account},"claudeAiOauth":{"refreshToken":token}})
                }
                Provider::Codex => {
                    serde_json::json!({"tokens":{"account_id":account,"refresh_token":token}})
                }
            };
            fs::write(
                &path,
                serde_json::to_vec(&credentials("account-a", "secret-a")).unwrap(),
            )
            .unwrap();
            let first = from_config(&config).unwrap();
            assert_eq!(from_config(&config).unwrap(), first);
            fs::write(
                &path,
                serde_json::to_vec(&credentials("account-a", "secret-refreshed")).unwrap(),
            )
            .unwrap();
            assert_eq!(from_config(&config).unwrap(), first);
            fs::write(
                &path,
                serde_json::to_vec(&credentials("account-b", "secret-b")).unwrap(),
            )
            .unwrap();
            assert_ne!(from_config(&config).unwrap(), first);
        }
    }
}

#[cfg(test)]
mod opaque_credential_tests {
    use super::*;
    use crate::chat::testkit::Fake;
    #[test]
    fn null_refresh_tokens_fall_back_to_access_tokens_and_empty_keys_have_no_scope() {
        let fake = Fake::new(&[]);
        let config = fake.config(Provider::Claude);
        let directory = std::path::PathBuf::from(env(&config, "CLAUDE_CONFIG_DIR").unwrap());
        fs::write(
            directory.join(".credentials.json"),
            br#"{"claudeAiOauth":{"refreshToken":null,"accessToken":"secret"}}"#,
        )
        .unwrap();
        assert_eq!(from_config(&config), Some(hash("Claude:credential:secret")));
        let config = fake.config(Provider::Codex);
        let directory = std::path::PathBuf::from(env(&config, "CODEX_HOME").unwrap());
        fs::write(directory.join("auth.json"), br#"{"OPENAI_API_KEY":""}"#).unwrap();
        assert!(from_config(&config).is_none());
    }
}
