//! The client half of a Mac-to-Mac remote: the registry of paired hosts, redeeming
//! a protocol v2 invite, and the encrypted link to one host.
//!
//! A host is another Mac running `somtus-remote start`. This Mac pairs with it the
//! way a phone does (`pair --protocol 2 --kind desktop` there, a `somtus://pair?v=2`
//! link here) and then talks the same RPCs through the same blind relay, in the
//! role "mobile". [`Link`] is that connection: one per host, because the relay lets
//! one socket per role hold a route. It reconnects with backoff, so callers see a
//! [`LinkState`] and failed calls, never a half-open session.
//!
//! The registry is `$RIWORK_HOME/remote/hosts.json` (mode 600, written with the same
//! helpers as `devices.json`). It holds each host's root key, so it is never logged
//! and [`HostRecord`] has no `Debug` that prints it.
use crate::{
    HANDSHAKE_SECONDS, MAX_FRAME,
    config::{Pairing, Storage, private_read, private_write},
    connector::{connect_registered, receive_json, send_json},
    crypto::{
        Envelope, PairAccept, ServerHelloV2, accept_pair, accept_server_hello_v2, b64,
        client_hello_v2, decode, pair_hello, random32, uuid,
    },
    link, log_safe,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use std::{
    collections::HashMap,
    fs::File,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Semaphore, mpsc, oneshot, watch},
    time::{Instant, interval, sleep, sleep_until, timeout, timeout_at},
};
use tokio_tungstenite::tungstenite::Message;
use zeroize::Zeroize;

pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub const HOSTS_FILE: &str = "hosts.json";
const HOSTS_LOCK: &str = "hosts.lock";
const HOSTS_LOCK_WAIT: Duration = Duration::from_secs(10);
pub(crate) const MAX_HOSTS: usize = 64;
const MAX_HOSTS_BYTES: u64 = 1024 * 1024;
const MAX_LINK_BYTES: usize = 16 * 1024;
pub(crate) const MAX_LABEL_CHARS: usize = 64;
/// What a link says while the relay reports nobody on the host's side of the route.
const HOST_AWAY: &str = "host offline (or access revoked)";

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// One paired host. `pairing` is the established protocol v2 record: it holds the
/// root key and the relay token, so a `HostRecord` is a secret.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRecord {
    /// The host's desktop id, which is what `--desktop` takes.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing: Option<Pairing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<crate::ssh::Route>,
    pub label: String,
    pub relay: String,
    pub route: String,
    pub added_at_unix: u64,
    #[serde(default)]
    pub allow_insecure_loopback: bool,
}
impl std::fmt::Debug for HostRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostRecord")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("relay", &self.relay)
            .field("route", &self.route)
            .finish_non_exhaustive()
    }
}
/// What `hosts list` shows: everything but the secrets.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostSummary {
    pub id: String,
    pub label: String,
    pub relay: String,
    pub route: String,
    pub added_at_unix: u64,
    pub allow_insecure_loopback: bool,
    #[serde(default)]
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// How an SSH route connects, as `hosts add-ssh` takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshSummary>,
}
/// An SSH route without its `client_id`, which names this Mac to the host and which no
/// command takes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshSummary {
    pub target: String,
    pub remote_bin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub somtus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
}

pub(crate) fn check_label(label: &str) -> Result<()> {
    ensure!(
        !label.trim().is_empty()
            && label.chars().count() <= MAX_LABEL_CHARS
            && !label.chars().any(char::is_control)
            && label.trim() == label,
        "label needs 1 to {MAX_LABEL_CHARS} characters, no control characters and no space at either end"
    );
    Ok(())
}

impl HostRecord {
    pub fn new(
        pairing: Pairing,
        label: String,
        allow_insecure_loopback: bool,
    ) -> Result<HostRecord> {
        let record = HostRecord {
            id: pairing.desktop_id.clone(),
            label,
            relay: pairing.relay_url.clone(),
            route: pairing.route_id.clone(),
            pairing: Some(pairing),
            ssh: None,
            added_at_unix: now_unix(),
            allow_insecure_loopback,
        };
        record.validate()?;
        Ok(record)
    }
    /// A record is usable only when it is an established v2 pairing whose copies
    /// of the relay and route agree, so a hand-edited file fails here, not as a
    /// confusing handshake error later.
    pub fn validate(&self) -> Result<()> {
        uuid(&self.id)?;
        check_label(&self.label)?;
        if let Some(ssh) = &self.ssh {
            ensure!(
                self.pairing.is_none()
                    && self.relay.is_empty()
                    && self.route.is_empty()
                    && !self.allow_insecure_loopback,
                "SSH record cannot carry relay credentials"
            );
            return ssh.validate();
        }
        let pairing = self.pairing.as_ref().context("host has no route")?;
        ensure!(
            self.id == pairing.desktop_id
                && self.relay == pairing.relay_url
                && self.route == pairing.route_id,
            "host record disagrees with its pairing"
        );
        ensure!(
            pairing.v == 2 && pairing.invite_state.as_deref() == Some("established"),
            "host pairing must be an established protocol 2 pairing"
        );
        pairing.validate(self.allow_insecure_loopback)?;
        self.root()?;
        Ok(())
    }
    pub fn summary(&self) -> HostSummary {
        HostSummary {
            id: self.id.clone(),
            label: self.label.clone(),
            relay: self.relay.clone(),
            route: self.route.clone(),
            added_at_unix: self.added_at_unix,
            allow_insecure_loopback: self.allow_insecure_loopback,
            transport: if self.ssh.is_some() { "ssh" } else { "relay" }.into(),
            host_id: self.ssh.as_ref().map(|ssh| ssh.host_id.clone()),
            ssh: self.ssh.as_ref().map(crate::ssh::Route::summary),
        }
    }
    fn root(&self) -> Result<Secret> {
        Ok(Secret(decode::<32>(
            self.pairing
                .as_ref()
                .context("SSH has no relay root key")?
                .root_key
                .as_deref()
                .context("missing root key")?,
        )?))
    }
}

/// 32 secret bytes, wiped when dropped.
struct Secret([u8; 32]);
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostsFile {
    pub v: u8,
    pub hosts: Vec<HostRecord>,
}
impl HostsFile {
    fn path(storage: &Storage) -> std::path::PathBuf {
        storage.dir.join(HOSTS_FILE)
    }
    /// The registry, or an empty one when no host was added yet. All of it is valid or
    /// none of it is used. An error says what can take a bad record out, when something
    /// can, and repeats what the file says only through `log_safe`: it can hold anything.
    ///
    /// Only "no such file" means no hosts. Failing to look says nothing about them, and a
    /// daemon or a desktop that took it for an empty list would act on that. A symlink is
    /// there to be refused, wherever it points.
    pub fn load(storage: &Storage) -> Result<Self> {
        let path = Self::path(storage);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    v: 1,
                    hosts: vec![],
                });
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        }
        Self::read(&path).map_err(|error| {
            if RawHostsFile::read(&path).is_ok() {
                anyhow!(
                    "{}. `somtus-remote hosts remove ID` takes a record out of the file, and lists them if ID is not there",
                    log_safe(&format!("{error:#}"))
                )
            } else if error.is::<serde_json::Error>() {
                anyhow!("{}", log_safe(&format!("{error:#}")))
            } else {
                error
            }
        })
    }
    fn read(path: &std::path::Path) -> Result<Self> {
        let file: HostsFile = private_read(path, MAX_HOSTS_BYTES)?;
        ensure!(
            file.v == 1 && file.hosts.len() <= MAX_HOSTS,
            "unsupported or oversized {HOSTS_FILE}"
        );
        let mut ids = std::collections::HashSet::new();
        for host in &file.hosts {
            host.validate()
                .with_context(|| format!("invalid host record {}", log_safe(&host.id)))?;
            ensure!(ids.insert(&host.id), "duplicate host {}", host.id);
        }
        Ok(file)
    }
    pub fn save(&self, storage: &Storage) -> Result<()> {
        private_write(&Self::path(storage), self)
    }
    /// Serializes read-modify-write cycles of the `hosts` commands.
    pub fn lock(storage: &Storage) -> Result<File> {
        storage.lock_wait(HOSTS_LOCK, HOSTS_LOCK_WAIT)
    }
    pub fn find(&self, id: &str) -> Option<&HostRecord> {
        self.hosts.iter().find(|h| h.id == id)
    }
    /// The host `--desktop` names, with a hint when there is none.
    pub fn require(&self, id: &str) -> Result<&HostRecord> {
        self.find(id).with_context(|| {
            format!("no host {id}; `somtus-remote hosts list` shows the hosts added on this Mac")
        })
    }
}
/// The registry loaded for one host.
pub fn load_host(storage: &Storage, id: &str) -> Result<HostRecord> {
    uuid(id).context("--desktop takes a host's full desktop id")?;
    Ok(HostsFile::load(storage)?.require(id)?.clone())
}

/// The registry as `hosts remove` repairs it: its outline as strict as `HostsFile`'s, so
/// that a newer format is never rewritten, and each record left as the text it is, because
/// one this build cannot read may be fine to the build that wrote it.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHostsFile {
    v: u8,
    hosts: Vec<Box<RawValue>>,
}
impl RawHostsFile {
    fn read(path: &std::path::Path) -> Result<Self> {
        let file: Self = private_read(path, MAX_HOSTS_BYTES)?;
        ensure!(file.v == 1, "unsupported {HOSTS_FILE}");
        Ok(file)
    }
}

/// A host `hosts remove` took out, with what the person may still want of it. From a file
/// that failed validation `id` and `label` can be anything, so they are for `log_safe`.
#[derive(Debug)]
pub struct RemovedHost {
    pub id: String,
    pub label: String,
    pub ssh: Option<SshSummary>,
    /// What a relay host knows this Mac by, which leaves this Mac with the record.
    pub device_id: Option<String>,
}
impl RemovedHost {
    /// What can still be used of an entry of a file that failed validation.
    fn from_entry(id: &str, entry: &Value) -> Self {
        Self {
            id: id.to_owned(),
            label: entry["label"].as_str().unwrap_or_default().to_owned(),
            ssh: SshSummary::from_damaged(&entry["ssh"]),
            device_id: entry["pairing"]["device_id"]
                .as_str()
                .filter(|device| uuid(device).is_ok())
                .map(str::to_owned),
        }
    }
    /// The command that registers this route again.
    pub fn add_command(&self) -> Option<String> {
        let label = check_label(&self.label).is_ok().then_some(&*self.label);
        self.ssh.as_ref().map(|route| route.add_command(label))
    }
    /// Whether that command holds a home `hosts add-ssh` will not take as it is, as a route
    /// registered before it asked for an absolute one does.
    pub fn home_is_not_absolute(&self) -> bool {
        self.ssh
            .as_ref()
            .and_then(|route| route.home.as_deref())
            .is_some_and(|home| crate::ssh::check_new_home(home).is_err())
    }
}
/// What became of the registry in `hosts remove`.
#[derive(Debug, PartialEq, Eq)]
pub enum RegistryAfter {
    /// It was valid to begin with.
    Valid,
    /// It failed validation, and removing this was what it took.
    Repaired,
    /// It failed validation and still does, for this reason, which is safe to print.
    StillInvalid(String),
}
/// The result of `hosts remove`.
#[derive(Debug)]
pub struct Removal {
    /// One host, or all that held the id in a damaged file that repeats it.
    pub hosts: Vec<RemovedHost>,
    pub registry: RegistryAfter,
}

