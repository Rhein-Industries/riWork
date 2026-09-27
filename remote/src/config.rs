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
};
use uuid::Uuid;

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
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
impl Storage {
    pub fn from_env() -> Result<Self> {
        let home = env::var_os("RIWORK_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/share/riwork")
            });
        ensure!(!home.as_os_str().is_empty(), "set RIWORK_HOME");
        fs::create_dir_all(&home)?;
        Self::at(home)
    }
    pub fn at(home: PathBuf) -> Result<Self> {
        let dir = home.join("remote");
        private_dir(&dir)?;
        Ok(Self { dir })
    }
    pub fn lock(&self, name: &str) -> Result<File> {
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
        f.try_lock_exclusive()
            .context("another remote process is using this storage")?;
        Ok(f)
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
        let _lock = self.lock("config.lock")?;
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
        if let Some(path) = routes {
            let mut r: crate::relay::Routes = if path.exists() {
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
                desktop_token_sha256: hex::encode(Sha256::digest(decode::<32>(&desktop_token)?)),
                mobile_token_sha256: hex::encode(Sha256::digest(decode::<32>(&p.relay_token)?)),
            });
            private_write(path, &r)?;
        }
        // The export can be shown/scanned only by its owner; never overwrite an existing file.
        private_export(out, &p)?;
        c.devices.push(Device {
            pairing: p.clone(),
            desktop_token,
            allow_insecure_loopback: dev,
            revoked: false,
        });
        self.save(&c)?;
        Ok(p)
    }
    pub fn revoke(&self, id: &str) -> Result<()> {
        uuid(id)?;
        let _lock = self.lock("config.lock")?;
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
}
