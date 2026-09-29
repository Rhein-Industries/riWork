use crate::crypto::{Identity, b64, decode, random32, uuid};
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
use zeroize::Zeroize;

/// `pair`, `revoke` and the connector's audit record share config.lock. They
/// hold it for milliseconds, so waiting is bounded rather than failing spuriously.
const CONFIG_LOCK_WAIT: Duration = Duration::from_secs(10);

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pairing {
    pub v: u8,
    pub relay_url: String,
    pub desktop_id: String,
    pub device_id: String,
    pub route_id: String,
    pub device_name: String,
    /// v1 long-term PSK. Empty for v2, where the invite secret and root key are separate.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pairing_secret: String,
    pub relay_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_key: Option<String>,
    /// `pending`, `established` or `expired`. Absent on v1 records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_state: Option<String>,
}
impl Pairing {
    pub fn identity(&self) -> Identity {
        Identity {
            desktop_id: self.desktop_id.clone(),
            device_id: self.device_id.clone(),
            route_id: self.route_id.clone(),
        }
    }
    pub fn validate(&self, dev: bool) -> Result<()> {
        self.identity().bytes()?;
        decode::<32>(&self.relay_token)?;
        validate_url(&self.relay_url, dev)?;
        match self.v {
            1 => {
                ensure!(
                    self.invite_id.is_none()
                        && self.invite_secret.is_none()
                        && self.expires_at.is_none()
                        && self.root_key.is_none()
                        && self.invite_state.is_none(),
                    "v1 pairing cannot carry v2 invite fields"
                );
                decode::<32>(&self.pairing_secret)?;
            }
            2 => {
                ensure!(self.pairing_secret.is_empty(), "v2 pairing has no v1 PSK");
                let invite = self.invite_id.as_deref().context("missing invite id")?;
                uuid(invite)?;
                ensure!(
                    self.expires_at.is_some_and(|t| t > 0),
                    "v2 invite needs an expiry"
                );
                match self.invite_state.as_deref() {
                    Some("pending") => {
                        decode::<32>(
                            self.invite_secret
                                .as_deref()
                                .context("missing invite secret")?,
                        )?;
                        ensure!(self.root_key.is_none(), "pending invite has no root key");
                    }
                    Some("established") => {
                        decode::<32>(self.root_key.as_deref().context("missing root key")?)?;
                        ensure!(
                            self.invite_secret.is_none(),
                            "established device must not keep the invite secret"
                        );
                    }
                    Some("expired") => {
                        ensure!(
                            self.invite_secret.is_none() && self.root_key.is_none(),
                            "expired invite must not keep key material"
                        );
                    }
                    _ => bail!("invalid v2 invite state"),
                }
            }
            _ => bail!("unsupported pairing version"),
        }
        Ok(())
    }
    pub fn deep_link(&self) -> Result<String> {
        ensure!(self.v == 1 || self.v == 2, "unsupported pairing version");
        Ok(format!(
            "riwork://pair?v={}&data={}",
            self.v,
            b64(&serde_json::to_vec(self)?)
        ))
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub pairing: Pairing,
    pub desktop_token: String,
    pub allow_insecure_loopback: bool,
    pub revoked: bool,
    // Unix seconds. Absent in configs written before these were recorded, and
    // `first`/`last` stay absent until the device completes a handshake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_authenticated_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_authenticated_unix: Option<u64>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub v: u8,
    pub desktop_id: String,
    pub devices: Vec<Device>,
}
#[derive(Clone)]
pub struct Storage {
    pub dir: PathBuf,
}

pub fn validate_url(raw: &str, dev: bool) -> Result<()> {
    let u = url::Url::parse(raw)?;
    ensure!(
        u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
            && u.path() == "/v1/ws",
        "relay URL must have /v1/ws and no credentials/query/fragment"
    );
    let host = u.host_str().context("relay hostname missing")?;
    let loopback = host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1";
    ensure!(
        u.scheme() == "wss" || (dev && u.scheme() == "ws" && loopback),
        "use wss://; plaintext requires --allow-insecure-loopback and a loopback hostname"
    );
    Ok(())
}

fn private_dir(path: &Path) -> Result<()> {
    if path.exists() {
        let m = fs::symlink_metadata(path)?;
        ensure!(
            m.is_dir() && !m.file_type().is_symlink(),
            "private directory is not a real directory"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                m.permissions().mode() & 0o077 == 0,
                "{} must have mode 700",
                path.display()
            );
        }
    } else {
        let mut b = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(path)?;
    }
    Ok(())
}
fn options() -> OpenOptions {
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    o
}
pub fn private_read<T: DeserializeOwned>(path: &Path, max: u64) -> Result<T> {
    let mut f = options()
        .read(true)
        .open(path)
        .with_context(|| format!("read {}", path.display()))?;
    let m = f.metadata()?;
    ensure!(m.is_file() && m.len() <= max, "invalid private file/size");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            m.permissions().mode() & 0o077 == 0,
            "{} must have mode 600",
            path.display()
        );
    }
    let mut data = Vec::new();
    Read::by_ref(&mut f).take(max + 1).read_to_end(&mut data)?;
    ensure!(data.len() as u64 <= max, "private file limit");
    Ok(serde_json::from_slice(&data)?)
}
pub fn private_write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("file parent missing")?;
    // A bare relative name has the empty path as parent, which cannot be opened.
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let tmp = parent.join(format!(".remote-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut f = options().write(true).create_new(true).open(&tmp)?;
        serde_json::to_writer_pretty(&mut f, value)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
pub fn private_export<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut f = options()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {} (must not already exist)", path.display()))?;
    // create_new means this file is ours; do not leave a partial secret behind.
    let written = (|| {
        serde_json::to_writer_pretty(&mut f, value)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(path);
    }
    written
}
impl Storage {
    pub fn from_env() -> Result<Self> {
        let home = match env::var_os("RIWORK_HOME") {
            Some(home) => PathBuf::from(home),
            // Never fall back to the working directory: this tree holds the PSKs.
            None => match env::var_os("HOME").filter(|h| !h.is_empty()) {
                Some(home) => PathBuf::from(home).join(".local/share/riwork"),
                None => anyhow::bail!("HOME is not set; set RIWORK_HOME"),
            },
        };
        ensure!(!home.as_os_str().is_empty(), "set RIWORK_HOME");
        fs::create_dir_all(&home)?;
        Self::at(home)
    }
    pub fn at(home: PathBuf) -> Result<Self> {
        let dir = home.join("remote");
        private_dir(&dir)?;
        Ok(Self { dir })
    }
    fn open_lock(&self, name: &str) -> Result<File> {
        let f = options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join(name))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                f.metadata()?.permissions().mode() & 0o077 == 0,
                "lock must have mode 600"
            );
        }
        Ok(f)
    }
    pub fn lock(&self, name: &str) -> Result<File> {
        let f = self.open_lock(name)?;
        f.try_lock_exclusive()
            .context("another remote process is using this storage")?;
        Ok(f)
    }
    /// Like `lock`, but waits up to `wait` for a holder to finish.
    pub fn lock_wait(&self, name: &str, wait: Duration) -> Result<File> {
        let f = self.open_lock(name)?;
        let end = Instant::now() + wait;
        loop {
            match f.try_lock_exclusive() {
                Ok(()) => return Ok(f),
                Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                    ensure!(
                        Instant::now() < end,
                        "timed out waiting for another remote process using this storage"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e).context("lock remote storage"),
            }
        }
    }
    pub fn config(&self) -> Result<Config> {
        private_dir(&self.dir)?;
        let path = self.dir.join("devices.json");
        if !path.exists() {
            return Ok(Config {
                v: 1,
                desktop_id: Uuid::new_v4().to_string(),
                devices: vec![],
            });
        }
        let c: Config = private_read(&path, 2 * 1024 * 1024)?;
        ensure!(
            c.v == 1 && c.devices.len() <= 64,
            "unsupported/oversized desktop config"
        );
        uuid(&c.desktop_id)?;
        let mut ids = std::collections::HashSet::new();
        let mut routes = std::collections::HashSet::new();
        for d in &c.devices {
            ensure!(
                d.pairing.desktop_id == c.desktop_id
                    && ids.insert(d.pairing.device_id.clone())
                    && routes.insert(d.pairing.route_id.clone()),
                "invalid/duplicate device config"
            );
            uuid(&d.pairing.device_id)?;
            uuid(&d.pairing.route_id)?;
            if !d.revoked {
                d.pairing.validate(d.allow_insecure_loopback)?;
                decode::<32>(&d.desktop_token)?;
            }
        }
        Ok(c)
    }
    pub fn save(&self, c: &Config) -> Result<()> {
        private_write(&self.dir.join("devices.json"), c)
    }
    pub fn authorized(&self, id: &str) -> Result<bool> {
        Ok(self
            .config()?
            .devices
            .iter()
            .any(|d| d.pairing.device_id == id && !d.revoked))
    }
    pub fn pair(
        &self,
        relay: String,
        name: String,
        dev: bool,
        out: &Path,
        routes: Option<&Path>,
    ) -> Result<Pairing> {
        self.pair_with(relay, name, dev, out, routes, 1, 600)
    }
    /// `version` 1 is the frozen PSK pairing. `version` 2 is a single-use invite.
    /// `ttl_secs` applies only to v2 and must be 30..=3600.
    #[allow(clippy::too_many_arguments)]
    pub fn pair_with(
        &self,
        relay: String,
        name: String,
        dev: bool,
        out: &Path,
        routes: Option<&Path>,
        version: u8,
        ttl_secs: u64,
    ) -> Result<Pairing> {
        validate_url(&relay, dev)?;
        ensure!(version == 1 || version == 2, "unsupported pairing version");
        if version == 2 {
            ensure!(
                (30..=3600).contains(&ttl_secs),
                "v2 invite lifetime must be 30..=3600 seconds"
            );
        }
        ensure!(
            fs::symlink_metadata(out).is_err(),
            "pairing export must not already exist"
        );
        ensure!(
            !name.is_empty() && name.len() <= 128,
            "device name needs 1..128 bytes"
        );
        let _lock = self.lock_wait("config.lock", CONFIG_LOCK_WAIT)?;
        let mut c = self.config()?;
        // Revoked identities can never authorize again; keep outcome files for
        // review, but release their bounded live configuration slots.
        c.devices.retain(|d| !d.revoked);
        ensure!(c.devices.len() < 64, "active device limit (64)");
        let mobile_token = b64(&random32());
        let p = if version == 1 {
            Pairing {
                v: 1,
                relay_url: relay,
                desktop_id: c.desktop_id.clone(),
                device_id: Uuid::new_v4().to_string(),
                route_id: Uuid::new_v4().to_string(),
                device_name: name,
                pairing_secret: b64(&random32()),
                relay_token: mobile_token,
                invite_id: None,
                invite_secret: None,
                expires_at: None,
                root_key: None,
                invite_state: None,
            }
        } else {
            let expires_at = now_unix()
                .checked_add(ttl_secs)
                .context("invite expiry overflow")?;
            Pairing {
                v: 2,
                relay_url: relay,
                desktop_id: c.desktop_id.clone(),
                device_id: Uuid::new_v4().to_string(),
                route_id: Uuid::new_v4().to_string(),
                device_name: name,
                pairing_secret: String::new(),
                relay_token: mobile_token,
                invite_id: Some(Uuid::new_v4().to_string()),
                invite_secret: Some(b64(&random32())),
                expires_at: Some(expires_at),
                root_key: None,
                invite_state: Some("pending".into()),
            }
        };
        p.validate(dev)?;
        let desktop_token = b64(&random32());
        // Stage the route in memory so every validation happens before any file
        // changes. Commit order is then export, routes, config; a failure undoes
        // what came before it, so retrying never accumulates orphan routes.
        let staged = match routes {
            Some(path) => {
                let existed = path.exists();
                let mut r: crate::relay::Routes = if existed {
                    private_read(path, 1024 * 1024)?
                } else {
                    crate::relay::Routes {
                        v: 1,
                        routes: vec![],
                    }
                };
                r.validate()?;
                ensure!(r.routes.len() < 128, "relay route limit");
                r.routes.push(crate::relay::Route {
                    route_id: p.route_id.clone(),
                    desktop_token_sha256: hex::encode(Sha256::digest(decode::<32>(
                        &desktop_token,
                    )?)),
                    mobile_token_sha256: hex::encode(Sha256::digest(decode::<32>(&p.relay_token)?)),
                });
                r.validate()?;
                Some((path, r, existed))
            }
            None => None,
        };
        // The export can be shown/scanned only by its owner; never overwrite an existing file.
        // It fails most often (missing directory, existing file), so it goes first.
        private_export(out, &p)?;
        if let Some((path, r, _)) = &staged
            && let Err(e) = private_write(path, r)
        {
            let _ = fs::remove_file(out);
            return Err(e);
        }
        c.devices.push(Device {
            pairing: p.clone(),
            desktop_token,
            allow_insecure_loopback: dev,
            revoked: false,
            paired_at_unix: Some(now_unix()),
            first_authenticated_unix: None,
            last_authenticated_unix: None,
        });
        if let Err(e) = self.save(&c) {
            let _ = fs::remove_file(out);
            if let Some((path, _, existed)) = staged
                && let Err(undo) = remove_route(path, &p.route_id, existed)
            {
                return Err(e.context(format!(
                    "relay routes {} were not restored ({undo:#}); remove route {}",
                    path.display(),
                    p.route_id
                )));
            }
            return Err(e);
        }
        Ok(p)
    }
    pub fn revoke(&self, id: &str) -> Result<()> {
        uuid(id)?;
        // Emergency path: wait for a concurrent pair/audit write rather than fail.
        let _lock = self.lock_wait("config.lock", CONFIG_LOCK_WAIT)?;
        let mut c = self.config()?;
        let d = c
            .devices
            .iter_mut()
            .find(|d| d.pairing.device_id == id)
            .context("device not found")?;
        d.revoked = true;
        d.pairing.pairing_secret.clear();
        d.pairing.invite_secret = None;
        d.pairing.root_key = None;
        d.pairing.relay_token.clear();
        d.desktop_token.clear();
        self.save(&c)
    }
    pub fn fresh_device(&self, id: &str) -> Result<Option<Device>> {
        Ok(self
            .config()?
            .devices
            .into_iter()
            .find(|d| d.pairing.device_id == id && !d.revoked))
    }
    /// Blocks one invite so an overlapping `redeem_invite` returns `invite_race`
    /// without consuming it. Drop the guard to release the claim.
    pub fn testing_hold_invite(&self, invite_id: &str) -> InviteHold {
        claim_invite(invite_id).expect("test hold must be the first claim")
    }
    pub fn redeem_invite(
        &self,
        hello: &crate::crypto::PairHello,
        now_unix: u64,
        desktop_nonce: [u8; 32],
    ) -> std::result::Result<RedeemedInvite, RedeemError> {
        // Authenticate before taking the in-process claim so a bad proof cannot
        // block the legitimate phone, and a lost race does not burn the invite.
        self.inspect_invite(hello, now_unix, true)?;
        let _claim = claim_invite(&hello.invite_id).ok_or(RedeemError::Rejected("invite_race"))?;
        self.inspect_invite(hello, now_unix, true)?;
        self.commit_invite(hello, now_unix, desktop_nonce)
    }
    fn inspect_invite(
        &self,
        hello: &crate::crypto::PairHello,
        now_unix: u64,
        wipe_expired: bool,
    ) -> std::result::Result<(), RedeemError> {
        let _lock = self
            .lock_wait("config.lock", CONFIG_LOCK_WAIT)
            .map_err(RedeemError::Io)?;
        let mut config = self.config().map_err(RedeemError::Io)?;
        let device = invite_device(&mut config, hello)?;
        if let Some(code) = invite_terminal(device) {
            return Err(RedeemError::Rejected(code));
        }
        let expires_at = device
            .pairing
            .expires_at
            .ok_or(RedeemError::Rejected("invite_malformed"))?;
        if now_unix >= expires_at {
            if wipe_expired {
                device.pairing.invite_secret = None;
                device.pairing.invite_state = Some("expired".into());
                self.save(&config).map_err(RedeemError::Io)?;
            }
            return Err(RedeemError::Rejected("invite_expired"));
        }
        let encoded = device
            .pairing
            .invite_secret
            .clone()
            .ok_or(RedeemError::Rejected("invite_malformed"))?;
        let mut secret =
            decode::<32>(&encoded).map_err(|_| RedeemError::Rejected("invite_malformed"))?;
        let verified = crate::crypto::verify_pair_hello(
            &device.pairing.identity(),
            &device.pairing.relay_url,
            &hello.invite_id,
            expires_at,
            &secret,
            hello,
        );
        secret.zeroize();
        verified
            .map(|_| ())
            .map_err(|_| RedeemError::Rejected("invite_rejected"))
    }
    fn commit_invite(
        &self,
        hello: &crate::crypto::PairHello,
        now_unix: u64,
        desktop_nonce: [u8; 32],
    ) -> std::result::Result<RedeemedInvite, RedeemError> {
        let _lock = self
            .lock_wait("config.lock", CONFIG_LOCK_WAIT)
            .map_err(RedeemError::Io)?;
        let mut config = self.config().map_err(RedeemError::Io)?;
        let device = invite_device(&mut config, hello)?;
        if let Some(code) = invite_terminal(device) {
            return Err(RedeemError::Rejected(code));
        }
        let expires_at = device
            .pairing
            .expires_at
            .ok_or(RedeemError::Rejected("invite_malformed"))?;
        if now_unix >= expires_at {
            device.pairing.invite_secret = None;
            device.pairing.invite_state = Some("expired".into());
            self.save(&config).map_err(RedeemError::Io)?;
            return Err(RedeemError::Rejected("invite_expired"));
        }
        let encoded = device
            .pairing
            .invite_secret
            .clone()
            .ok_or(RedeemError::Rejected("invite_malformed"))?;
        let mut secret =
            decode::<32>(&encoded).map_err(|_| RedeemError::Rejected("invite_malformed"))?;
        let completed = crate::crypto::complete_pair(
            &device.pairing.identity(),
            &device.pairing.relay_url,
            &hello.invite_id,
            expires_at,
            &secret,
            hello,
            desktop_nonce,
        );
        secret.zeroize();
        let (accept, root, transcript) =
            completed.map_err(|_| RedeemError::Rejected("invite_rejected"))?;
        device.pairing.invite_secret = None;
        device.pairing.root_key = Some(b64(&root));
        device.pairing.invite_state = Some("established".into());
        self.save(&config).map_err(RedeemError::Io)?;
        Ok(RedeemedInvite {
            accept,
            root_key: root,
            transcript,
        })
    }
    /// Records a completed endpoint handshake and reports whether it was the
    /// device's first. Waits for config.lock so it cannot overwrite (or resurrect
    /// a device from) a concurrent pair/revoke.
    pub fn record_authentication(&self, id: &str, wait: Duration) -> Result<bool> {
        let _lock = self.lock_wait("config.lock", wait)?;
        let mut c = self.config()?;
        let d = c
            .devices
            .iter_mut()
            .find(|d| d.pairing.device_id == id && !d.revoked)
            .context("device not found")?;
        let now = now_unix();
        let first = d.first_authenticated_unix.is_none();
        d.first_authenticated_unix.get_or_insert(now);
        d.last_authenticated_unix = Some(now);
        self.save(&c)?;
        Ok(first)
    }
}