/// Removes a host; its credentials leave this Mac. A bad record must not stop the one
/// command that can take it out, so a registry that fails validation is repaired instead,
/// while everything else that reads it stays refused.
pub fn remove_host(storage: &Storage, id: &str) -> Result<Removal> {
    let _lock = HostsFile::lock(storage)?;
    let mut file = match HostsFile::load(storage) {
        Ok(file) => file,
        Err(error) => return remove_from_damaged(storage, id, error),
    };
    uuid(id).context("hosts remove takes a host's full desktop id")?;
    let at = file
        .hosts
        .iter()
        .position(|h| h.id == id)
        .with_context(|| format!("no host {id}"))?;
    let removed = file.hosts.remove(at);
    file.save(storage)?;
    Ok(Removal {
        hosts: vec![RemovedHost {
            id: removed.id,
            label: removed.label,
            ssh: removed.ssh.as_ref().map(crate::ssh::Route::summary),
            device_id: removed.pairing.map(|pairing| pairing.device_id),
        }],
        registry: RegistryAfter::Valid,
    })
}
/// An id from a damaged file, in full, for a person to read: as JSON, so that it cannot be
/// mistaken for anything around it, with every control character escaped. JSON leaves DEL
/// and the C1 range as they are, and a terminal can act on those too.
fn shown_id(id: &str) -> String {
    let mut shown = String::with_capacity(id.len() + 2);
    for c in serde_json::to_string(id).unwrap_or_default().chars() {
        if c.is_control() {
            shown.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else {
            shown.push(c);
        }
    }
    shown
}
/// Cuts every entry whose `id` is exactly `id` out of a registry that failed to load,
/// with `why` as the answer if the file is not one this may rewrite. The caller holds the
/// lock. Nothing else in the file is dropped or changed, whatever is wrong with it: under
/// version skew a record invalid here may be valid elsewhere, and a lost relay record
/// needs a new invite.
fn remove_from_damaged(storage: &Storage, id: &str, why: anyhow::Error) -> Result<Removal> {
    let path = HostsFile::path(storage);
    let Ok(file) = RawHostsFile::read(&path) else {
        return Err(why.context(format!("{HOSTS_FILE} was left as it is")));
    };
    let parsed = |entry: &RawValue| serde_json::from_str::<Value>(entry.get()).unwrap_or_default();
    let (taken, kept): (Vec<_>, Vec<_>) = file
        .hosts
        .into_iter()
        .partition(|entry| parsed(entry)["id"].as_str() == Some(id));
    if taken.is_empty() {
        // `hosts list` cannot say, with the file like this.
        let mut by_hand = false;
        let holds: Vec<String> = kept
            .iter()
            .take(MAX_HOSTS)
            .map(|entry| {
                let entry = parsed(entry);
                let shown = entry["id"].as_str().map(|id| (id, shown_id(id)));
                // What this finds a record by is the exact id, so one with no id, or one
                // that typing what is shown would not match, cannot be taken out from here.
                by_hand |= shown.as_ref().is_none_or(|(id, shown)| {
                    let quoted = shown.strip_prefix('"').and_then(|s| s.strip_suffix('"'));
                    quoted != Some(*id)
                });
                format!(
                    "{} (\"{}\")",
                    shown.map_or("(no id)".into(), |(_, shown)| shown),
                    log_safe(entry["label"].as_str().unwrap_or(""))
                )
            })
            .collect();
        let by_hand = if by_hand {
            format!(
                ". A record shown with no id, or with an escape in its id, has to be taken out of {} by hand",
                log_safe(&path.display().to_string())
            )
        } else {
            String::new()
        };
        bail!(
            "no host {}; the file holds: {}{by_hand}",
            log_safe(id),
            holds.join(", ")
        );
    }
    private_write(&path, &RawHostsFile { v: 1, hosts: kept })?;
    Ok(Removal {
        hosts: taken
            .iter()
            .map(|entry| RemovedHost::from_entry(id, &parsed(entry)))
            .collect(),
        registry: match HostsFile::load(storage) {
            Ok(_) => RegistryAfter::Repaired,
            Err(error) => RegistryAfter::StillInvalid(format!("{error:#}")),
        },
    })
}

/// What `hosts add` sees of a link.
pub fn parse_pair_link(text: &str, allow_insecure_loopback: bool) -> Result<Pairing> {
    let text = text.trim();
    ensure!(text.len() <= MAX_LINK_BYTES, "pairing link is too large");
    // The URL parser drops tabs and newlines wherever they are; a link is one token.
    ensure!(
        !text.chars().any(|c| c.is_whitespace() || c.is_control()),
        "expected a pairing link: somtus://pair?v=2&data=…"
    );
    let url = url::Url::parse(text)
        .ok()
        .filter(|u| matches!(u.scheme(), "somtus" | "riwork"))
        .context("expected a pairing link: somtus://pair?v=2&data=…")?;
    // Nothing a differential parser could read two ways: no userinfo, port,
    // fragment or path, no encoded host, and exactly the two query items.
    ensure!(
        url.host_str() == Some("pair")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.fragment().is_none()
            && matches!(url.path(), "" | "/"),
        "expected a pairing link: somtus://pair?v=2&data=…"
    );
    let raw = url.query().unwrap_or_default();
    ensure!(
        !raw.contains('%'),
        "expected a pairing link: somtus://pair?v=2&data=…"
    );
    let items: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let get = |name: &str| {
        let mut found = items.iter().filter(|(k, _)| k == name);
        match (found.next(), found.next()) {
            (Some((_, value)), None) => Some(value.as_str()),
            _ => None,
        }
    };
    ensure!(
        items.len() == 2,
        "expected a pairing link: somtus://pair?v=2&data=…"
    );
    let (Some(version), Some(data)) = (get("v"), get("data")) else {
        bail!("expected a pairing link: somtus://pair?v=2&data=…");
    };
    match version {
        "2" => {}
        "1" => bail!(
            "this is a protocol 1 link, which a phone uses; pair this Mac on the host with `pair --protocol 2 --kind desktop`"
        ),
        _ => bail!("unsupported pairing link version"),
    }
    let json = URL_SAFE_NO_PAD
        .decode(data)
        .ok()
        .filter(|bytes| b64(bytes) == data)
        .context("the pairing link holds malformed base64url")?;
    let pairing: Pairing = serde_json::from_slice(&json)
        .map_err(|_| anyhow::anyhow!("the pairing link does not hold a pairing"))?;
    ensure!(
        pairing.v == 2,
        "the link version does not match the pairing it holds"
    );
    pairing.validate(allow_insecure_loopback)?;
    ensure!(
        pairing.invite_state.as_deref() == Some("pending"),
        "this invite was already used or expired; make a new one with `pair --protocol 2 --kind desktop`"
    );
    ensure!(
        pairing.expires_at.is_some_and(|t| t > now_unix()),
        "this invite has expired; make a new one with `pair --protocol 2 --kind desktop`"
    );
    Ok(pairing)
}

/// What the host announced in `ready`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Features {
    pub deflate: bool,
    pub pty: Option<PtyFeatures>,
    pub history_max_lines: Option<u32>,
}
/// The limits of the host's `pty.*` streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtyFeatures {
    pub max_streams: u32,
    pub max_reads: u32,
    pub max_write: usize,
    pub max_chunk: usize,
}
impl Features {
    pub fn from_ready(ready: &Value) -> Self {
        let features = &ready["features"];
        let number = |value: &Value, default: u64, min: u64, max: u64| {
            value.as_u64().unwrap_or(default).clamp(min, max)
        };
        let pty = features["pty"].is_object().then(|| {
            let pty = &features["pty"];
            PtyFeatures {
                max_streams: number(&pty["max_streams"], 8, 1, 64) as u32,
                max_reads: number(&pty["max_reads"], 12, 1, 64) as u32,
                max_write: number(&pty["max_write"], 32_768, 1, 65_536) as usize,
                max_chunk: number(&pty["max_chunk"], 65_536, 1, 65_536) as usize,
            }
        });
        Self {
            deflate: features["deflate"].is_object(),
            pty,
            history_max_lines: features["history_max_lines"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok()),
        }
    }
}

/// Reads frames until the host's side of the route is there, or `wait` has
/// passed. A registration made while the host is away stays valid, so waiting
/// is cheaper than reconnecting.
async fn await_peer(ws: &mut Socket, wait: Duration) -> Result<bool> {
    let end = Instant::now() + wait;
    loop {
        let frame = match timeout_at(end, ws.next()).await {
            Err(_) => return Ok(false),
            Ok(frame) => frame,
        };
        match frame {
            Some(Ok(Message::Text(text))) => {
                ensure!(text.len() <= MAX_FRAME, "frame limit");
                let value: Value = serde_json::from_str(&text)?;
                ensure!(
                    value["v"] == 1 && value["type"] == "peer",
                    "unexpected frame while waiting for the host"
                );
                if value["online"].as_bool() == Some(true) {
                    return Ok(true);
                }
            }
            Some(Ok(Message::Ping(payload))) => {
                timeout(Duration::from_secs(10), ws.send(Message::Pong(payload))).await??;
            }
            Some(Ok(Message::Pong(_))) => {}
            _ => bail!("relay disconnected while waiting for the host"),
        }
    }
}
/// The next control frame within `HANDSHAKE_SECONDS` of `start`, refusing a `peer`
/// frame: the host going away mid-handshake ends the handshake.
async fn next_handshake_frame(ws: &mut Socket, end: Instant) -> Result<Value> {
    let value = timeout_at(end, receive_json(ws))
        .await
        .context("handshake timed out")??;
    if value["type"] == "peer" {
        bail!("the host went away during the handshake");
    }
    Ok(value)
}

/// A frame as the type it should be, for one nothing has authenticated yet: the relay can
/// send those as well as the host. serde's errors quote what they could not read, so that
/// sender would choose the words of a link's reason, and of what `hosts add` prints.
fn frame_as<T: serde::de::DeserializeOwned>(frame: Value) -> Result<T> {
    serde_json::from_value(frame)
        .map_err(|_| anyhow!("the host or the relay sent a frame of an unexpected shape"))
}

