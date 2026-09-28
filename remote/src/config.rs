use crate::crypto::{Identity, b64, decode, random32, uuid};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

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
    pub pairing_secret: String,
    pub relay_token: String,
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
        ensure!(self.v == 1, "unsupported pairing version");
        self.identity().bytes()?;
        decode::<32>(&self.pairing_secret)?;
        decode::<32>(&self.relay_token)?;
        validate_url(&self.relay_url, dev)?;
        Ok(())
    }
    pub fn deep_link(&self) -> Result<String> {
        Ok(format!(
            "riwork://pair?v=1&data={}",
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
        validate_url(&relay, dev)?;
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
        let p = Pairing {
            v: 1,
            relay_url: relay,
            desktop_id: c.desktop_id.clone(),
            device_id: Uuid::new_v4().to_string(),
            route_id: Uuid::new_v4().to_string(),
            device_name: name,
            pairing_secret: b64(&random32()),
            relay_token: b64(&random32()),
        };
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
        d.pairing.relay_token.clear();
        d.desktop_token.clear();
        self.save(&c)
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
