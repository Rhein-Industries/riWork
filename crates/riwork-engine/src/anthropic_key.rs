//! The Anthropic API key that Claude chats on API key billing get (`ClaudeBilling::ApiKey`).
//! The key is kept in the login Keychain as one generic password (service `SERVICE`,
//! account `ACCOUNT`) and RiWork writes it nowhere else: never to `settings.json`, a chat's
//! info or events, a log or a test. Settings saves and removes it. The chat host reads it
//! at each start of a Claude chat on API key (never when the host itself starts) and gives
//! it to that `claude` as `ANTHROPIC_API_KEY` (`chat::launch`), as a key exported in the
//! shell would be: the commands Claude runs inherit it, so what they print (`env`) is the
//! agent's output. RiWork processes started from inside such a chat (a chat host, the app,
//! a tmux server) drop it again (`scrub_injected`).
//!
//! Everything goes through `KeyStore`, so a test has `MemoryStore`. In a test build
//! `Keychain` refuses every call: no test can reach the user's Keychain, even by mistake.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

/// Set beside `ANTHROPIC_API_KEY` when RiWork gave a Claude chat its saved key: the key in
/// this environment is RiWork's to hand out, not the user's own.
pub const INJECTED_MARKER: &str = "RIWORK_ANTHROPIC_KEY_INJECTED";

/// Removes a key RiWork gave a Claude chat (`INJECTED_MARKER`) from a command that starts a
/// new RiWork root: a chat host, the app or a tmux server started by a command the agent
/// ran. They must not take it for the user's own environment, hand it to Claude terminal
/// tabs, or keep handing it out after the key is removed in Settings. A key the user
/// exported is left alone.
pub fn scrub_injected(command: &mut Command) {
    scrub_injected_when(std::env::var_os(INJECTED_MARKER).is_some(), command);
}

fn scrub_injected_when(injected: bool, command: &mut Command) {
    if injected {
        command.env_remove("ANTHROPIC_API_KEY");
        command.env_remove(INJECTED_MARKER);
    }
}

/// The Keychain item's service, as Keychain Access shows it.
pub const SERVICE: &str = "dev.riwork.anthropic-api-key";
/// The Keychain item's account.
pub const ACCOUNT: &str = "claude-chats";
/// Far longer than any Anthropic key; a paste longer than this took more than a key.
const MAX_KEY: usize = 1024;

/// Where the key is kept.
pub trait KeyStore: Send + Sync {
    /// The saved key; `Ok(None)` when none is saved.
    fn read(&self) -> Result<Option<String>, String>;
    /// Whether a key is saved, without reading the key itself.
    fn is_saved(&self) -> Result<bool, String> {
        self.read().map(|key| key.is_some())
    }
    /// Saves `key`, which `normalized` accepted, in place of a saved one. A failure
    /// leaves the saved key, or none, as it was.
    fn save(&self, key: &str) -> Result<(), String>;
    /// Removes the saved key. Removing when none is saved is no error.
    fn remove(&self) -> Result<(), String>;
}

/// A pasted key as it is saved: without the whitespace around it. Refused when that
/// leaves nothing, or a key with whitespace, control or non-ASCII characters in it, or
/// longer than any key: a paste that took more than the key. The refusal never repeats
/// the paste.
pub fn normalized(pasted: &str) -> Result<String, String> {
    let key = pasted.trim();
    if key.is_empty() {
        return Err("Paste an Anthropic API key first.".into());
    }
    if key.len() > MAX_KEY || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(
            "That is not an API key: a key is one word of letters, digits and dashes.".into(),
        );
    }
    Ok(key.to_owned())
}

/// The login Keychain. Test builds never reach it.
pub struct Keychain;

/// macOS `errSecItemNotFound`: no such item.
const ITEM_NOT_FOUND: i32 = -25300;

impl Keychain {
    fn guard(&self) -> Result<(), String> {
        if cfg!(any(test, feature = "test-support")) {
            return Err("The Keychain is not used in tests.".into());
        }
        Ok(())
    }
}

/// "The Keychain could not save the API key: User interaction is not allowed."
fn failure(action: &str, error: security_framework::base::Error) -> String {
    let reason = error
        .message()
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| format!("error {}", error.code()));
    format!("The Keychain could not {action} the Anthropic API key: {reason}")
}

impl KeyStore for Keychain {
    fn read(&self) -> Result<Option<String>, String> {
        self.guard()?;
        match security_framework::passwords::get_generic_password(SERVICE, ACCOUNT) {
            Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| {
                "The Anthropic API key in the Keychain is not text; remove it in Settings and save it again."
                    .to_owned()
            }),
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
            Err(error) => Err(failure("read", error)),
        }
    }

    /// Asks for the item's attributes only, which needs no access to the key.
    fn is_saved(&self) -> Result<bool, String> {
        use security_framework::item::{ItemClass, ItemSearchOptions};
        self.guard()?;
        match ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(SERVICE)
            .account(ACCOUNT)
            .load_attributes(true)
            .limit(1)
            .search()
        {
            Ok(found) => Ok(!found.is_empty()),
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(false),
            Err(error) => Err(failure("look up", error)),
        }
    }

    fn save(&self, key: &str) -> Result<(), String> {
        self.guard()?;
        // Adds the item, or replaces the key of the one there: either all of it or nothing.
        security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, key.as_bytes())
            .map_err(|error| failure("save", error))
    }

    fn remove(&self) -> Result<(), String> {
        self.guard()?;
        match security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
            Err(error) => Err(failure("remove", error)),
        }
    }
}