/// A session handshake on a registered socket whose root is established: the
/// X25519 exchange, then the authenticated `ready`.
async fn handshake_session(
    ws: &mut Socket,
    pairing: &Pairing,
    root: &[u8; 32],
) -> Result<(crate::crypto::Session, Features)> {
    let end = Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS);
    let identity = pairing.identity();
    let mut private = random32();
    let result = async {
        let (hello, _) = client_hello_v2(&identity, root, private)?;
        send_json(ws, &hello).await?;
        let server = next_handshake_frame(ws, end).await?;
        if server["type"] == "pair_error" {
            bail!(
                "the host refused the session ({})",
                known_pair_error(server["error"].as_str())
            );
        }
        let server: ServerHelloV2 = frame_as(server)?;
        let (finish, mut session) = accept_server_hello_v2(&identity, root, private, &server)?;
        send_json(ws, &finish).await?;
        let ready = next_handshake_frame(ws, end).await?;
        let envelope: Envelope = frame_as(ready)?;
        let plaintext = session.open("d2c", &envelope)?;
        let ready: Value = serde_json::from_slice(&link::decode_frame(&plaintext)?)?;
        ensure!(
            ready["v"] == 1
                && ready["type"] == "ready"
                && ready["desktop_id"] == pairing.desktop_id
                && ready["device_id"] == pairing.device_id,
            "the host did not authenticate its readiness"
        );
        Ok((session, Features::from_ready(&ready)))
    }
    .await;
    private.zeroize();
    result
}

/// The host's `pair_error` codes (see remote-protocol-v2.md), and nothing a peer could
/// put its own words in.
fn known_pair_error(code: Option<&str>) -> &'static str {
    const CODES: [&str; 5] = [
        "invite_expired",
        "invite_replay",
        "invite_race",
        "invite_rejected",
        "invite_malformed",
    ];
    CODES
        .iter()
        .copied()
        .find(|known| Some(*known) == code)
        .unwrap_or("unknown")
}

/// Redeems the invite on a registered socket: `pair_hello`, `pair_accept`, then
/// `pair_finish`. `persist` stores the established record and runs before the
/// finish is sent: the host consumed the invite when it accepted the hello, so
/// the root must be on disk before anything else can go wrong.
async fn redeem_on(
    ws: &mut Socket,
    pairing: &Pairing,
    persist: impl FnOnce(&Pairing) -> Result<()>,
) -> Result<Secret> {
    let end = Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS);
    let identity = pairing.identity();
    let invite_id = pairing.invite_id.as_deref().context("missing invite id")?;
    let expires_at = pairing.expires_at.context("missing invite expiry")?;
    let mut invite_secret = decode::<32>(
        pairing
            .invite_secret
            .as_deref()
            .context("missing invite secret")?,
    )?;
    let mut nonce = random32();
    let exchange = async {
        let hello = pair_hello(
            &identity,
            &pairing.relay_url,
            invite_id,
            expires_at,
            &invite_secret,
            nonce,
        )?;
        send_json(ws, &hello).await?;
        let reply = next_handshake_frame(ws, end).await?;
        if reply["type"] == "pair_error" {
            bail!(
                "the host rejected the invite ({})",
                known_pair_error(reply["error"].as_str())
            );
        }
        let accept: PairAccept = frame_as(reply)?;
        let (finish, root, mut transcript) = accept_pair(
            &identity,
            &pairing.relay_url,
            invite_id,
            expires_at,
            &invite_secret,
            &nonce,
            &accept,
        )?;
        transcript.zeroize();
        let root = Secret(root);
        let mut established = pairing.clone();
        established.invite_secret = None;
        established.root_key = Some(b64(&root.0));
        established.invite_state = Some("established".into());
        persist(&established)?;
        send_json(ws, &finish).await?;
        Ok(root)
    }
    .await;
    invite_secret.zeroize();
    nonce.zeroize();
    exchange
}

/// The result of `hosts add`.
#[derive(Debug)]
pub struct Added {
    pub host: HostSummary,
    /// What the host announced when this Mac first connected. `None` when that
    /// connection failed after the host was stored.
    pub features: Option<Features>,
    pub warning: Option<String>,
}

/// Redeems a pairing link and stores the established host. The record is on disk
/// before the first RPC, as the v2 contract requires; a failure before that leaves
/// nothing behind (and the invite, unless the host already consumed it).
pub async fn add_host(
    storage: &Storage,
    link_text: &str,
    label: Option<&str>,
    allow_insecure_loopback: bool,
) -> Result<Added> {
    let pairing = parse_pair_link(link_text, allow_insecure_loopback)?;
    let label = match label {
        Some(label) => label.to_owned(),
        None => format!("Mac {}", &pairing.desktop_id[..8]),
    };
    check_label(&label)?;
    let _lock = HostsFile::lock(storage)?;
    let mut file = HostsFile::load(storage)?;
    ensure!(file.hosts.len() < MAX_HOSTS, "host limit ({MAX_HOSTS})");
    if let Some(existing) = file.find(&pairing.desktop_id) {
        bail!(
            "host {} is already added as \"{}\"; run `somtus-remote hosts remove {}` first to pair it again",
            existing.id,
            log_safe(&existing.label),
            existing.id
        );
    }
    let (mut ws, online) = connect_registered(
        &pairing.relay_url,
        &pairing.route_id,
        "mobile",
        &pairing.relay_token,
    )
    .await?;
    if !online {
        ensure!(
            await_peer(&mut ws, Duration::from_secs(10)).await?,
            "the host is offline: `somtus remote start` has to be running there"
        );
    }
    let mut stored: Option<HostRecord> = None;
    let root = redeem_on(&mut ws, &pairing, |established| {
        let record = HostRecord::new(established.clone(), label, allow_insecure_loopback)?;
        file.hosts.push(record.clone());
        file.save(storage)?;
        stored = Some(record);
        Ok(())
    })
    .await?;
    let record = stored.context("pairing finished without a record")?;
    let (features, warning) = match handshake_session(
        &mut ws,
        record.pairing.as_ref().context("pairing")?,
        &root.0,
    )
    .await
    {
        Ok((_, features)) => (Some(features), None),
        Err(e) => (
            None,
            Some(format!(
                "the host was added, but its first connection failed: {}",
                log_safe(&format!("{e:#}"))
            )),
        ),
    };
    // The relay lets one socket per role hold the route; give it up before a client
    // process asks for it.
    // A relay that stopped reading must not hold this up.
    let _ = timeout(Duration::from_secs(2), ws.close(None)).await;
    Ok(Added {
        host: record.summary(),
        features,
        warning,
    })
}

// ---- The link -------------------------------------------------------------

/// Why a link is offline, when the failed attempt knows: fixed words only, never
/// what the far side printed. `needs_action` means retrying cannot help until a
/// person acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfflineCause {
    pub code: &'static str,
    pub needs_action: bool,
}
impl OfflineCause {
    /// The fields of a status line or of a call's error that say this.
    pub(crate) fn add_to(self, fields: &mut Value) {
        fields["cause"] = json!(self.code);
        if self.needs_action {
            fields["needs_action"] = json!(true);
        }
    }
}

/// Where a [`Link`] is. `since` is Unix seconds, the start of the outage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    Connecting,
    Online {
        rtt_ms: Option<u64>,
    },
    Offline {
        since: u64,
        reason: String,
        cause: Option<OfflineCause>,
    },
}
impl LinkState {
    /// The status line the daemon sends (`{"state":…,"label":…}`).
    pub fn to_status(&self, label: &str) -> Value {
        match self {
            LinkState::Connecting => json!({"state":"connecting","label":label}),
            LinkState::Online { rtt_ms: Some(ms) } => {
                json!({"state":"online","rtt_ms":ms,"label":label})
            }
            LinkState::Online { rtt_ms: None } => json!({"state":"online","label":label}),
            LinkState::Offline {
                since,
                reason,
                cause,
            } => {
                let mut status =
                    json!({"state":"offline","since":since,"reason":reason,"label":label});
                if let Some(cause) = cause {
                    cause.add_to(&mut status);
                }
                status
            }
        }
    }
    pub fn is_online(&self) -> bool {
        matches!(self, LinkState::Online { .. })
    }
}

/// Why a call did not produce a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// Not written: there is no session, or it ended with the request still queued.
    Offline(String),
    /// The request was written, or may have been, and the session ended before its
    /// answer. It may have run on the host, or be running still; only the host can say.
    Lost(String),
    /// No answer in time. The request may still have run on the host.
    Timeout,
    /// Refused here, before it was sent.
    Invalid(String),
    /// The host answered with an error.
    Rpc { code: String, message: String },
}
impl CallError {
    /// The `error.code` the daemon reports.
    pub fn code(&self) -> &str {
        match self {
            CallError::Offline(_) => "offline",
            CallError::Lost(_) => "connection_lost",
            CallError::Timeout => "timeout",
            CallError::Invalid(_) => "invalid_request",
            CallError::Rpc { code, .. } => code,
        }
    }
    pub fn message(&self) -> String {
        match self {
            CallError::Offline(reason) => format!("host is offline: {reason}"),
            CallError::Lost(reason) => {
                format!("the connection to the host ended before it answered: {reason}")
            }
            CallError::Timeout => "the host did not answer in time".into(),
            CallError::Invalid(message) => message.clone(),
            CallError::Rpc { message, .. } => message.clone(),
        }
    }
    /// A failure of the connection rather than of the request.
    pub fn is_link_loss(&self) -> bool {
        matches!(
            self,
            CallError::Offline(_) | CallError::Lost(_) | CallError::Timeout
        )
    }
}
impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}
impl std::error::Error for CallError {}

/// A decoded response, whatever it says.
#[derive(Clone, Debug)]
pub struct Reply {
    pub response: Value,
    /// From sending the request to receiving the reply.
    pub elapsed: Duration,
}
impl Reply {
    pub fn server_ms(&self) -> Option<u64> {
        self.response["server_ms"].as_u64()
    }
    pub fn into_result(self) -> Result<Value, CallError> {
        if self.response["ok"] == true {
            return Ok(self.response["result"].clone());
        }
        let error = &self.response["error"];
        Err(CallError::Rpc {
            code: error["code"].as_str().unwrap_or("invalid_response").into(),
            message: error["message"]
                .as_str()
                .unwrap_or("the host rejected the request")
                .into(),
        })
    }
}

pub(crate) enum Command {
    Request {
        id: String,
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<Reply, CallError>>,
    },
    /// End this session; the supervisor connects again.
    Reset(String),
}

/// A sent request whose answer is awaited with [`PendingCall::wait`]. Sending is
/// ordered (the order of `start` calls on one task is the order on the wire);
/// waiting is not, so many can be in flight.
pub struct PendingCall {
    rx: oneshot::Receiver<Result<Reply, CallError>>,
    pub generation: u64,
}
impl PendingCall {
    pub async fn wait(mut self, limit: Duration) -> Result<Reply, CallError> {
        timeout(limit, self.answer())
            .await
            .unwrap_or(Err(CallError::Timeout))
    }
    /// The answer, however long it takes. Dropping this future loses nothing: the answer
    /// can still be waited for, which is not so after `wait` runs out of time.
    pub(crate) async fn answer(&mut self) -> Result<Reply, CallError> {
        (&mut self.rx)
            .await
            // Nobody will answer now, and this cannot tell whether it was written.
            .unwrap_or_else(|_| Err(CallError::Lost("the session ended".into())))
    }
}

