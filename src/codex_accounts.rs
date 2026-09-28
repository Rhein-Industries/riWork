//! Saved account discovery without reading, moving, or rewriting credentials.
//!
//! Orca's public CLI supplies account metadata. Its managed homes stay owned by
//! Orca; RiWork only points a newly launched Codex process at the selected home.

use std::{
    collections::HashSet,
    env, fs,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SYSTEM_DEFAULT_ID: &str = "system-default";
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_ACCOUNTS: usize = 128;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const CACHE_NAME: &str = "codex-accounts.json";

#[derive(Clone, Debug)]
pub struct CodexAccount {
    pub id: String,
    pub label: String,
    /// Email supplied by Orca's public account list, if it is usable.
    pub email: Option<String>,
    pub home: PathBuf,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub is_system_default: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AccountsSnapshot {
    pub accounts: Vec<CodexAccount>,
    pub error: Option<String>,
    pub source_active_id: Option<String>,
    pub from_cache: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexAccountBinding {
    pub id: Option<String>,
    pub label: Option<String>,
    pub email: Option<String>,
    pub home: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountMetadata {
    id: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    workspace_label: Option<String>,
    #[serde(default)]
    managed_home_runtime: Option<String>,
    #[serde(default)]
    wsl_distro: Option<String>,
}

// Unknown fields are ignored: a newer build may add some, and rejecting them
// would report a different Orca profile and block every Codex launch.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct AccountCache {
    version: u32,
    user_data: PathBuf,
    accounts: Vec<AccountMetadata>,
    source_active_id: Option<String>,
}

#[derive(Deserialize)]
struct AccountResponse {
    ok: bool,
    result: Option<AccountResult>,
}

#[derive(Deserialize)]
struct AccountResult {
    codex: CodexResponse,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexResponse {
    accounts: Vec<AccountMetadata>,
    #[serde(default)]
    active_account_id: Option<String>,
    #[serde(default)]
    active_account_ids_by_runtime: ActiveAccountIds,
}

#[derive(Default, Deserialize)]
struct ActiveAccountIds {
    host: Option<String>,
}

/// Run this bounded discovery on the background executor, not the UI thread.
pub fn discover() -> AccountsSnapshot {
    match default_state_home() {
        Ok(home) => discover_in(&home),
        Err(error) => snapshot(None, Some(error), false),
    }
}

pub fn discover_in(state_home: &Path) -> AccountsSnapshot {
    let result = (|| {
        let user_data = orca_user_data()?;
        let output = run_orca_account_list(&installed_orca_cli()?)?;
        let cache = parse_accounts(&output, user_data)?;
        write_cache(state_home, &cache)?;
        Ok(cache)
    })();
    match result {
        Ok(cache) => snapshot(Some(cache), None, false),
        Err(error) => match read_cache(state_home) {
            Ok(cache) => snapshot(
                Some(cache),
                Some(format!("{error} Showing previously discovered accounts.")),
                true,
            ),
            Err(_) => snapshot(None, Some(error), false),
        },
    }
}

/// Resolves only saved metadata for launches. A bad selection never changes
/// identities by silently falling back to the system account.
pub fn resolve_launch_binding(
    state_home: &Path,
    selected: Option<&str>,
) -> Result<CodexAccountBinding, String> {
    let Some(selected) = selected.filter(|id| *id != SYSTEM_DEFAULT_ID) else {
        return Ok(CodexAccountBinding {
            id: None,
            label: None,
            email: None,
            home: default_codex_home()?,
        });
    };
    if !safe_account_id(selected) {
        return Err("The selected Codex account is invalid. Choose an account in Settings.".into());
    }
    let cache = read_cache(state_home).map_err(|_| {
        "The selected Codex account is unavailable. Refresh accounts in Settings before launching."
            .to_owned()
    })?;
    resolve_cached_binding(&cache, selected)
}

pub fn resolve_project_launch_binding(
    state_home: &Path,
    project_id: &str,
) -> Result<CodexAccountBinding, String> {
    let store = crate::store::Store::open(state_home)?;
    let state = store.snapshot()?;
    let project = state.project(project_id)?;
    let app_selected = if matches!(
        project.codex_account,
        crate::store::ProjectCodexAccount::Inherit
    ) {
        crate::settings::SettingsStore::open(state_home)?
            .load()?
            .selected_codex_account
    } else {
        None
    };
    resolve_project_choice_binding(state_home, &project.codex_account, app_selected.as_deref())
}

pub fn resolve_project_choice_binding(
    state_home: &Path,
    choice: &crate::store::ProjectCodexAccount,
    app_selected: Option<&str>,
) -> Result<CodexAccountBinding, String> {
    use crate::store::ProjectCodexAccount;
    match choice {
        ProjectCodexAccount::Inherit => resolve_launch_binding(state_home, app_selected),
        ProjectCodexAccount::SystemDefault => resolve_launch_binding(state_home, None),
        ProjectCodexAccount::Saved(id) => resolve_launch_binding(state_home, Some(id)),
    }
}

fn resolve_cached_binding(
    cache: &AccountCache,
    selected: &str,
) -> Result<CodexAccountBinding, String> {
    let account = cache
        .accounts
        .iter()
        .find(|account| account.id == selected && host_account(account))
        .ok_or("The selected Codex account was not found. Choose an account in Settings.")?;
    let home = managed_home(&cache.user_data, &account.id)?;
    Ok(CodexAccountBinding {
        id: Some(account.id.clone()),
        label: Some(account_label(account)),
        email: account_email(account),
        home,
    })
}

fn snapshot(
    cache: Option<AccountCache>,
    mut error: Option<String>,
    from_cache: bool,
) -> AccountsSnapshot {
    let mut accounts = Vec::new();
    match default_codex_home() {
        Ok(home) => accounts.push(CodexAccount {
            id: SYSTEM_DEFAULT_ID.into(),
            label: "System default".into(),
            email: None,
            home,
            available: true,
            unavailable_reason: None,
            is_system_default: true,
        }),
        Err(reason) => error = Some(reason),
    }
    let source_active_id = cache
        .as_ref()
        .and_then(|cache| cache.source_active_id.clone());
    if let Some(cache) = cache {
        for metadata in cache.accounts {
            let result = managed_home(&cache.user_data, &metadata.id);
            let home = cache
                .user_data
                .join("codex-accounts")
                .join(&metadata.id)
                .join("home");
            accounts.push(CodexAccount {
                label: account_label(&metadata),
                email: account_email(&metadata),
                id: metadata.id,
                home: result.as_ref().cloned().unwrap_or(home),
                available: result.is_ok(),
                unavailable_reason: result.err(),
                is_system_default: false,
            });
        }
    }
    AccountsSnapshot {
        accounts,
        error,
        source_active_id,
        from_cache,
    }
}

fn account_label(account: &AccountMetadata) -> String {
    let email =
        clean_label(account.email.as_deref()).unwrap_or_else(|| "Saved Codex account".into());
    match clean_label(account.workspace_label.as_deref()) {
        Some(workspace) => format!("{email} · {workspace}"),
        None => email,
    }
}

fn account_email(account: &AccountMetadata) -> Option<String> {
    clean_label(account.email.as_deref()).filter(|email| {
        let (local, domain) = email.split_once('@').unwrap_or(("", ""));
        !local.is_empty() && domain.contains('.') && !email.chars().any(char::is_whitespace)
    })
}

fn clean_label(value: Option<&str>) -> Option<String> {
    let label: String = value?
        .chars()
        .filter(|character| !character.is_control())
        .take(160)
        .collect();
    let label = label.trim();
    (!label.is_empty()).then(|| label.to_owned())
}

fn safe_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id != SYSTEM_DEFAULT_ID
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn host_account(account: &AccountMetadata) -> bool {
    account.managed_home_runtime.as_deref() == Some("host")
        && account
            .wsl_distro
            .as_deref()
            .is_none_or(|distro| distro.is_empty())
}

fn parse_accounts(bytes: &[u8], user_data: PathBuf) -> Result<AccountCache, String> {
    // Never surface serde's raw text, CLI stderr, or provider errors: responses
    // may contain unrelated provider metadata and authentication diagnostics.
    let response: AccountResponse = serde_json::from_slice(bytes)
        .map_err(|_| "Orca returned an invalid account list.".to_owned())?;
    if !response.ok {
        return Err("Orca could not list saved accounts. Open Orca locally and refresh.".into());
    }
    let result = response
        .result
        .ok_or("Orca returned an incomplete account list.")?
        .codex;
    if result.accounts.len() > MAX_ACCOUNTS {
        return Err("Orca returned too many accounts.".into());
    }
    let mut ids = HashSet::new();
    let accounts: Vec<_> = result
        .accounts
        .into_iter()
        .filter(|account| safe_account_id(&account.id) && host_account(account))
        .filter(|account| ids.insert(account.id.clone()))
        .map(|mut account| {
            account.email = clean_label(account.email.as_deref());
            account.workspace_label = clean_label(account.workspace_label.as_deref());
            account
        })
        .collect();
    let active = result
        .active_account_ids_by_runtime
        .host
        .or(result.active_account_id);
    let source_active_id = active.filter(|id| accounts.iter().any(|account| &account.id == id));
    Ok(AccountCache {
        version: 1,
        user_data,
        accounts,
        source_active_id,
    })
}

fn managed_home(user_data: &Path, id: &str) -> Result<PathBuf, String> {
    if !safe_account_id(id) || !user_data.is_absolute() {
        return Err("This saved account has an invalid home location.".into());
    }
    let root = user_data
        .canonicalize()
        .map_err(|_| "The Orca account directory is unavailable.".to_owned())?;
    let expected = root.join("codex-accounts").join(id).join("home");
    let home = user_data.join("codex-accounts").join(id).join("home");
    if !home.is_dir() {
        return Err(
            "This saved account's home is missing. Restore or sign in through Orca.".into(),
        );
    }
    let canonical = home
        .canonicalize()
        .map_err(|_| "This saved account's home cannot be accessed.".to_owned())?;
    if canonical != expected {
        return Err(
            "This saved account's home points outside its managed account directory.".into(),
        );
    }
    Ok(canonical)
}

fn default_state_home() -> Result<PathBuf, String> {
    match env::var_os("RIWORK_HOME") {
        Some(home) => Ok(PathBuf::from(home)),
        None => Ok(user_home()?.join(".local/share/riwork")),
    }
}

fn user_home() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "The user home directory is unavailable.".into())
}

pub fn default_codex_home() -> Result<PathBuf, String> {
    match env::var_os("CODEX_HOME") {
        Some(home) if !home.is_empty() => absolute_path(PathBuf::from(home)),
        Some(_) => Err("CODEX_HOME is empty. Set a valid home before launching Codex.".into()),
        None => Ok(user_home()?.join(".codex")),
    }
}

fn absolute_path(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path);
    }
    env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|_| "The account home cannot be resolved from this directory.".into())
}