/// A store in memory, for tests: the key it holds, and the failure the next calls meet.
#[derive(Default)]
pub struct MemoryStore {
    key: Mutex<Option<String>>,
    failure: Mutex<Option<String>>,
    read_failure: Mutex<Option<String>>,
    lookups: AtomicUsize,
}

impl MemoryStore {
    pub fn with_key(key: &str) -> Self {
        Self {
            key: Mutex::new(Some(key.to_owned())),
            ..Self::default()
        }
    }

    /// Every call fails with `message` until `fail(None)`, and changes nothing.
    pub fn fail(&self, message: Option<&str>) {
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = message.map(str::to_owned);
    }

    /// Reading the key fails with `message`, while whether one is saved can still be told:
    /// a Keychain whose item is there but will not give its secret (access denied).
    pub fn fail_reads(&self, message: Option<&str>) {
        *self
            .read_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = message.map(str::to_owned);
    }

    /// How often it was asked for the key or whether one is saved.
    pub fn lookups(&self) -> usize {
        self.lookups.load(Ordering::SeqCst)
    }

    fn check(&self) -> Result<(), String> {
        match &*self.failure.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(message) => Err(message.clone()),
            None => Ok(()),
        }
    }

    fn key(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.key.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl KeyStore for MemoryStore {
    fn read(&self) -> Result<Option<String>, String> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.check()?;
        if let Some(message) = &*self
            .read_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Err(message.clone());
        }
        Ok(self.key().clone())
    }

    fn is_saved(&self) -> Result<bool, String> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.check()?;
        Ok(self.key().is_some())
    }

    fn save(&self, key: &str) -> Result<(), String> {
        self.check()?;
        *self.key() = Some(key.to_owned());
        Ok(())
    }

    fn remove(&self) -> Result<(), String> {
        self.check()?;
        *self.key() = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMMY: &str = "sk-ant-dummy-0000";

    #[test]
    fn a_test_build_never_reaches_the_keychain() {
        for result in [
            Keychain.read().map(drop),
            Keychain.is_saved().map(drop),
            Keychain.save(DUMMY),
            Keychain.remove(),
        ] {
            assert_eq!(result.unwrap_err(), "The Keychain is not used in tests.");
        }
    }

    #[test]
    fn a_store_saves_reads_replaces_and_removes_one_key_and_a_failure_changes_nothing() {
        let store = MemoryStore::default();
        assert_eq!(store.read(), Ok(None));
        assert_eq!(store.is_saved(), Ok(false));
        // Removing none is no error.
        store.remove().unwrap();
        store.save(DUMMY).unwrap();
        assert_eq!(store.read().unwrap().as_deref(), Some(DUMMY));
        assert_eq!(store.is_saved(), Ok(true));
        store.save("sk-ant-dummy-1111").unwrap();
        assert_eq!(store.read().unwrap().as_deref(), Some("sk-ant-dummy-1111"));

        store.fail(Some(
            "The Keychain could not save the Anthropic API key: locked",
        ));
        assert!(store.save(DUMMY).unwrap_err().contains("locked"));
        assert!(store.remove().unwrap_err().contains("locked"));
        assert!(store.read().is_err());
        assert!(store.is_saved().is_err());
        store.fail(None);
        assert_eq!(store.read().unwrap().as_deref(), Some("sk-ant-dummy-1111"));
        store.remove().unwrap();
        assert_eq!(store.read(), Ok(None));
    }

    #[test]
    fn a_paste_is_trimmed_and_one_that_is_not_a_key_is_refused_without_repeating_it() {
        assert_eq!(normalized(&format!("  {DUMMY}\n")).unwrap(), DUMMY);
        for pasted in [
            "",
            "   \n",
            "sk-ant-dummy 0000",
            "export ANTHROPIC_API_KEY=sk-ant-dummy\nsecond line",
            "sk-ant-dummy\u{7}",
            "sk-ant-dümmy",
            &"x".repeat(MAX_KEY + 1),
        ] {
            let error = normalized(pasted).unwrap_err();
            assert!(
                pasted.trim().is_empty() || !error.contains(pasted.trim()),
                "{error}"
            );
        }
        assert!(normalized(&"x".repeat(MAX_KEY)).is_ok());
    }

    #[test]
    fn a_key_riwork_injected_is_dropped_for_a_new_riwork_root_and_one_the_user_exported_stays() {
        let removed = |command: &Command| {
            command
                .get_envs()
                .filter(|(_, value)| value.is_none())
                .map(|(name, _)| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let mut command = Command::new("/usr/bin/true");
        scrub_injected_when(true, &mut command);
        let mut gone = removed(&command);
        gone.sort();
        assert_eq!(gone, ["ANTHROPIC_API_KEY", INJECTED_MARKER]);
        let mut command = Command::new("/usr/bin/true");
        scrub_injected_when(false, &mut command);
        assert!(removed(&command).is_empty());
    }

    #[test]
    fn the_item_has_one_clear_name() {
        assert_eq!(
            (SERVICE, ACCOUNT),
            ("dev.riwork.anthropic-api-key", "claude-chats")
        );
    }
}