/// The live session of a [`Link`]. `generation` counts sessions, so a caller can
/// tell a reconnect from a session that simply lasted.
#[derive(Clone)]
pub struct Connection {
    pub generation: u64,
    pub features: Features,
    pub(crate) tx: mpsc::Sender<Command>,
    /// Held by the one `pty.open` this session may have out (see `Attach::open`).
    pub(crate) open_turn: Arc<Semaphore>,
}
impl Connection {
    pub(crate) fn new(generation: u64, features: Features, tx: mpsc::Sender<Command>) -> Self {
        Self {
            generation,
            features,
            tx,
            open_turn: Arc::new(Semaphore::new(1)),
        }
    }
    /// Sends a request on this session and returns at once. Unlike [`Link::start`] it
    /// never goes on a later session: it fails once this one has ended.
    pub(crate) async fn start(
        &self,
        method: &str,
        params: Value,
    ) -> Result<PendingCall, CallError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Request {
                id: uuid::Uuid::new_v4().to_string(),
                method: method.into(),
                params,
                reply,
            })
            .await
            .map_err(|_| CallError::Offline("the session ended".into()))?;
        Ok(PendingCall {
            rx,
            generation: self.generation,
        })
    }
}

/// Liveness and backoff; tests shorten them.
#[derive(Clone, Copy, Debug)]
pub struct LinkTiming {
    /// How often the relay is pinged and the host probed.
    pub ping: Duration,
    /// Nothing from the relay (its pings and pongs count) for this long: dead.
    pub silence: Duration,
    /// A probe to the host unanswered for this long: dead.
    pub probe: Duration,
    /// How long a registration waits for a host that is away.
    pub peer_wait: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
}
impl Default for LinkTiming {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(10),
            silence: Duration::from_secs(35),
            probe: Duration::from_secs(25),
            peer_wait: Duration::from_secs(30),
            backoff_min: Duration::from_millis(500),
            backoff_max: Duration::from_secs(15),
        }
    }
}
impl LinkTiming {
    /// The wait after the `failures`-th consecutive failure: doubling from the
    /// minimum to the maximum, spread by 20 percent so that clients which lost
    /// the same relay do not return together.
    pub fn backoff(&self, failures: u32) -> Duration {
        use rand::Rng;
        let doubled = self
            .backoff_min
            .saturating_mul(1u32 << failures.saturating_sub(1).min(16));
        let base = doubled.min(self.backoff_max);
        base.mul_f64(rand::thread_rng().gen_range(0.8..1.2))
    }
}

struct Inner {
    label: String,
    state: watch::Receiver<LinkState>,
    conn: watch::Receiver<Option<Connection>>,
    /// Dropping the last handle drops this sender, which stops the supervisor.
    _stop: watch::Sender<bool>,
}
/// The connection to one host, kept up for as long as a handle exists.
#[derive(Clone)]
pub struct Link {
    inner: Arc<Inner>,
}
impl Link {
    pub fn spawn(host: HostRecord) -> Link {
        Self::spawn_with(host, LinkTiming::default())
    }
    pub fn spawn_with(host: HostRecord, timing: LinkTiming) -> Link {
        let (state_tx, state) = watch::channel(LinkState::Connecting);
        let (conn_tx, conn) = watch::channel(None);
        let (stop_tx, stop) = watch::channel(false);
        let label = host.label.clone();
        tokio::spawn(supervise(Arc::new(host), timing, state_tx, conn_tx, stop));
        Link {
            inner: Arc::new(Inner {
                label,
                state,
                conn,
                _stop: stop_tx,
            }),
        }
    }
    /// A link that only holds `connection`, with nobody behind it: for tests of what goes
    /// on a session.
    #[cfg(test)]
    pub(crate) fn holding(connection: Option<Connection>) -> Link {
        let (_, state) = watch::channel(LinkState::Connecting);
        let (_, conn) = watch::channel(connection);
        let (stop, _) = watch::channel(false);
        Link {
            inner: Arc::new(Inner {
                label: String::new(),
                state,
                conn,
                _stop: stop,
            }),
        }
    }
    pub fn label(&self) -> &str {
        &self.inner.label
    }
    pub fn state(&self) -> LinkState {
        self.inner.state.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<LinkState> {
        self.inner.state.clone()
    }
    /// The live session, if there is one.
    pub fn connection(&self) -> Option<Connection> {
        self.inner.conn.borrow().clone()
    }
    pub fn subscribe_connection(&self) -> watch::Receiver<Option<Connection>> {
        self.inner.conn.clone()
    }
    /// The live session. While the first connection is still being made this waits
    /// up to `max` for it; once the link is known to be down it fails at once, so
    /// a caller polling a dead host is not held up for its whole timeout.
    pub async fn ready(&self, max: Duration) -> Result<Connection, CallError> {
        let end = Instant::now() + max;
        let mut state = self.inner.state.clone();
        loop {
            if let Some(connection) = self.connection() {
                return Ok(connection);
            }
            if let LinkState::Offline { reason, .. } = state.borrow_and_update().clone() {
                return Err(CallError::Offline(reason));
            }
            match timeout_at(end, state.changed()).await {
                Ok(Ok(())) => {}
                _ => return Err(CallError::Offline("still connecting".into())),
            }
        }
    }
    /// Waits, however long it takes, for a session newer than `after`.
    pub async fn next_session(&self, after: u64) -> Connection {
        let mut conn = self.inner.conn.clone();
        loop {
            if let Some(connection) = conn.borrow_and_update().clone()
                && connection.generation > after
            {
                return connection;
            }
            if conn.changed().await.is_err() {
                // The supervisor ended with the link; nothing will connect again.
                std::future::pending::<()>().await;
            }
        }
    }
    /// Sends a request on the live session and returns at once. Requests started
    /// in sequence by one task reach the host in that sequence.
    pub async fn start(&self, method: &str, params: Value) -> Result<PendingCall, CallError> {
        self.connection()
            .ok_or_else(|| CallError::Offline(self.offline_reason()))?
            .start(method, params)
            .await
    }
    /// A request and its reply, waiting up to `limit` for the connection too.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> Result<Reply, CallError> {
        let end = Instant::now() + limit;
        self.ready(limit).await?;
        let pending = tokio::time::timeout_at(end, self.start(method, params))
            .await
            .map_err(|_| CallError::Timeout)??;
        pending
            .wait(end.saturating_duration_since(Instant::now()))
            .await
    }
    /// Ends the live session; the link connects again. Streams on the host die
    /// with the session, which is what a caller that lost track of one wants.
    pub fn reconnect(&self, reason: &str) {
        if let Some(connection) = self.connection() {
            let _ = connection.tx.try_send(Command::Reset(reason.into()));
        }
    }
    fn offline_reason(&self) -> String {
        match self.state() {
            LinkState::Offline { reason, .. } => reason,
            _ => "not connected".into(),
        }
    }
}

struct Pending {
    reply: Option<oneshot::Sender<Result<Reply, CallError>>>,
    sent: Instant,
}
/// How one attempt to hold a session ended.
pub(crate) struct Outcome {
    pub(crate) reason: String,
    /// How long the session was ready; `None` if it never was.
    pub(crate) online_for: Option<Duration>,
    /// The relay was fine and the host was not there: no reason to back off.
    pub(crate) host_away: bool,
    /// What a failed SSH dial knows about why it failed.
    pub(crate) failure: Option<crate::ssh::DialFailure>,
}
impl Outcome {
    pub(crate) fn failed(reason: impl AsRef<str>) -> Self {
        Self {
            reason: log_safe(reason.as_ref()),
            online_for: None,
            host_away: false,
            failure: None,
        }
    }
}

/// Logs a link failure once a minute at most per reason.
#[derive(Default)]
struct FailureLog {
    last: Option<(String, Instant)>,
}
impl FailureLog {
    fn note(&mut self, label: &str, reason: &str) {
        if let Some((last, at)) = &self.last
            && last == reason
            && at.elapsed() < Duration::from_secs(60)
        {
            return;
        }
        eprintln!(
            "somtus-remote client: link to \"{}\" is down: {reason}; reconnecting (no payload logged).",
            log_safe(label)
        );
        self.last = Some((reason.to_owned(), Instant::now()));
    }
}

async fn supervise(
    host: Arc<HostRecord>,
    timing: LinkTiming,
    state: watch::Sender<LinkState>,
    conn: watch::Sender<Option<Connection>>,
    mut stop: watch::Receiver<bool>,
) {
    let mut generation = 0u64;
    let mut failures = 0u32;
    let mut outage: Option<u64> = None;
    let mut log = FailureLog::default();
    loop {
        let outcome = tokio::select! {
            _ = stop.changed() => return,
            outcome = attempt(&host, timing, &state, &conn, &mut generation) => outcome,
        };
        conn.send_replace(None);
        if outcome.online_for.is_some() {
            outage = Some(now_unix());
        }
        // The attempt may have said so already, when the relay said the host was away.
        let since = *outage.get_or_insert_with(|| match &*state.borrow() {
            LinkState::Offline { since, .. } => *since,
            _ => now_unix(),
        });
        state.send_replace(LinkState::Offline {
            since,
            reason: outcome.reason.clone(),
            cause: outcome.failure.map(crate::ssh::DialFailure::cause),
        });
        if outcome.host_away {
            failures = 0;
        } else {
            log.note(&host.label, &outcome.reason);
            if outcome
                .online_for
                .is_some_and(|d| d >= Duration::from_secs(5))
            {
                failures = 0;
            }
            failures += 1;
        }
        let pause = if outcome.host_away {
            Duration::from_millis(200)
        } else if outcome
            .failure
            .is_some_and(crate::ssh::DialFailure::needs_action)
        {
            // Trying again cannot help until a person acts, so a try only has to
            // notice that they did, and each one is a login on the far side.
            timing.backoff_max.saturating_mul(4)
        } else {
            timing.backoff(failures)
        };
        tokio::select! {
            _ = stop.changed() => return,
            _ = sleep(pause) => {}
        }
    }
}