fn orca_user_data() -> Result<PathBuf, String> {
    // These paths match Orca's installed public CLI runtime/metadata.js. Honor
    // its profile override so metadata and managed homes refer to one instance.
    if let Some(path) = env::var_os("ORCA_USER_DATA_PATH").filter(|path| !path.is_empty()) {
        return absolute_path(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    return Ok(user_home()?.join("Library/Application Support/orca"));
    #[cfg(target_os = "windows")]
    return env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("orca"))
        .ok_or_else(|| "Orca's profile directory is unavailable.".into());
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Ok(env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or(user_home()?.join(".config"))
        .join("orca"));
}

fn installed_orca_cli() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        let path = PathBuf::from("/Applications/Orca.app/Contents/Resources/bin/orca");
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(
        "Orca's installed CLI is unavailable. Previously discovered accounts can still be used."
            .into(),
    )
}

fn read_cache(state_home: &Path) -> Result<AccountCache, String> {
    let path = state_home.join(CACHE_NAME);
    let metadata = fs::symlink_metadata(&path).map_err(|_| "No saved account metadata.")?;
    if !metadata.is_file() || metadata.len() > MAX_OUTPUT as u64 {
        return Err("Invalid account metadata cache.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("The account metadata cache is not private.".into());
        }
    }
    let file = fs::File::open(path).map_err(|_| "Cannot open saved account metadata.")?;
    let mut bytes = Vec::new();
    file.take((MAX_OUTPUT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read saved account metadata.")?;
    if bytes.len() > MAX_OUTPUT {
        return Err("Invalid account metadata cache.".into());
    }
    let cache: AccountCache =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid account metadata cache.")?;
    if cache.version != 1
        || cache.user_data != orca_user_data()?
        || cache.accounts.len() > MAX_ACCOUNTS
    {
        return Err(
            "Saved accounts belong to a different Orca profile. Refresh accounts in Settings."
                .into(),
        );
    }
    let mut ids = HashSet::new();
    if cache.accounts.iter().any(|account| {
        !safe_account_id(&account.id) || !host_account(account) || !ids.insert(&account.id)
    }) {
        return Err("Invalid account metadata cache.".into());
    }
    Ok(cache)
}

fn write_cache(state_home: &Path, cache: &AccountCache) -> Result<(), String> {
    fs::create_dir_all(state_home).map_err(|_| "Cannot save account metadata.")?;
    let temporary = state_home.join(format!(".codex-accounts-{}.tmp", Uuid::new_v4()));
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
            .map_err(|_| "Cannot save account metadata.")?;
        serde_json::to_writer_pretty(&mut file, cache)
            .map_err(|_| "Cannot save account metadata.")?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_all())
            .map_err(|_| "Cannot save account metadata.")?;
        fs::rename(&temporary, state_home.join(CACHE_NAME))
            .map_err(|_| "Cannot save account metadata.")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn run_orca_account_list(cli: &Path) -> Result<Vec<u8>, String> {
    let mut command = local_account_command(cli);
    let mut child = command
        .spawn()
        .map_err(|_| "Cannot run Orca's installed CLI.")?;
    let overflow = Arc::new(AtomicBool::new(false));
    let (send, recv) = mpsc::channel();
    read_output(
        child.stdout.take().unwrap(),
        overflow.clone(),
        send.clone(),
        true,
    );
    read_output(child.stderr.take().unwrap(), overflow.clone(), send, false);
    let started = Instant::now();
    let status = loop {
        if overflow.load(Ordering::Relaxed) || started.elapsed() >= COMMAND_TIMEOUT {
            terminate(&mut child);
            return Err("Orca account discovery timed out or exceeded its output limit. Open Orca locally and refresh.".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(15)),
            Err(_) => {
                terminate(&mut child);
                return Err("Orca account discovery could not complete.".into());
            }
        }
    };
    let mut output = Vec::new();
    for _ in 0..2 {
        match recv.recv_timeout(Duration::from_secs(1)) {
            Ok((stdout, Ok(bytes))) => {
                if stdout {
                    output = bytes;
                }
            }
            _ => {
                terminate(&mut child);
                return Err("Orca account discovery could not read its response.".into());
            }
        }
    }
    if overflow.load(Ordering::Relaxed) || !status.success() {
        return Err("Orca could not list saved accounts. Open Orca locally and refresh.".into());
    }
    Ok(output)
}

fn local_account_command(cli: &Path) -> Command {
    let mut command = Command::new(cli);
    command
        .args(["account", "list", "--json"])
        .env_remove("ORCA_ENVIRONMENT")
        .env_remove("ORCA_PAIRING_CODE")
        .env_remove("ORCA_REMOTE_PAIRING")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

fn read_output(
    mut reader: impl Read + Send + 'static,
    overflow: Arc<AtomicBool>,
    sender: mpsc::Sender<(bool, std::io::Result<Vec<u8>>)>,
    stdout: bool,
) {
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0; 8192];
        let result = loop {
            match reader.read(&mut chunk) {
                Ok(0) => break Ok(output),
                Ok(count) => {
                    let remaining = MAX_OUTPUT.saturating_sub(output.len());
                    output.extend_from_slice(&chunk[..count.min(remaining)]);
                    if count > remaining {
                        overflow.store(true, Ordering::Relaxed);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send((stdout, result));
    });
}

fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        // Only this CLI process group; never Orca's already-running application.
        unsafe {
            kill(-(child.id() as i32), 9);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = env::temp_dir().join(format!("riwork-accounts-test-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn response(accounts: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"ok":true,"result":{"codex":{
            "accounts":accounts,"activeAccountId":"local-1",
            "activeAccountIdsByRuntime":{"host":"local-2"}
        },"unrelatedProvider":{"authToken":"NEVER_CACHE_ME"}}}))
        .unwrap()
    }

    #[test]
    fn cache_from_a_newer_build_with_extra_fields_still_parses() {
        let cache: AccountCache = serde_json::from_value(serde_json::json!({
            "version": 1,
            "user_data": "/profile",
            "accounts": [{"id": "local-1", "email": null, "futureField": {"nested": true}}],
            "source_active_id": null,
            "refreshed_at": 7
        }))
        .unwrap();
        assert_eq!(cache.accounts[0].id, "local-1");
    }

    #[test]
    fn keeps_only_unique_host_accounts_and_safe_ids() {
        let bytes = response(serde_json::json!([
            {"id":"local-1","email":"first@example.test","managedHomeRuntime":"host"},
            {"id":"local-2","email":"second@example.test","managedHomeRuntime":"host"},
            {"id":"local-2","email":"duplicate@example.test","managedHomeRuntime":"host"},
            {"id":"wsl","managedHomeRuntime":"wsl","wslDistro":"Ubuntu"},
            {"id":"remote","managedHomeRuntime":"ssh"},
            {"id":"unknown"},
            {"id":"../escape","managedHomeRuntime":"host"}
        ]));
        let parsed = parse_accounts(&bytes, PathBuf::from("/profile")).unwrap();
        assert_eq!(parsed.accounts.len(), 2);
        assert_eq!(parsed.source_active_id.as_deref(), Some("local-2"));
        let cache = serde_json::to_string(&parsed).unwrap();
        assert!(!cache.contains("NEVER_CACHE_ME"));
        assert!(!cache.contains("authToken"));
    }

    #[test]
    fn account_ids_cannot_escape_or_conflict_with_default() {
        for id in ["", ".", "..", "a/b", "a\\b", "a\n", SYSTEM_DEFAULT_ID] {
            assert!(!safe_account_id(id));
        }
        assert!(safe_account_id("948f08ad-ec46-4f87-806e-3afb2a5fd8c4"));
    }

    #[test]
    fn homes_are_only_existing_contained_directories() {
        let fixture = Fixture::new();
        assert!(managed_home(&fixture.0, "local-1").is_err());
        let path = fixture.0.join("codex-accounts/local-1/home");
        fs::create_dir_all(&path).unwrap();
        assert_eq!(
            managed_home(&fixture.0, "local-1").unwrap(),
            path.canonicalize().unwrap()
        );
        assert!(managed_home(&fixture.0, "../escape").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_home_symlink_cannot_select_another_identity() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let account = fixture.0.join("codex-accounts/local-1");
        let other = fixture.0.join("codex-accounts/local-2/home");
        fs::create_dir_all(&account).unwrap();
        fs::create_dir_all(&other).unwrap();
        symlink(other, account.join("home")).unwrap();
        assert!(managed_home(&fixture.0, "local-1").is_err());
    }

    #[test]
    fn parsing_errors_do_not_expose_provider_diagnostics() {
        let secret = b"{\"ok\":false,\"error\":\"TOKEN_SHOULD_STAY_PRIVATE\"}";
        assert!(
            !parse_accounts(secret, PathBuf::from("/profile"))
                .unwrap_err()
                .contains("TOKEN_")
        );
        assert!(
            !parse_accounts(b"TOKEN_SHOULD_STAY_PRIVATE", PathBuf::from("/profile"))
                .unwrap_err()
                .contains("TOKEN_")
        );
    }

    #[test]
    fn selected_missing_account_fails_instead_of_using_default() {
        let fixture = Fixture::new();
        assert!(resolve_launch_binding(&fixture.0, Some("local-1")).is_err());
        assert!(resolve_launch_binding(&fixture.0, Some("../escape")).is_err());
        assert_eq!(resolve_launch_binding(&fixture.0, None).unwrap().id, None);
        assert_eq!(
            resolve_launch_binding(&fixture.0, Some(SYSTEM_DEFAULT_ID))
                .unwrap()
                .id,
            None
        );
    }

    #[test]
    fn selected_binding_uses_its_home_without_following_orca_active_account() {
        let fixture = Fixture::new();
        let home = fixture.0.join("codex-accounts/local-1/home");
        fs::create_dir_all(&home).unwrap();
        let cache = parse_accounts(
            &response(serde_json::json!([
                {"id":"local-1","email":"first@example.test","managedHomeRuntime":"host"},
                {"id":"local-2","email":"second@example.test","managedHomeRuntime":"host"}
            ])),
            fixture.0.clone(),
        )
        .unwrap();
        let binding = resolve_cached_binding(&cache, "local-1").unwrap();
        assert_eq!(binding.id.as_deref(), Some("local-1"));
        assert_eq!(binding.label.as_deref(), Some("first@example.test"));
        assert_eq!(binding.home, home.canonicalize().unwrap());
        assert_eq!(cache.source_active_id.as_deref(), Some("local-2"));
        assert!(resolve_cached_binding(&cache, "local-2").is_err());
        fs::remove_dir_all(home).unwrap();
        assert!(resolve_cached_binding(&cache, "local-1").is_err());
    }

    #[test]
    fn account_command_is_explicitly_local_and_uses_public_list_only() {
        let command = local_account_command(Path::new("/installed/orca"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["account", "list", "--json"]
        );
        let removed = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect::<HashSet<_>>();
        assert!(removed.contains("ORCA_ENVIRONMENT"));
        assert!(removed.contains("ORCA_PAIRING_CODE"));
        assert!(removed.contains("ORCA_REMOTE_PAIRING"));
    }

    #[cfg(unix)]
    #[test]
    fn cli_failure_never_displays_raw_stderr() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let cli = fixture.0.join("orca");
        fs::write(
            &cli,
            b"#!/bin/sh\nprintf 'PRIVATE_PROVIDER_ERROR' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let error = run_orca_account_list(&cli).unwrap_err();
        assert!(!error.contains("PRIVATE_PROVIDER_ERROR"));
    }

    #[cfg(unix)]
    #[test]
    fn cli_stdout_is_bounded_even_when_child_exits_successfully() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let cli = fixture.0.join("orca");
        fs::write(&cli, b"#!/bin/sh\nhead -c 1048577 /dev/zero\n").unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(run_orca_account_list(&cli).is_err());
    }

    #[test]
    fn cache_is_private_metadata_and_works_without_orca() {
        let fixture = Fixture::new();
        let cache = parse_accounts(
            &response(serde_json::json!([
                {"id":"local-1","email":"first@example.test","managedHomeRuntime":"host"}
            ])),
            orca_user_data().unwrap(),
        )
        .unwrap();
        write_cache(&fixture.0, &cache).unwrap();
        let loaded = read_cache(&fixture.0).unwrap();
        assert_eq!(loaded.accounts[0].id, "local-1");
        let bytes = fs::read(fixture.0.join(CACHE_NAME)).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("NEVER_CACHE_ME"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(fixture.0.join(CACHE_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn cache_symlinks_are_rejected_before_reading_contents() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let other = fixture.0.join("unrelated-file");
        fs::write(&other, b"DO_NOT_READ").unwrap();
        symlink(other, fixture.0.join(CACHE_NAME)).unwrap();
        assert_eq!(
            read_cache(&fixture.0).unwrap_err(),
            "Invalid account metadata cache."
        );
    }

    #[test]
    fn labels_are_compact_and_do_not_contain_control_characters() {
        let metadata = AccountMetadata {
            id: "local-1".into(),
            email: Some(" name@example.test\n".into()),
            workspace_label: Some(" Personal (Pro)\t".into()),
            managed_home_runtime: Some("host".into()),
            wsl_distro: None,
        };
        assert_eq!(
            account_label(&metadata),
            "name@example.test · Personal (Pro)"
        );
        assert_eq!(
            account_email(&metadata).as_deref(),
            Some("name@example.test")
        );
        let unknown = AccountMetadata {
            email: Some("Saved account".into()),
            ..metadata
        };
        assert_eq!(account_email(&unknown), None);
    }
}
