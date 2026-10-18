//! Read-only previews of local references actually present in a pinned chat item.
//! Immutable, bounded transfer snapshots avoid reopening paths/CLI work for every chunk.
use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{
    io::Read,
    path::{Component, Path},
    time::Instant,
};

const FILE_MAX: usize = 8 * 1024 * 1024;
const CACHE_MAX: usize = 16 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
const TTL: Duration = Duration::from_secs(120);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetRequest {
    chat_id: String,
    cursor: String,
    item_id: String,
    reference: String,
    offset: usize,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    device: String,
    chat: String,
    cursor: String,
    item: String,
    reference: String,
}
struct Asset {
    bytes: Vec<u8>,
    hash: String,
    name: String,
    media_type: &'static str,
}
#[derive(Default)]
pub(super) struct AssetCache {
    entries: BTreeMap<Key, (Arc<Asset>, Instant)>,
}
impl AssetCache {
    fn prune(&mut self) {
        self.entries.retain(|_, (_, used)| used.elapsed() < TTL);
    }
    fn get(&mut self, key: &Key) -> Option<Arc<Asset>> {
        self.prune();
        self.entries.get_mut(key).map(|(asset, used)| {
            *used = Instant::now();
            asset.clone()
        })
    }
    fn insert(&mut self, key: Key, value: Arc<Asset>) {
        self.prune();
        while self.entries.len() >= 8
            || self
                .entries
                .values()
                .map(|(a, _)| a.bytes.len())
                .sum::<usize>()
                + value.bytes.len()
                > CACHE_MAX
        {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.entries.insert(key, (value, Instant::now()));
    }
}

fn validate(p: &AssetRequest) -> std::result::Result<(), Fault> {
    id(&p.chat_id)?;
    if p.cursor.is_empty()
        || p.cursor.len() > 80
        || !p
            .cursor
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase() || b == b'-')
        || p.item_id.is_empty()
        || p.item_id.len() > 512
        || p.reference.is_empty()
        || p.reference.len() > 4096
        || p.reference.chars().any(char::is_control)
        || p.reference.contains(':')
        || p.reference.starts_with("//")
        || p.offset > FILE_MAX
        || p.offset % CHUNK != 0
    {
        return Err(invalid("invalid chat asset reference or offset"));
    }
    Ok(())
}

/// Match complete Markdown destinations, never arbitrary substrings or plain model text.
fn referenced(item: &Value, reference: &str) -> bool {
    if item["presentation"]["images"]
        .as_array()
        .is_some_and(|images| {
            images
                .iter()
                .any(|i| i["source"]["kind"] == "local" && i["source"]["path"] == reference)
        })
    {
        return true;
    }
    if item["body"]["type"] != "agent_message" {
        return false;
    }
    let Some(text) = item["body"]["text"].as_str() else {
        return false;
    };
    text.split("](").skip(1).any(|part| {
        let Some((target, _)) = part.split_once(')') else {
            return false;
        };
        target == reference
            || target.strip_prefix('<').and_then(|t| t.strip_suffix('>')) == Some(reference)
    })
}
fn media_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "pdf" => Some("application/pdf"),
        "txt" | "md" | "csv" | "json" => Some("text/plain"),
        _ => None,
    }
}

/// Walk from an already-open trusted workspace directory, refusing symlinks at every step.
#[cfg(unix)]
fn open_beneath(root: &Path, reference: &str) -> std::io::Result<std::fs::File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::ffi::OsStrExt,
        },
    };
    let root = root.canonicalize()?;
    let path = Path::new(reference);
    let relative = if path.is_absolute() {
        path.strip_prefix(&root)
            .map_err(|_| std::io::ErrorKind::PermissionDenied)?
    } else {
        path
    };
    let parts: Vec<_> = relative
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect();
    if parts.is_empty() || parts.iter().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    let root_name =
        CString::new(root.as_os_str().as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
    let fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(fd) };
    for (index, part) in parts.iter().enumerate() {
        let name = CString::new(part.as_os_str().as_bytes())
            .map_err(|_| std::io::ErrorKind::InvalidInput)?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if index + 1 < parts.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        directory = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    Ok(std::fs::File::from(directory))
}
#[cfg(not(unix))]
fn open_beneath(_: &Path, _: &str) -> std::io::Result<std::fs::File> {
    Err(std::io::ErrorKind::Unsupported.into())
}