/// Connects, authenticates and serves requests until the session ends.
async fn attempt(
    host: &HostRecord,
    timing: LinkTiming,
    state: &watch::Sender<LinkState>,
    conn: &watch::Sender<Option<Connection>>,
    generation: &mut u64,
) -> Outcome {
    if let Some(route) = &host.ssh {
        return crate::ssh::attempt(route, timing, state, conn, generation).await;
    }
    let Some(pairing) = host.pairing.as_ref() else {
        return Outcome::failed("host has no route");
    };
    let root = match host.root() {
        Ok(root) => root,
        Err(e) => return Outcome::failed(format!("host record: {e:#}")),
    };
    let (mut ws, online) = match connect_registered(
        &pairing.relay_url,
        &pairing.route_id,
        "mobile",
        &pairing.relay_token,
    )
    .await
    {
        Ok(registered) => registered,
        Err(e) => return Outcome::failed(format!("relay: {e:#}")),
    };
    if !online {
        // Nobody needs to wait out `peer_wait` to learn what the relay has just said, and
        // the registration stays valid, so this socket goes on listening for the host.
        // Only the first time: a later attempt finds the outage already told.
        state.send_if_modified(|current| {
            let first = matches!(current, LinkState::Connecting);
            if first {
                *current = LinkState::Offline {
                    since: now_unix(),
                    reason: HOST_AWAY.into(),
                    cause: None,
                };
            }
            first
        });
        match await_peer(&mut ws, timing.peer_wait).await {
            Ok(true) => {}
            Ok(false) => {
                return Outcome {
                    reason: HOST_AWAY.into(),
                    online_for: None,
                    host_away: true,
                    failure: None,
                };
            }
            Err(e) => return Outcome::failed(format!("{e:#}")),
        }
    }
    let (mut session, features) = match handshake_session(&mut ws, pairing, &root.0).await {
        Ok(established) => established,
        Err(e) => return Outcome::failed(format!("handshake: {e:#}")),
    };
    drop(root);
    *generation += 1;
    let (tx, mut rx) = mpsc::channel(256);
    let started = Instant::now();
    let mut pending: HashMap<String, Pending> = HashMap::new();
    conn.send_replace(Some(Connection::new(*generation, features.clone(), tx)));
    state.send_replace(LinkState::Online { rtt_ms: None });
    let reason = run_connection(
        &mut ws,
        &mut session,
        &mut rx,
        &mut pending,
        &features,
        timing,
        state,
    )
    .await;
    // Callers must see "offline" before they see their requests fail.
    conn.send_replace(None);
    rx.close();
    let lost = CallError::Lost(reason.clone());
    for (_, request) in pending.drain() {
        if let Some(reply) = request.reply {
            let _ = reply.send(Err(lost.clone()));
        }
    }
    // What never left the queue was never written.
    let unsent = CallError::Offline(reason.clone());
    while let Ok(command) = rx.try_recv() {
        if let Command::Request { reply, .. } = command {
            let _ = reply.send(Err(unsent.clone()));
        }
    }
    // The session ended, often because this socket stopped taking data; closing it
    // must not wait for it to start again.
    let _ = timeout(Duration::from_secs(2), ws.close(None)).await;
    Outcome {
        reason: log_safe(&reason),
        online_for: Some(started.elapsed()),
        host_away: false,
        failure: None,
    }
}

/// Seals and sends one request. `Err` is a reason to end the session; a request
/// that cannot be sealed (too large) is answered to its caller and the session
/// goes on.
async fn send_request(
    ws: &mut Socket,
    session: &mut crate::crypto::Session,
    pending: &mut HashMap<String, Pending>,
    request: (String, &str, Value),
    reply: Option<oneshot::Sender<Result<Reply, CallError>>>,
) -> Result<(), String> {
    let (id, method, params) = request;
    let body = json!({"v":1,"type":"request","id":id,"method":method,"params":params});
    let sealed = serde_json::to_vec(&body)
        .map_err(|e| e.to_string())
        .and_then(|bytes| session.seal("c2d", &bytes).map_err(|e| e.to_string()));
    let envelope = match sealed {
        Ok(envelope) => envelope,
        Err(e) => {
            if let Some(reply) = reply {
                let _ = reply.send(Err(CallError::Invalid(format!("request not sent: {e}"))));
            }
            return Ok(());
        }
    };
    pending.insert(
        id,
        Pending {
            reply,
            sent: Instant::now(),
        },
    );
    send_json(ws, &envelope)
        .await
        .map_err(|e| format!("send failed: {e:#}"))
}

/// A decrypted, decoded response.
fn open_response(session: &mut crate::crypto::Session, frame: Value) -> Result<Value> {
    let envelope: Envelope = frame_as(frame)?;
    let plaintext = session.open("d2c", &envelope)?;
    let response: Value = serde_json::from_slice(&link::decode_frame(&plaintext)?)?;
    ensure!(
        response["v"] == 1 && response["type"] == "response",
        "unexpected message from the host"
    );
    Ok(response)
}