pub struct RedeemedInvite {
    pub accept: crate::crypto::PairAccept,
    pub root_key: [u8; 32],
    pub transcript: Vec<u8>,
}
impl std::fmt::Debug for RedeemedInvite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedeemedInvite")
            .field("accept", &self.accept.kind)
            .field("transcript_len", &self.transcript.len())
            .finish()
    }
}
#[derive(Debug)]
pub enum RedeemError {
    Rejected(&'static str),
    Io(anyhow::Error),
}
impl std::fmt::Display for RedeemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(code) => f.write_str(code),
            Self::Io(error) => write!(f, "{error:#}"),
        }
    }
}
impl std::error::Error for RedeemError {}
pub struct InviteHold {
    id: String,
}
impl Drop for InviteHold {
    fn drop(&mut self) {
        with_invite_gates(|gates| {
            gates.remove(&self.id);
        });
    }
}
fn invite_gates() -> &'static Mutex<HashSet<String>> {
    static GATES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    GATES.get_or_init(|| Mutex::new(HashSet::new()))
}
fn with_invite_gates<T>(body: impl FnOnce(&mut HashSet<String>) -> T) -> T {
    let mut gates = invite_gates()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    body(&mut gates)
}
fn claim_invite(invite_id: &str) -> Option<InviteHold> {
    with_invite_gates(|gates| {
        gates.insert(invite_id.to_string()).then(|| InviteHold {
            id: invite_id.to_string(),
        })
    })
}
fn invite_device<'a>(
    config: &'a mut Config,
    hello: &crate::crypto::PairHello,
) -> std::result::Result<&'a mut Device, RedeemError> {
    config
        .devices
        .iter_mut()
        .find(|device| {
            device.pairing.v == 2
                && device.pairing.desktop_id == hello.desktop_id
                && device.pairing.device_id == hello.device_id
                && device.pairing.route_id == hello.route_id
                && device.pairing.invite_id.as_deref() == Some(hello.invite_id.as_str())
        })
        .ok_or(RedeemError::Rejected("invite_rejected"))
}
fn invite_terminal(device: &Device) -> Option<&'static str> {
    if device.revoked {
        return Some("invite_rejected");
    }
    match device.pairing.invite_state.as_deref() {
        Some("established") => Some("invite_replay"),
        Some("expired") => Some("invite_expired"),
        Some("pending") => None,
        _ => Some("invite_malformed"),
    }
}

/// Undo of the route `pair` added; removes the manifest again if `pair` created it.
fn remove_route(path: &Path, route_id: &str, existed: bool) -> Result<()> {
    let mut r: crate::relay::Routes = private_read(path, 1024 * 1024)?;
    r.routes.retain(|x| x.route_id != route_id);
    if r.routes.is_empty() && !existed {
        fs::remove_file(path)?;
    } else {
        private_write(path, &r)?;
    }
    Ok(())
}