fn load_asset(root: &Path, reference: &str) -> std::result::Result<Asset, Fault> {
    let path = Path::new(reference);
    let media_type = media_type(path).ok_or_else(|| {
        Fault::new(
            "asset_unavailable",
            "This file type cannot be previewed on the phone.",
        )
    })?;
    let file = open_beneath(root, reference).map_err(|_| {
        Fault::new(
            "asset_unavailable",
            "The file must be a regular file inside this chat's workspace, without symlinks.",
        )
    })?;
    let meta = file.metadata().map_err(cli_fault)?;
    if !meta.is_file() || meta.len() > FILE_MAX as u64 {
        return Err(Fault::new(
            "asset_unavailable",
            "Preview files must be regular files no larger than 8 MiB.",
        ));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(FILE_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(cli_fault)?;
    if bytes.len() > FILE_MAX {
        return Err(Fault::new(
            "asset_unavailable",
            "Preview file grew beyond 8 MiB.",
        ));
    }
    Ok(Asset {
        hash: hex::encode(Sha256::digest(&bytes)),
        bytes,
        name: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("preview")
            .into(),
        media_type,
    })
}

impl Rpc {
    pub(super) async fn chat_asset_read(
        &self,
        device: &str,
        r: &Request,
    ) -> std::result::Result<Value, Fault> {
        let p: AssetRequest = params(r)?;
        validate(&p)?;
        self.still_authorized(device)?;
        let key = Key {
            device: device.into(),
            chat: p.chat_id.clone(),
            cursor: p.cursor.clone(),
            item: p.item_id.clone(),
            reference: p.reference.clone(),
        };
        let cached = {
            let mut cache = self
                .chat_assets
                .lock()
                .map_err(|_| cli_fault("asset cache unavailable"))?;
            cache.get(&key)
        };
        let asset = if let Some(asset) = cached {
            asset
        } else {
            if p.offset != 0 {
                return Err(Fault::new(
                    "asset_expired",
                    "The preview expired. Open the file again.",
                ));
            }
            // Reuse the snapshot validator and bounded CLI reader; the phone cannot supply a cwd.
            let spec = chat::snapshot_spec(
                &json!({"chat_id":p.chat_id,"cursor":p.cursor,"item_ids":[p.item_id]}),
            )?;
            let matches = |snapshot: &Value| {
                snapshot["items"].as_array().is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row["item"]["id"] == p.item_id && referenced(&row["item"], &p.reference)
                    })
                })
            };
            let snapshot = self.chat_snapshot(&r.id, spec, MAX_PLAINTEXT).await;
            let mut found = match snapshot {
                Ok(snapshot) => matches(&snapshot),
                Err(fault) if fault.code == "snapshot_expired" => false,
                Err(fault) => return Err(fault),
            };
            if !found {
                // A newly streamed item can be newer than the phone's historical prefix.
                // Pin a fresh bounded snapshot, then hydrate only the exact requested item.
                let fresh = self
                    .chat_snapshot(
                        &r.id,
                        chat::snapshot_spec(&json!({"chat_id":p.chat_id,"limit":1}))?,
                        MAX_PLAINTEXT,
                    )
                    .await?;
                found = matches(&fresh);
                if !found {
                    let spec = chat::snapshot_spec(
                        &json!({"chat_id":p.chat_id,"cursor":fresh["cursor"],"item_ids":[p.item_id]}),
                    )?;
                    found = matches(&self.chat_snapshot(&r.id, spec, MAX_PLAINTEXT).await?);
                }
            }
            if !found {
                return Err(Fault::new(
                    "asset_unavailable",
                    "This file reference is not in the selected chat message.",
                ));
            }
            let chats = array(
                self.read_capped(
                    vec!["chat".into(), "list".into()],
                    CLI_TIMEOUT,
                    MAX_LIST_OUTPUT,
                )
                .await?,
            )?;
            let root = chats
                .iter()
                .find(|c| c["id"] == p.chat_id)
                .and_then(|c| c["cwd"].as_str())
                .map(PathBuf::from)
                .ok_or_else(|| Fault::new("not_found", "chat not found"))?;
            let reference = p.reference.clone();
            let loaded = tokio::task::spawn_blocking(move || load_asset(&root, &reference))
                .await
                .map_err(cli_fault)??;
            self.still_authorized(device)?;
            let asset = Arc::new(loaded);
            self.chat_assets
                .lock()
                .map_err(|_| cli_fault("asset cache unavailable"))?
                .insert(key, asset.clone());
            asset
        };
        if p.offset > asset.bytes.len() {
            return Err(invalid("asset offset is past the file"));
        }
        let end = (p.offset + CHUNK).min(asset.bytes.len());
        Ok(
            json!({"chat_id":p.chat_id,"item_id":p.item_id,"reference":p.reference,"offset":p.offset,"total":asset.bytes.len(),"data":URL_SAFE_NO_PAD.encode(&asset.bytes[p.offset..end]),"sha256":asset.hash,"name":asset.name,"media_type":asset.media_type}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_cache_expires_when_idle_and_caps_total_entries() {
        let key = |n| Key {
            device: "device".into(),
            chat: "chat".into(),
            cursor: "cursor".into(),
            item: format!("item{n}"),
            reference: "plot.png".into(),
        };
        let asset = Arc::new(Asset {
            bytes: vec![],
            hash: String::new(),
            name: "plot.png".into(),
            media_type: "image/png",
        });
        let mut cache = AssetCache::default();
        for n in 0..20 {
            cache.insert(key(n), asset.clone());
        }
        assert_eq!(
            cache.entries.len(),
            8,
            "even empty files cannot create unbounded cache entries"
        );
        let latest = key(19);
        let idle_since = Instant::now() - TTL / 2;
        cache.entries.get_mut(&latest).unwrap().1 = idle_since;
        assert!(cache.get(&latest).is_some());
        assert!(
            cache.entries[&latest].1 > idle_since,
            "active transfers refresh their idle expiry"
        );
        cache.entries.get_mut(&latest).unwrap().1 = Instant::now() - TTL;
        assert!(cache.get(&latest).is_none());
    }
    #[test]
    fn references_require_an_exact_chat_destination() {
        let item = json!({"body":{"type":"agent_message","text":"![plot](plots/a.png) [report](<out/report.pdf>)"}});
        assert!(referenced(&item, "plots/a.png"));
        assert!(referenced(&item, "out/report.pdf"));
        assert!(!referenced(&item, "a.png"));
        assert!(!referenced(&item, "secret.txt"));
        assert!(!referenced(
            &json!({"body":{"type":"user_message","text":"[x](secret.txt)"}}),
            "secret.txt"
        ));
    }
    #[test]
    fn rejects_url_and_unbounded_or_misaligned_requests() {
        let mut p = AssetRequest {
            chat_id: "11111111-1111-4111-8111-111111111111".into(),
            cursor: "abcd-1234".into(),
            item_id: "item".into(),
            reference: "plot.png".into(),
            offset: 0,
        };
        assert!(validate(&p).is_ok());
        for bad in [
            "https://x/a.png",
            "file:///a.png",
            "//server/a.png",
            "a\n.png",
        ] {
            p.reference = bad.into();
            assert!(validate(&p).is_err());
        }
        p.reference = "plot.png".into();
        p.offset = 1;
        assert!(validate(&p).is_err());
        p.offset = FILE_MAX + CHUNK;
        assert!(validate(&p).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn workspace_walk_rejects_escape_symlinks_and_special_files() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("ok.txt"), b"hello").unwrap();
        std::fs::write(temp.path().join("secret.txt"), b"secret").unwrap();
        assert_eq!(load_asset(&root, "ok.txt").unwrap().bytes, b"hello");
        assert!(load_asset(&root, "../secret.txt").is_err());
        symlink(temp.path().join("secret.txt"), root.join("link.txt")).unwrap();
        assert!(load_asset(&root, "link.txt").is_err());
        symlink(temp.path(), root.join("outside")).unwrap();
        assert!(load_asset(&root, "outside/secret.txt").is_err());
        assert!(load_asset(&root, temp.path().join("secret.txt").to_str().unwrap()).is_err());
        std::fs::create_dir(root.join("folder.txt")).unwrap();
        assert!(load_asset(&root, "folder.txt").is_err());
        std::fs::write(root.join("large.txt"), vec![0; FILE_MAX + 1]).unwrap();
        assert!(load_asset(&root, "large.txt").is_err());
    }
}