/// The serving loop of one session. Returns why it ended.
///
/// Liveness has two parts. The relay path: a transport ping every `timing.ping`,
/// and no frame of any kind for `timing.silence` means dead. The host: a probe
/// (`link.configure` with nothing to change, which the host answers at once from
/// its connection loop) every `timing.ping`, unanswered for `timing.probe` means
/// dead. The probe's round trip, less the host's own `server_ms`, is the
/// `rtt_ms` of [`LinkState::Online`].
async fn run_connection(
    ws: &mut Socket,
    session: &mut crate::crypto::Session,
    rx: &mut mpsc::Receiver<Command>,
    pending: &mut HashMap<String, Pending>,
    features: &Features,
    timing: LinkTiming,
    state: &watch::Sender<LinkState>,
) -> String {
    let mut ping = interval(timing.ping);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;
    let mut last_rx = Instant::now();
    let new_id = || uuid::Uuid::new_v4().to_string();
    if features.deflate {
        // Replies are understood in both forms; this only makes big ones smaller.
        let configure = (new_id(), "link.configure", json!({"compression":"deflate"}));
        if let Err(e) = send_request(ws, session, pending, configure, None).await {
            return e;
        }
    }
    let first = (new_id(), "link.configure", json!({}));
    let mut probe = Some(first.0.clone());
    if let Err(e) = send_request(ws, session, pending, first, None).await {
        return e;
    }
    loop {
        tokio::select! {
            command = rx.recv() => match command {
                Some(Command::Request { id, method, params, reply }) => {
                    if reply.is_closed() { continue; }
                    let request = (id, method.as_str(), params);
                    if let Err(e) = send_request(ws, session, pending, request, Some(reply)).await {
                        return e;
                    }
                }
                Some(Command::Reset(reason)) => return reason,
                None => return "link closed".into(),
            },
            frame = ws.next() => {
                if matches!(frame, Some(Ok(_))) {
                    last_rx = Instant::now();
                }
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        if text.len() > MAX_FRAME {
                            return "frame limit".into();
                        }
                        let value: Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(e) => return format!("bad frame: {e}"),
                        };
                        match value["type"].as_str() {
                            Some("encrypted") => {
                                let received = Instant::now();
                                let response = match open_response(session, value) {
                                    Ok(r) => r,
                                    Err(e) => return format!("bad frame: {e:#}"),
                                };
                                // No id: an error for a request the host could not read.
                                // No entry: the caller stopped waiting.
                                let Some(request) = response["id"]
                                    .as_str()
                                    .and_then(|id| pending.remove(id).map(|p| (id.to_owned(), p)))
                                else {
                                    continue;
                                };
                                let (id, request) = request;
                                let elapsed = received.saturating_duration_since(request.sent);
                                if probe.as_deref() == Some(id.as_str()) {
                                    probe = None;
                                    let server = response["server_ms"].as_u64().unwrap_or(0);
                                    let rtt = u64::try_from(elapsed.as_millis())
                                        .unwrap_or(u64::MAX)
                                        .saturating_sub(server);
                                    state.send_if_modified(|current| match current {
                                        LinkState::Online { rtt_ms } if *rtt_ms != Some(rtt) => {
                                            *rtt_ms = Some(rtt);
                                            true
                                        }
                                        _ => false,
                                    });
                                }
                                if let Some(reply) = request.reply {
                                    let _ = reply.send(Ok(Reply { response, elapsed }));
                                }
                            }
                            // The host left or came back: the session is over either way.
                            Some("peer") => return "the host's connection changed".into(),
                            _ => return "unexpected frame from the host".into(),
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        match timeout(Duration::from_secs(10), ws.send(Message::Pong(payload))).await {
                            Ok(Ok(())) => {}
                            _ => return "relay stopped accepting data".into(),
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => {
                        return "the relay closed the connection".into();
                    }
                    Some(Ok(_)) => return "unsupported relay frame".into(),
                    Some(Err(e)) => return format!("relay: {e}"),
                }
            },
            _ = ping.tick() => {
                // A caller that gave up leaves its entry behind; drop those here.
                pending.retain(|_, p| p.reply.as_ref().is_none_or(|r| !r.is_closed()));
                if let Some(id) = &probe
                    && pending.get(id).is_some_and(|p| p.sent.elapsed() > timing.probe)
                {
                    return "the host stopped answering".into();
                }
                match timeout(Duration::from_secs(10), ws.send(Message::Ping(Vec::new().into()))).await {
                    Ok(Ok(())) => {}
                    _ => return "relay stopped accepting data".into(),
                }
                if probe.is_none() {
                    let next = (new_id(), "link.configure", json!({}));
                    probe = Some(next.0.clone());
                    if let Err(e) = send_request(ws, session, pending, next, None).await {
                        return e;
                    }
                }
            },
            _ = sleep_until(last_rx + timing.silence) => {
                if last_rx.elapsed() >= timing.silence {
                    return "relay silent past the liveness deadline".into();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const RELAY: &str = "wss://relay.example.com/v1/ws";

    /// A pending v2 pairing as the host's `pair --protocol 2` makes it.
    fn invite(dir: &std::path::Path, relay: &str, dev: bool) -> Pairing {
        // Each call is another host, with a desktop id of its own.
        let home = dir.join(format!("host-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        Storage::at(home)
            .unwrap()
            .pair_with(
                relay.into(),
                "client".into(),
                dev,
                &dir.join(format!("export-{}.json", uuid::Uuid::new_v4())),
                None,
                2,
                600,
            )
            .unwrap()
    }
    fn established(pairing: &Pairing) -> Pairing {
        let mut p = pairing.clone();
        p.invite_secret = None;
        p.root_key = Some(b64(&random32()));
        p.invite_state = Some("established".into());
        p
    }
    fn link_for(pairing: &Pairing) -> String {
        format!(
            "somtus://pair?v={}&data={}",
            pairing.v,
            b64(&serde_json::to_vec(pairing).unwrap())
        )
    }
    fn load_error(storage: &Storage) -> String {
        format!("{:#}", HostsFile::load(storage).err().expect("refused"))
    }
    fn refused(link: &str) -> String {
        format!("{:#}", parse_pair_link(link, false).err().expect("refused"))
    }

    #[test]
    fn a_v2_link_is_read_into_its_pending_pairing() {
        let tmp = tempfile::tempdir().unwrap();
        let pairing = invite(tmp.path(), RELAY, false);
        let link = pairing.deep_link().unwrap();
        assert!(link.starts_with("somtus://pair?"));
        let legacy = link.replacen("somtus:", "riwork:", 1);
        assert!(parse_pair_link(&legacy, false).is_ok());
        let read = parse_pair_link(&link, false).unwrap();
        assert_eq!(read.desktop_id, pairing.desktop_id);
        assert_eq!(read.invite_secret, pairing.invite_secret);
        assert_eq!(read.invite_state.as_deref(), Some("pending"));
        // Whitespace around a pasted link is not part of it.
        let padded = format!("  \n{}\n ", pairing.deep_link().unwrap());
        assert!(parse_pair_link(&padded, false).is_ok());
        // The scheme is not case sensitive; the rest is exact.
        let upper = pairing
            .deep_link()
            .unwrap()
            .replacen("somtus:", "SOMTUS:", 1);
        assert!(parse_pair_link(&upper, false).is_ok());
    }

    #[test]
    fn links_that_are_not_a_v2_pairing_link_are_refused_with_a_reason_and_no_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let pairing = invite(tmp.path(), RELAY, false);
        let good = pairing.deep_link().unwrap();
        let data = good.split("data=").nth(1).unwrap();
        let secrets = [
            pairing.invite_secret.clone().unwrap(),
            pairing.relay_token.clone(),
        ];
        let mut cases: Vec<(String, &str)> = vec![
            ("".into(), "expected a pairing link"),
            ("hello".into(), "expected a pairing link"),
            (
                format!("https://pair?v=2&data={data}"),
                "expected a pairing link",
            ),
            (
                format!("somtus://other?v=2&data={data}"),
                "expected a pairing link",
            ),
            (
                format!("somtus://user@pair?v=2&data={data}"),
                "expected a pairing link",
            ),
            (
                format!("somtus://pair:80?v=2&data={data}"),
                "expected a pairing link",
            ),
            (
                format!("somtus://pair/extra?v=2&data={data}"),
                "expected a pairing link",
            ),
            (
                format!("somtus://pair?v=2&data={data}#fragment"),
                "expected a pairing link",
            ),
            (
                format!("somtus://pair?v=2&data={data}&extra=1"),
                "expected a pairing link",
            ),
            (
                format!("somtus://pair?v=2&v=2&data={data}"),
                "expected a pairing link",
            ),
            (
                format!("somtus://pair?data={data}"),
                "expected a pairing link",
            ),
            (
                format!("somtus://pair?v=2&data=%41{data}"),
                "expected a pairing link",
            ),
            ("somtus://pair?v=2".into(), "expected a pairing link"),
            (format!("somtus://pair?v=1&data={data}"), "protocol 1"),
            (
                format!("somtus://pair?v=3&data={data}"),
                "unsupported pairing link version",
            ),
            (
                format!("somtus://pair?v=2&data={data}="),
                "malformed base64url",
            ),
            (
                "somtus://pair?v=2&data=!!!!".to_string(),
                "malformed base64url",
            ),
            (
                format!("somtus://pair?v=2&data={}", b64(b"not json")),
                "does not hold a pairing",
            ),
            (
                format!("somtus://pair?v=2&data={}", "A".repeat(20_000)),
                "too large",
            ),
        ];
        // A link whose version and pairing disagree, and a pairing with a field of its own.
        let mut v1ish = serde_json::to_value(&pairing).unwrap();
        v1ish["v"] = json!(1);
        cases.push((
            format!(
                "somtus://pair?v=2&data={}",
                b64(&serde_json::to_vec(&v1ish).unwrap())
            ),
            "does not match",
        ));
        let mut extra = serde_json::to_value(&pairing).unwrap();
        extra["surprise"] = json!(true);
        cases.push((
            format!(
                "somtus://pair?v=2&data={}",
                b64(&serde_json::to_vec(&extra).unwrap())
            ),
            "does not hold a pairing",
        ));
        for (link, reason) in cases {
            let error = refused(&link);
            assert!(error.contains(reason), "{link:.60}: {error}");
            for secret in &secrets {
                assert!(!error.contains(secret.as_str()), "{error}");
            }
        }
    }

    #[test]
    fn used_expired_or_unsafe_invites_are_refused_before_the_network_is_used() {
        let tmp = tempfile::tempdir().unwrap();
        let pending = invite(tmp.path(), RELAY, false);
        // Already redeemed (it holds a root key, not an invite secret).
        assert!(refused(&link_for(&established(&pending))).contains("already used"));
        // Expired.
        let mut old = pending.clone();
        old.expires_at = Some(now_unix() - 1);
        assert!(refused(&link_for(&old)).contains("expired"));
        let mut expired = pending.clone();
        expired.invite_secret = None;
        expired.invite_state = Some("expired".into());
        assert!(parse_pair_link(&link_for(&expired), false).is_err());
        // A plaintext relay needs the development switch and a loopback host.
        let local = invite(tmp.path(), "ws://127.0.0.1:9/v1/ws", true);
        assert!(refused(&local.deep_link().unwrap()).contains("--allow-insecure-loopback"));
        assert!(parse_pair_link(&local.deep_link().unwrap(), true).is_ok());
        let mut remote = local.clone();
        remote.relay_url = "ws://relay.example.com/v1/ws".into();
        assert!(parse_pair_link(&link_for(&remote), true).is_err());
        // A relay URL with credentials or another path is not one.
        let mut sneaky = pending;
        sneaky.relay_url = "wss://user:pass@relay.example.com/v1/ws".into();
        assert!(parse_pair_link(&link_for(&sneaky), false).is_err());
    }

    #[test]
    fn a_host_record_holds_only_established_pairings_that_agree_with_themselves() {
        let tmp = tempfile::tempdir().unwrap();
        let pending = invite(tmp.path(), RELAY, false);
        let record = HostRecord::new(established(&pending), "Studio".into(), false).unwrap();
        assert_eq!(record.id, pending.desktop_id);
        assert_eq!(
            (record.relay.as_str(), record.route.as_str()),
            (RELAY, pending.route_id.as_str())
        );
        assert!(
            HostRecord::new(pending.clone(), "Studio".into(), false).is_err(),
            "pending"
        );
        let mut v1 = established(&pending);
        v1.v = 1;
        assert!(HostRecord::new(v1, "Studio".into(), false).is_err());
        let mut moved = record.clone();
        moved.relay = "wss://elsewhere.example.com/v1/ws".into();
        assert!(
            moved
                .validate()
                .unwrap_err()
                .to_string()
                .contains("disagrees")
        );
        let mut other = record.clone();
        other.id = uuid::Uuid::new_v4().to_string();
        assert!(other.validate().is_err());
        let mut no_root = record.clone();
        no_root.pairing.as_mut().unwrap().root_key = None;
        assert!(no_root.validate().is_err());

        // What is printed for debugging, and listed, carries no secret.
        let debug = format!("{record:?}");
        let root = record.pairing.as_ref().unwrap().root_key.clone().unwrap();
        assert!(
            !debug.contains(&root)
                && !debug.contains(&record.pairing.as_ref().unwrap().relay_token)
        );
        let summary = serde_json::to_string(&record.summary()).unwrap();
        assert!(
            !summary.contains(&root)
                && !summary.contains(&record.pairing.as_ref().unwrap().relay_token)
        );
        assert!(!summary.contains("pairing"));
        // The record survives its own JSON.
        let again: HostRecord =
            serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
        assert_eq!(again.id, record.id);
        assert_eq!(
            again.pairing.as_ref().unwrap().root_key,
            record.pairing.as_ref().unwrap().root_key
        );
        // Existing relay files predate optional SSH routes: their JSON shape and
        // pairing material remain readable without a migration or new keys.
        let legacy = serde_json::to_value(&record).unwrap();
        assert!(legacy.get("ssh").is_none());
        assert!(legacy["pairing"].is_object());
        let restored: HostRecord = serde_json::from_value(legacy.clone()).unwrap();
        restored.validate().unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), legacy);
        assert_eq!(record.summary().transport, "relay");
        assert!(record.summary().host_id.is_none());
        assert!(record.summary().ssh.is_none());
    }

    #[test]
    fn labels_are_short_printable_and_trimmed() {
        for good in ["Studio", "Mac mini (office)", "Büro-Mac", "a"] {
            check_label(good).unwrap();
        }
        for bad in [
            "",
            " ",
            " Studio",
            "Studio ",
            "two\nlines",
            "esc\x1b[0m",
            &"x".repeat(65),
        ] {
            assert!(check_label(bad).is_err(), "{bad:?}");
        }
        check_label(&"x".repeat(64)).unwrap();
    }

    #[test]
    fn the_registry_is_private_survives_a_round_trip_and_refuses_what_is_not_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        assert!(
            HostsFile::load(&storage).unwrap().hosts.is_empty(),
            "no file, no hosts"
        );
        let a = HostRecord::new(
            established(&invite(tmp.path(), RELAY, false)),
            "A".into(),
            false,
        )
        .unwrap();
        let b = HostRecord::new(
            established(&invite(tmp.path(), RELAY, false)),
            "B".into(),
            false,
        )
        .unwrap();
        let mut file = HostsFile::load(&storage).unwrap();
        file.hosts = vec![a.clone(), b.clone()];
        file.v = 1;
        file.save(&storage).unwrap();
        let path = storage.dir.join(HOSTS_FILE);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&storage.dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let loaded = HostsFile::load(&storage).unwrap();
        assert_eq!(
            loaded
                .hosts
                .iter()
                .map(|h| h.label.as_str())
                .collect::<Vec<_>>(),
            ["A", "B"]
        );
        assert_eq!(
            loaded
                .require(&a.id)
                .unwrap()
                .pairing
                .as_ref()
                .unwrap()
                .root_key,
            a.pairing.as_ref().unwrap().root_key
        );
        assert!(
            loaded
                .require(&uuid::Uuid::new_v4().to_string())
                .unwrap_err()
                .to_string()
                .contains("hosts list")
        );
        assert_eq!(load_host(&storage, &b.id).unwrap().label, "B");
        assert!(load_host(&storage, "not-a-uuid").is_err());
        // No stray temp files stay behind a write.
        let strays: Vec<_> = std::fs::read_dir(&storage.dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty());

        // Removing returns the host and keeps the other.
        let removal = remove_host(&storage, &a.id).unwrap();
        assert_eq!(removal.hosts[0].label, "A");
        assert_eq!(removal.registry, RegistryAfter::Valid);
        assert_eq!(HostsFile::load(&storage).unwrap().hosts.len(), 1);
        assert!(remove_host(&storage, &a.id).is_err());

        // A file others can read, or that names a host twice, or has fields it should not, is refused.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(load_error(&storage).contains("mode 600"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut twice = HostsFile::load(&storage).unwrap();
        twice.hosts.push(b.clone());
        twice.save(&storage).unwrap();
        assert!(load_error(&storage).contains("duplicate"));
        let mut value = serde_json::to_value(HostsFile {
            v: 1,
            hosts: vec![b.clone()],
        })
        .unwrap();
        value["hosts"][0]["extra"] = json!(1);
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(HostsFile::load(&storage).is_err());
        let mut newer = serde_json::to_value(HostsFile {
            v: 1,
            hosts: vec![b],
        })
        .unwrap();
        newer["v"] = json!(2);
        std::fs::write(&path, newer.to_string()).unwrap();
        assert!(HostsFile::load(&storage).is_err());
    }

    /// A registry written by hand, as `hosts_json` holds it.
    fn write_registry(storage: &Storage, hosts_json: &str) -> std::path::PathBuf {
        let path = storage.dir.join(HOSTS_FILE);
        std::fs::write(&path, hosts_json).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
    fn a_record(dir: &std::path::Path, label: &str) -> HostRecord {
        HostRecord::new(established(&invite(dir, RELAY, false)), label.into(), false).unwrap()
    }

    #[test]
    fn hosts_remove_takes_a_record_out_of_a_damaged_registry_and_touches_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        let good = a_record(tmp.path(), "Good");
        let mut bad = a_record(tmp.path(), "Bad");
        bad.label = " too long and spaced ".repeat(9);
        // As another build might write it: other keys, in another order, in another layout.
        let newer = r#"{ "zzz":1.10, "id":"11111111-2222-4333-8444-555555555555",
            "aaa":{"big":123456789012345678901234567890,"z":[1, 2,3]}, "label":"From a newer build"}"#;
        let good_text = serde_json::to_string_pretty(&good).unwrap();
        let bad_text = serde_json::to_string(&bad).unwrap();
        let path = write_registry(
            &storage,
            &format!(r#"{{"v":1,"hosts":[{good_text},{newer},{bad_text}]}}"#),
        );
        assert!(HostsFile::load(&storage).is_err());

        let removal = remove_host(&storage, &bad.id).unwrap();
        assert_eq!(removal.hosts.len(), 1);
        assert_eq!(removal.hosts[0].id, bad.id);
        assert_eq!(removal.hosts[0].label, bad.label);
        assert_eq!(
            removal.hosts[0].device_id.as_ref(),
            bad.pairing.as_ref().map(|pairing| &pairing.device_id)
        );
        assert!(removal.hosts[0].add_command().is_none(), "not an SSH route");
        // The record from the newer build is still in the way, and the result says so.
        let RegistryAfter::StillInvalid(reason) = &removal.registry else {
            panic!("{:?}", removal.registry);
        };
        assert!(reason.contains("hosts remove"), "{reason}");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains(&bad.id), "{after}");
        // Nothing about the other two moved, down to the bytes.
        assert!(after.contains(&good_text), "{after}");
        assert!(after.contains(newer), "{after}");
        // It stays private and leaves nothing behind.
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // The record from the newer build still stops a load: dropping it is the person's
        // choice, because to that build it may be fine.
        assert!(HostsFile::load(&storage).is_err());

        let removal = remove_host(&storage, "11111111-2222-4333-8444-555555555555").unwrap();
        assert_eq!(removal.hosts[0].label, "From a newer build");
        assert_eq!(removal.registry, RegistryAfter::Repaired);
        let file = HostsFile::load(&storage).unwrap();
        assert_eq!(file.hosts.len(), 1);
        assert_eq!(file.hosts[0].id, good.id);
        assert_eq!(
            file.hosts[0].pairing.as_ref().unwrap().root_key,
            good.pairing.as_ref().unwrap().root_key
        );
        let strays: Vec<_> = std::fs::read_dir(&storage.dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty());
    }

    #[test]
    fn hosts_remove_on_a_damaged_registry_goes_by_the_exact_id_and_takes_all_that_hold_it() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        let path = write_registry(
            &storage,
            r#"{"v":1,"hosts":[
                {"id":"not a uuid","label":"named oddly"},
                {"id":"NOT A UUID","label":"a near miss"},
                {"id":"twice","label":"first"},
                {"id":"twice","label":"second"},
                {"label":"no id"},
                {"id":5,"label":"a number"},
                7
            ]}"#,
        );
        let labels_left = || -> Vec<Value> {
            let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            file["hosts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|host| host["label"].clone())
                .collect()
        };
        assert_eq!(
            remove_host(&storage, "not a uuid").unwrap().hosts[0].label,
            "named oddly"
        );
        assert_eq!(
            labels_left(),
            [
                json!("a near miss"),
                json!("first"),
                json!("second"),
                json!("no id"),
                json!("a number"),
                Value::Null
            ]
        );
        let labels: Vec<_> = remove_host(&storage, "twice")
            .unwrap()
            .hosts
            .into_iter()
            .map(|host| host.label)
            .collect();
        assert_eq!(labels, ["first", "second"]);
        assert_eq!(
            labels_left(),
            [
                json!("a near miss"),
                json!("no id"),
                json!("a number"),
                Value::Null
            ]
        );
        // What is not there stays an error, with nothing written. A number is not its text.
        let before = std::fs::read(&path).unwrap();
        for absent in ["nobody", "5", "not a uuid", ""] {
            let error = format!("{:#}", remove_host(&storage, absent).expect_err(absent));
            assert!(error.contains("no host"), "{error}");
            assert!(
                error.contains("a near miss"),
                "shows what it could mean: {error}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }

    /// The ids and any key this build does not know play no part in how a route connects,
    /// and the record was the only copy of how it does.
    #[test]
    fn hosts_remove_of_a_damaged_route_still_gives_the_command_that_adds_it_back() {
        const ID: &str = "11111111-2222-4333-8444-555555555555";
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        let removed = |ssh: &Value| {
            let record =
                json!({"id":ID,"label":"Studio","ssh":ssh,"relay":"","route":"","added_at_unix":1});
            write_registry(&storage, &json!({"v":1,"hosts":[record]}).to_string());
            assert!(HostsFile::load(&storage).is_err(), "{ssh}");
            remove_host(&storage, ID).unwrap().hosts.remove(0)
        };
        let good = json!({
            "target": "studio",
            "remote_bin": "/opt/somtus-remote",
            "somtus": "/opt/somtus",
            "home": "/data/somtus",
            "host_id": "22222222-2222-4222-8222-222222222222",
            "client_id": "33333333-3333-4333-8333-333333333333",
        });
        let with = |change: &dyn Fn(&mut serde_json::Map<String, Value>)| {
            let mut ssh = good.clone();
            change(ssh.as_object_mut().unwrap());
            ssh
        };
        let set = |name: &'static str, value: Value| {
            with(&move |ssh| {
                ssh.insert(name.into(), value.clone());
            })
        };
        let missing = |name: &'static str| {
            with(&move |ssh| {
                ssh.remove(name);
            })
        };
        let command = "somtus-remote hosts add-ssh --target='studio' \
            --remote-bin='/opt/somtus-remote' --somtus='/opt/somtus' \
            --home='/data/somtus' --label='Studio'";
        for damage in [
            set("host_id", json!("corrupted")),
            set("client_id", json!("corrupted")),
            set("from_a_newer_build", json!(1)),
            missing("host_id"),
            missing("client_id"),
        ] {
            assert_eq!(
                removed(&damage).add_command().as_deref(),
                Some(command),
                "{damage}"
            );
        }
        // Options the route does not have stay off, whichever way it does not have them.
        let plain = with(&|ssh| {
            ssh.insert("host_id".into(), json!("corrupted"));
            ssh.insert("somtus".into(), Value::Null);
            ssh.remove("home");
        });
        assert_eq!(
            removed(&plain).add_command().as_deref(),
            Some(
                "somtus-remote hosts add-ssh --target='studio' \
                 --remote-bin='/opt/somtus-remote' --label='Studio'"
            )
        );
        // What add-ssh would refuse, or a command for some other route, is not printed.
        for damage in [
            set("target", json!("has space")),
            set("target", json!("-oProxyCommand=x")),
            set("target", json!("studio\u{1b}[2J")),
            set("target", json!(5)),
            missing("target"),
            set("remote_bin", json!("/opt/\u{9b}31m")),
            set("remote_bin", json!("")),
            missing("remote_bin"),
            set("somtus", json!(5)),
            set("home", json!("/data\nsomtus")),
            set("home", json!(["/data/somtus"])),
            json!("not a route"),
        ] {
            assert_eq!(removed(&damage).add_command(), None, "{damage}");
        }
    }

    /// What this command cannot be given, the person has to be told to do themselves.
    #[test]
    fn hosts_remove_says_which_records_it_cannot_name_and_lists_ids_as_they_are() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        let long = "x".repeat(250);
        // Odd, and any of them can be typed back: listed whole, and no more to say.
        write_registry(
            &storage,
            &format!(
                r#"{{"v":1,"hosts":[{{"id":"plain","label":"a"}},{{"id":"{long}","label":"b"}}]}}"#
            ),
        );
        let error = format!("{:#}", remove_host(&storage, "nobody").err().unwrap());
        assert!(error.contains("\"plain\" (\"a\")"), "{error}");
        assert!(error.contains(&format!("\"{long}\"")), "{error}");
        assert!(!error.contains("by hand"), "{error}");
        for (entry, shown) in [
            (r#"{"label":"x"}"#, "(no id)"),
            (r#"{"id":5,"label":"x"}"#, "(no id)"),
            ("7", "(no id)"),
            // A real id, in a record that cannot be read to find it.
            (r#"{"id":"real","x":1e999}"#, "(no id)"),
            // ESC reaches this as itself, and is listed as something else.
            (
                r#"{"id":"\u001b[31mbad","label":"x"}"#,
                r#""\u001b[31mbad""#,
            ),
            // JSON has no escape it must use for these, and a terminal may act on them.
            (
                "{\"id\":\"\u{9b}31mbad\u{7f}\",\"label\":\"x\"}",
                r#""\u009b31mbad\u007f""#,
            ),
            (r#"{"id":"say \"hi\"","label":"x"}"#, r#""say \"hi\"""#),
        ] {
            let path = write_registry(
                &storage,
                &format!(r#"{{"v":1,"hosts":[{{"id":"plain","label":"a"}},{entry}]}}"#),
            );
            let before = std::fs::read(&path).unwrap();
            let error = format!("{:#}", remove_host(&storage, "nobody").err().unwrap());
            assert!(error.contains(shown), "{entry}: {error}");
            assert!(error.contains("by hand"), "{entry}: {error}");
            assert!(
                error.contains(path.to_str().unwrap()),
                "says which file: {error}"
            );
            assert!(!error.chars().any(char::is_control), "{entry}: {error:?}");
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }

    /// `hosts list` printing an empty list, and a daemon going away with its terminals, are
    /// for a file that is not there. Not for one nobody could look at just now.
    #[test]
    fn a_registry_that_cannot_be_looked_at_is_an_error_and_not_an_empty_one() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        let path = storage.dir.join(HOSTS_FILE);
        // A symlink to a volume that is away. A save would replace it, but nothing gets to
        // that when nothing loads.
        std::os::unix::fs::symlink(tmp.path().join("away.json"), &path).unwrap();
        assert!(HostsFile::load(&storage).is_err());
        let anyone = uuid::Uuid::new_v4().to_string();
        assert!(remove_host(&storage, &anyone).is_err());
        assert!(std::fs::symlink_metadata(&path).unwrap().is_symlink());
        std::fs::remove_file(&path).unwrap();
        // A directory that lost its search bit: a stat there is refused, not told "no such file".
        if unsafe { libc::geteuid() } != 0 {
            std::fs::set_permissions(&storage.dir, std::fs::Permissions::from_mode(0o000)).unwrap();
            let loaded = HostsFile::load(&storage);
            std::fs::set_permissions(&storage.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let error = format!("{:#}", loaded.err().expect("refused"));
            assert!(error.contains(HOSTS_FILE), "{error}");
        }
        // No such file is still no hosts.
        assert!(HostsFile::load(&storage).unwrap().hosts.is_empty());
    }

    #[test]
    fn hosts_remove_leaves_alone_a_registry_it_does_not_fully_understand() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        // Each has a record it could remove, and each is a reason not to rewrite the file:
        // a newer format would lose what this build does not know of.
        for content in [
            r#"{"v":2,"hosts":[{"id":"x"}]}"#,
            r#"{"v":1.0,"hosts":[{"id":"x"}]}"#,
            r#"{"v":"1","hosts":[{"id":"x"}]}"#,
            r#"{"hosts":[{"id":"x"}]}"#,
            r#"{"v":1}"#,
            r#"{"v":1,"hosts":[{"id":"x"}],"newer":true}"#,
            r#"{"v":1,"v":1,"hosts":[{"id":"x"}]}"#,
            r#"{"v":1,"hosts":{"x":{"id":"x"}}}"#,
            r#"[{"id":"x"}]"#,
            r#"{"v":1,"hosts":[{"id":"x"}"#,
            "not JSON",
        ] {
            let path = write_registry(&storage, content);
            assert!(HostsFile::load(&storage).is_err(), "{content}");
            assert!(remove_host(&storage, "x").is_err(), "{content}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        }
        let damaged = r#"{"v":1,"hosts":[{"id":"x"}]}"#;
        // Mode, kind and size stay what they are for every other read, and a rewrite
        // would launder them.
        let path = write_registry(&storage, damaged);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(format!("{:#}", remove_host(&storage, "x").err().unwrap()).contains("mode 600"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), damaged);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        std::fs::remove_file(&path).unwrap();
        let elsewhere = tmp.path().join("elsewhere.json");
        std::fs::write(&elsewhere, damaged).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(remove_host(&storage, "x").is_err());
        assert!(std::fs::symlink_metadata(&path).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), damaged);
        std::fs::remove_file(&path).unwrap();
        let huge = format!(
            r#"{{"v":1,"hosts":[{{"id":"x"}},{{"padding":"{}"}}]}}"#,
            "x".repeat(MAX_HOSTS_BYTES as usize)
        );
        write_registry(&storage, &huge);
        assert!(remove_host(&storage, "x").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), huge);
    }

    #[test]
    fn hosts_remove_on_a_registry_with_no_fault_stays_strict() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        // No file at all: nothing to remove, and nothing is created.
        assert!(remove_host(&storage, &uuid::Uuid::new_v4().to_string()).is_err());
        assert!(!storage.dir.join(HOSTS_FILE).exists());
        let a = a_record(tmp.path(), "A");
        HostsFile {
            v: 1,
            hosts: vec![a.clone()],
        }
        .save(&storage)
        .unwrap();
        let before = std::fs::read(storage.dir.join(HOSTS_FILE)).unwrap();
        let error = format!("{:#}", remove_host(&storage, "not-a-uuid").err().unwrap());
        assert!(error.contains("full desktop id"), "{error}");
        let error = format!(
            "{:#}",
            remove_host(&storage, &uuid::Uuid::new_v4().to_string())
                .err()
                .unwrap()
        );
        assert!(error.contains("no host"), "{error}");
        assert_eq!(std::fs::read(storage.dir.join(HOSTS_FILE)).unwrap(), before);
    }

    #[test]
    fn load_errors_say_what_can_take_a_bad_record_out_and_only_then() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        let good = a_record(tmp.path(), "Good");
        let mut bad = good.clone();
        bad.label = "x".repeat(65);
        let good_text = serde_json::to_string(&good).unwrap();
        let bad_text = serde_json::to_string(&bad).unwrap();
        let other = HostRecord {
            id: uuid::Uuid::new_v4().to_string(),
            ..a_record(tmp.path(), "Other")
        };
        let other_text = serde_json::to_string(&other).unwrap();
        let many = vec![good_text.clone(); MAX_HOSTS + 1].join(",");
        let repairable = [
            format!(r#"{{"v":1,"hosts":[{bad_text}]}}"#),
            format!(r#"{{"v":1,"hosts":[{good_text},{good_text}]}}"#),
            format!(r#"{{"v":1,"hosts":[{good_text},{other_text}]}}"#),
            r#"{"v":1,"hosts":[{"id":"x"}]}"#.to_owned(),
            r#"{"v":1,"hosts":[{"id":"x","label":"y","unknown":1}]}"#.to_owned(),
            format!(r#"{{"v":1,"hosts":[{many}]}}"#),
        ];
        for content in repairable {
            write_registry(&storage, &content);
            let error = load_error(&storage);
            assert!(error.contains("hosts remove"), "{error}");
        }
        for content in [r#"{"v":2,"hosts":[]}"#, "not JSON", r#"{"v":1"#] {
            write_registry(&storage, content);
            let error = load_error(&storage);
            assert!(!error.contains("hosts remove"), "{error}");
        }
        let path = write_registry(&storage, r#"{"v":1,"hosts":[]}"#);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(!load_error(&storage).contains("hosts remove"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        // What the file says is not repeated raw: it may hold anything.
        for content in [
            "{\"v\":1,\"hosts\":[{\"id\":\"\\u001b[31mx\",\"label\":\"y\",\"relay\":\"\",\"route\":\"\",\"added_at_unix\":1}]}",
            "{\"v\":1,\"hosts\":[{\"\\u001b[31mx\":1}]}",
        ] {
            write_registry(&storage, content);
            let error = load_error(&storage);
            assert!(!error.contains('\u{1b}'), "{error:?}");
            assert!(error.contains("hosts remove"), "{error}");
        }
    }

    #[test]
    fn the_registry_will_not_follow_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        let elsewhere = tmp.path().join("elsewhere.json");
        let file = HostsFile {
            v: 1,
            hosts: vec![],
        };
        file.save(&storage).unwrap();
        std::fs::rename(storage.dir.join(HOSTS_FILE), &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, storage.dir.join(HOSTS_FILE)).unwrap();
        assert!(HostsFile::load(&storage).is_err());
    }

    #[test]
    fn features_are_read_with_the_limits_the_host_gave_and_sane_ones_when_it_did_not() {
        let none = Features::from_ready(&json!({"type":"ready"}));
        assert_eq!(none, Features::default());
        let phone = Features::from_ready(
            &json!({"features":{"deflate":{"min_bytes":2048},"history_max_lines":5000}}),
        );
        assert!(phone.deflate && phone.pty.is_none());
        assert_eq!(phone.history_max_lines, Some(5000));
        let mac = Features::from_ready(
            &json!({"features":{"pty":{"max_streams":8,"max_reads":12,"max_write":32768,"max_chunk":65536}}}),
        );
        assert_eq!(
            mac.pty,
            Some(PtyFeatures {
                max_streams: 8,
                max_reads: 12,
                max_write: 32_768,
                max_chunk: 65_536
            })
        );
        // Missing fields default; absurd ones are clamped to what a frame can carry.
        let sparse = Features::from_ready(&json!({"features":{"pty":{}}}));
        assert_eq!(sparse.pty.unwrap().max_reads, 12);
        let wild = Features::from_ready(
            &json!({"features":{"pty":{"max_write":9_999_999,"max_reads":0,"max_streams":1000}}}),
        )
        .pty
        .unwrap();
        assert_eq!(
            (wild.max_write, wild.max_reads, wild.max_streams),
            (65_536, 1, 64)
        );
        assert!(
            Features::from_ready(&json!({"features":{"pty":"yes"}}))
                .pty
                .is_none()
        );
    }

    #[test]
    fn reconnecting_backs_off_to_a_ceiling_and_never_in_step() {
        let timing = LinkTiming::default();
        for failures in 0..40 {
            let wait = timing.backoff(failures);
            let base = timing
                .backoff_min
                .saturating_mul(1u32 << failures.saturating_sub(1).min(16))
                .min(timing.backoff_max);
            assert!(
                wait >= base.mul_f64(0.79) && wait <= base.mul_f64(1.21),
                "{failures}: {wait:?} vs {base:?}"
            );
        }
        assert!(timing.backoff(1) < Duration::from_millis(700));
        assert!(
            timing.backoff(30) > Duration::from_secs(11),
            "the ceiling is reached"
        );
        assert!(timing.backoff(30) < Duration::from_secs(19));
        let varied: std::collections::HashSet<_> =
            (0..20).map(|_| timing.backoff(5).as_millis()).collect();
        assert!(varied.len() > 1, "jitter");
    }

    #[test]
    fn the_state_a_link_reports_has_the_fields_the_clients_read() {
        assert_eq!(
            LinkState::Connecting.to_status("A"),
            json!({"state":"connecting","label":"A"})
        );
        assert_eq!(
            LinkState::Online { rtt_ms: Some(12) }.to_status("A"),
            json!({"state":"online","rtt_ms":12,"label":"A"})
        );
        assert_eq!(
            LinkState::Online { rtt_ms: None }.to_status("A"),
            json!({"state":"online","label":"A"})
        );
        assert_eq!(
            LinkState::Offline {
                since: 5,
                reason: "gone".into(),
                cause: None
            }
            .to_status("A"),
            json!({"state":"offline","since":5,"reason":"gone","label":"A"})
        );
        // A known cause is added; `needs_action` appears only when it is true.
        assert_eq!(
            LinkState::Offline {
                since: 5,
                reason: "gone".into(),
                cause: Some(OfflineCause {
                    code: "identity_changed",
                    needs_action: true
                })
            }
            .to_status("A"),
            json!({"state":"offline","since":5,"reason":"gone","label":"A","cause":"identity_changed","needs_action":true})
        );
        assert_eq!(
            LinkState::Offline {
                since: 5,
                reason: "gone".into(),
                cause: Some(OfflineCause {
                    code: "unreachable",
                    needs_action: false
                })
            }
            .to_status("A"),
            json!({"state":"offline","since":5,"reason":"gone","label":"A","cause":"unreachable"})
        );
        assert!(
            LinkState::Online { rtt_ms: None }.is_online() && !LinkState::Connecting.is_online()
        );
    }

    #[test]
    fn replies_and_failures_map_to_the_results_and_codes_the_daemon_reports() {
        let reply = |response: Value| Reply {
            response,
            elapsed: Duration::from_millis(3),
        };
        let ok = reply(json!({"ok":true,"result":{"a":1},"server_ms":4}));
        assert_eq!(ok.server_ms(), Some(4));
        assert_eq!(ok.into_result().unwrap(), json!({"a":1}));
        let failed =
            reply(json!({"ok":false,"error":{"code":"not_found","message":"no such shell"}}));
        let error = failed.into_result().unwrap_err();
        assert_eq!(
            (error.code(), error.message().as_str()),
            ("not_found", "no such shell")
        );
        assert!(!error.is_link_loss());
        let odd = reply(json!({"ok":false})).into_result().unwrap_err();
        assert_eq!(odd.code(), "invalid_response");
        assert_eq!(CallError::Timeout.code(), "timeout");
        assert!(CallError::Timeout.is_link_loss());
        let offline = CallError::Offline("host offline (or access revoked)".into());
        assert_eq!(offline.code(), "offline");
        assert!(offline.message().contains("host offline") && offline.is_link_loss());
        assert_eq!(CallError::Invalid("x".into()).code(), "invalid_request");
        let lost = CallError::Lost("the relay closed the connection".into());
        assert_eq!(lost.code(), "connection_lost");
        assert!(lost.message().contains("the relay closed") && lost.is_link_loss());
    }

    #[tokio::test]
    async fn a_reply_that_can_no_longer_come_is_lost_not_refused() {
        let (reply, rx) = oneshot::channel();
        let pending = PendingCall { rx, generation: 1 };
        drop(reply);
        let error = pending.wait(Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(error, CallError::Lost(_)), "{error:?}");
    }
}
