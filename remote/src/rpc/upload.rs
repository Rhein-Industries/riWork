//! Files from the phone ("File upload extension" in `docs/remote-protocol.md`): `upload.begin`,
//! `upload.chunk`, `upload.finish` and `upload.cancel` store a file in the inbox of a shell or
//! a chat (`crate::upload`), and `shell.paste` pastes finished uploads into their shell as a
//! drop of those files on its terminal would (`riwork shell paste SHELL -- FILE...`), exactly
//! once per batch UUID, with the same ledger as `shell.keys`.
//!
//! Everything is checked before anything is written, and the connector, not the phone, holds
//! the limits. The phone names no path: an upload names its shell or chat by UUID, and a paste
//! names uploads of this device for that very shell.
use super::*;
use crate::upload::{self, Kind, Refusal, Spec};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

impl From<Refusal> for Fault {
    fn from(refusal: Refusal) -> Self {
        Fault::new(refusal.code, refusal.message)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Begin {
    upload: String,
    shell_id: Option<String>,
    chat_id: Option<String>,
    name: String,
    size: u64,
    #[serde(rename = "type")]
    media_type: Option<String>,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    upload: String,
    offset: u64,
    data: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct One {
    upload: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Paste {
    shell_id: String,
    batch: String,
    uploads: Vec<String>,
}

pub(super) fn begin_spec(r: &Request) -> std::result::Result<Spec, Fault> {
    let p: Begin = params(r)?;
    id(&p.upload)?;
    let (kind, target) = match (p.shell_id, p.chat_id) {
        (Some(shell), None) => (Kind::Shell, shell),
        (None, Some(chat)) => (Kind::Chat, chat),
        _ => return Err(invalid("name exactly one of shell_id and chat_id")),
    };
    id(&target)?;
    if !upload::valid_name(&p.name) {
        return Err(invalid(format!(
            "name must be 1 to {} bytes without control characters",
            upload::NAME_MAX_BYTES
        )));
    }
    if let Some(media_type) = &p.media_type
        && !upload::valid_media_type(media_type)
    {
        return Err(invalid("type must be a media type such as image/jpeg"));
    }
    if !upload::valid_sha256(&p.sha256) {
        return Err(invalid("sha256 must be 64 lowercase hex digits"));
    }
    Ok(Spec {
        upload: p.upload,
        kind,
        target,
        name: p.name,
        media_type: p.media_type,
        size: p.size,
        sha256: p.sha256,
    })
}

/// `upload.chunk`: the upload, the offset and the decoded data (1 to `CHUNK_BYTES`).
pub(super) fn chunk_spec(r: &Request) -> std::result::Result<(String, u64, Vec<u8>), Fault> {
    let p: Chunk = params(r)?;
    id(&p.upload)?;
    // Decoding is bounded by the frame the request came in; check the size first all the same.
    if p.data.len() > upload::CHUNK_BYTES.div_ceil(3) * 4 {
        return Err(invalid(format!(
            "a chunk carries at most {} bytes",
            upload::CHUNK_BYTES
        )));
    }
    let data = URL_SAFE_NO_PAD
        .decode(&p.data)
        .map_err(|_| invalid("data must be unpadded URL-safe base64"))?;
    if data.is_empty() || data.len() > upload::CHUNK_BYTES {
        return Err(invalid(format!(
            "a chunk carries 1 to {} bytes",
            upload::CHUNK_BYTES
        )));
    }
    Ok((p.upload, p.offset, data))
}

pub(super) fn one_spec(r: &Request) -> std::result::Result<String, Fault> {
    let p: One = params(r)?;
    id(&p.upload)?;
    Ok(p.upload)
}

pub(super) struct PasteSpec {
    shell_id: String,
    batch: String,
    uploads: Vec<String>,
}
pub(super) fn paste_spec(r: &Request) -> std::result::Result<PasteSpec, Fault> {
    let p: Paste = params(r)?;
    id(&p.shell_id)?;
    id(&p.batch)?;
    if p.uploads.is_empty() || p.uploads.len() > upload::PASTE_MAX_FILES {
        return Err(invalid(format!(
            "uploads must hold 1 to {} entries",
            upload::PASTE_MAX_FILES
        )));
    }
    for (index, upload) in p.uploads.iter().enumerate() {
        id(upload)?;
        if p.uploads[..index].contains(upload) {
            return Err(invalid("an upload may be pasted once per batch"));
        }
    }
    Ok(PasteSpec {
        shell_id: p.shell_id,
        batch: p.batch,
        uploads: p.uploads,
    })
}

/// What a failed `riwork shell paste` says, as `keys_fault` reads `shell keys`: a CLI error that
/// begins with one of its tokens happened before anything was pasted.
fn paste_fault(error: &anyhow::Error) -> (Fault, bool) {
    let message = error.to_string();
    if message
        .strip_prefix("RiWork CLI failed: riwork: ")
        .is_some_and(|detail| detail.starts_with("Unknown shell command 'paste'"))
    {
        return (paste_unsupported(), true);
    }
    let (fault, not_sent) = keys_fault(error);
    if not_sent {
        return (fault, true);
    }
    (
        Fault::new(
            "cli_error",
            format!(
                "{message}; the paths may be partly pasted, so repeating its batch UUID reports uncertain"
            ),
        ),
        false,
    )
}
fn paste_unsupported() -> Fault {
    Fault::new(
        "cli_error",
        "the installed riwork CLI cannot paste files into a terminal; update RiWork",
    )
}

impl Rpc {
    /// Whether the installed CLI has `shell paste` (`"shell_paste": true` in `riwork
    /// capabilities --json`). Only a yes is remembered, like `require_attach_exec`.
    async fn require_shell_paste(&self) -> std::result::Result<(), Fault> {
        if self.shell_paste.load(Ordering::Relaxed) {
            return Ok(());
        }
        match self.raw(vec!["capabilities".into(), "--json".into()]).await {
            Ok(data) => {
                let reply = serde_json::from_slice::<Value>(&data).unwrap_or(Value::Null);
                if reply.get("v") == Some(&json!(1))
                    && reply.get("shell_paste") == Some(&Value::Bool(true))
                {
                    self.shell_paste.store(true, Ordering::Relaxed);
                    Ok(())
                } else {
                    Err(paste_unsupported())
                }
            }
            Err(e) if e.to_string().starts_with("RiWork CLI failed") => Err(paste_unsupported()),
            Err(e) => Err(cli_fault(e)),
        }
    }

    /// Run a step of the upload store off the async threads: it reads and writes files, and
    /// `finish` hashes the whole file.
    async fn uploads<T: Send + 'static>(
        &self,
        step: impl FnOnce(&upload::Uploads) -> std::result::Result<T, Refusal> + Send + 'static,
    ) -> std::result::Result<T, Fault> {
        let uploads = self.uploads.clone();
        tokio::task::spawn_blocking(move || step(&uploads))
            .await
            .map_err(|e| cli_fault(format!("the upload step was interrupted: {e}")))?
            .map_err(Fault::from)
    }

    pub(super) async fn upload_begin(
        &self,
        device: &str,
        spec: Spec,
    ) -> std::result::Result<Value, Fault> {
        // A file for a shell is good only if it can then be pasted, and only into a shell that is
        // there: say so before the phone sends the first byte.
        if spec.kind == Kind::Shell {
            self.require_shell_paste().await?;
            self.selected(&spec.target).await?;
        }
        let device = device.to_owned();
        let status = self
            .uploads(move |uploads| uploads.begin(&device, &spec))
            .await?;
        let mut answer = status.json();
        answer["chunk_bytes"] = json!(upload::CHUNK_BYTES);
        Ok(answer)
    }

    pub(super) async fn upload_chunk(
        &self,
        device: &str,
        (upload, offset, data): (String, u64, Vec<u8>),
    ) -> std::result::Result<Value, Fault> {
        let device = device.to_owned();
        let status = self
            .uploads(move |uploads| uploads.chunk(&device, &upload, offset, &data))
            .await?;
        Ok(status.json())
    }

    pub(super) async fn upload_finish(
        &self,
        device: &str,
        upload: String,
    ) -> std::result::Result<Value, Fault> {
        let device = device.to_owned();
        let status = self
            .uploads(move |uploads| uploads.finish(&device, &upload))
            .await?;
        Ok(status.json())
    }

    pub(super) async fn upload_cancel(
        &self,
        device: &str,
        upload: String,
    ) -> std::result::Result<Value, Fault> {
        let device = device.to_owned();
        let status = self
            .uploads(move |uploads| uploads.cancel(&device, &upload))
            .await?;
        let mut answer = status.json();
        if status.path.is_none() {
            answer["status"] = json!("cancelled");
        }
        Ok(answer)
    }

    /// Paste finished uploads into their shell, exactly once per (device, batch UUID), with
    /// the ledger and locks of `shell.keys`: a repeat answers `duplicate` or `uncertain`.
    pub(super) async fn shell_paste(
        &self,
        device: &str,
        p: PasteSpec,
    ) -> std::result::Result<Value, Fault> {
        id(device)?;
        let device_lock = self.input_lock(&format!("keys-{device}"));
        let _device_guard = device_lock.lock().await;
        let _lock = self
            .storage
            .lock(&format!("keys-{device}.lock"))
            .map_err(cli_fault)?;
        let path = self.storage.dir.join(format!("keys-{device}.json"));
        let mut ledger: BatchLedger = if path.exists() {
            private_read(&path, 8 * 1024 * 1024).map_err(cli_fault)?
        } else {
            BatchLedger::default()
        };
        let reply =
            |status: &str| Ok(json!({"shell_id":p.shell_id,"batch":p.batch,"status":status}));
        if let Some(record) = ledger.batches.iter().find(|r| r.batch == p.batch) {
            return reply(match record.state {
                BatchState::Sent => "duplicate",
                BatchState::Pending => "uncertain",
            });
        }
        let files = {
            let (device, shell, uploads) =
                (device.to_owned(), p.shell_id.clone(), p.uploads.clone());
            self.uploads(move |store| store.paths(&device, Kind::Shell, &shell, &uploads))
                .await?
        };
        self.require_shell_paste().await?;
        let shell_lock = self.input_lock(&p.shell_id);
        let _shell_guard = shell_lock.lock().await;
        let checked = self.ensure_selected(&p.shell_id).await?;
        if !self.storage.authorized(device).map_err(cli_fault)? {
            return Err(Fault::new("not_found", "device revoked"));
        }
        let excess = (ledger.batches.len() + 1).saturating_sub(KEYS_LEDGER_MAX);
        ledger.batches.drain(..excess);
        ledger.batches.push(BatchRecord {
            batch: p.batch.clone(),
            state: BatchState::Pending,
        });
        private_write(&path, &ledger).map_err(cli_fault)?; // durable before pasting
        let mut args: Vec<String> = ["shell", "paste", p.shell_id.as_str(), "--"]
            .into_iter()
            .map(String::from)
            .collect();
        args.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
        match self.raw(args).await {
            Ok(_) => {
                if let Some(record) = ledger.batches.last_mut() {
                    record.state = BatchState::Sent;
                }
                // As for `shell.keys`: if this write is lost the batch stays pending, which
                // says exactly what is known.
                let _ = private_write_relaxed(&path, &ledger);
                reply("sent")
            }
            Err(error) => {
                let (fault, not_sent) = paste_fault(&error);
                let fault = checked.explain(fault, &p.shell_id);
                if not_sent {
                    ledger.batches.pop();
                    let _ = private_write(&path, &ledger);
                }
                Err(fault)
            }
        }
    }

    /// Everything a device that was removed or revoked had sent.
    pub async fn forget_uploads(&self, device: &str) {
        let device = device.to_owned();
        let _ = self
            .uploads(move |uploads| {
                uploads.forget_device(&device);
                Ok(())
            })
            .await;
    }

    /// The hourly (and startup) sweep of the uploads: what has had its time, and everything of
    /// devices that are not paired any more.
    pub async fn sweep_uploads(&self) {
        let storage = self.storage.clone();
        let _ = self
            .uploads(move |uploads| {
                let paired = storage
                    .config()
                    .map(|c| {
                        c.devices
                            .into_iter()
                            .filter(|d| !d.revoked)
                            .map(|d| d.pairing.device_id)
                            .collect::<std::collections::HashSet<_>>()
                    })
                    // A config that cannot be read decides nothing: keep everything.
                    .map_err(|_| refuse_nothing())?;
                uploads.sweep(|device| paired.contains(device));
                Ok(())
            })
            .await;
    }
}

fn refuse_nothing() -> Refusal {
    Refusal {
        code: "cli_error",
        message: String::new(),
    }
}
