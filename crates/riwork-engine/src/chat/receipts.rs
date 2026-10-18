//! Durable, opt-in command deduplication for the single chat host.
//!
//! Reserve an operation as `Uncertain` and sync it before any action. Only the
//! reservation's creator may dispatch; every retry returns the saved receipt.
//! A crash between dispatch and settlement remains uncertain and is never
//! replayed automatically. Receipts outlive deleted chats so a key cannot be
//! reused to act on a different chat. They store a command digest, not its text.

use super::model::ChatCommand;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const DIRECTORY: &str = "chat-command-receipts";
const MAX_RECEIPT: u64 = 64 << 10;
pub const OPERATION_CONFLICT: &str = "COMMAND_OPERATION_CONFLICT";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// The host completed dispatch successfully. This does not mean the
    /// provider's turn finished or that requested settings became effective.
    Accepted,
    /// Validation failed before the host attempted the operation.
    Rejected,
    /// Reserved, executing, or interrupted before a durable outcome was saved.
    /// Query again to observe settlement; never resend under a new operation ID.
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReceipt {
    pub operation_id: String,
    pub chat_id: String,
    pub command_sha256: String,
    pub status: CommandStatus,
    pub created_at_unix: u64,
    pub updated_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    receipt: CommandReceipt,
}

pub(super) enum Reservation {
    New(CommandReceipt),
    Existing(CommandReceipt),
}

fn validate_operation_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err("operation_id must contain 1 to 256 bytes without control characters".into());
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Hash the typed command, so input JSON property order is immaterial. Object
/// keys are sorted even when another workspace crate enables preserve_order.
pub(super) fn command_digest(
    command: &ChatCommand,
    require_running: bool,
) -> Result<String, String> {
    command_digest_guarded(command, require_running, None)
}

pub(super) fn command_digest_guarded(
    command: &ChatCommand,
    require_running: bool,
    guard: Option<&super::wire::CommandGuard>,
) -> Result<String, String> {
    let mut value = serde_json::to_value(command).map_err(|error| error.to_string())?;
    if require_running {
        value = serde_json::json!({"command": value, "require_running": true});
    }
    if let Some(guard) = guard {
        value = serde_json::json!({"command": value, "guard": guard});
    }
    value.sort_all_objects();
    serde_json::to_vec(&value)
        .map(|bytes| digest(&bytes))
        .map_err(|error| error.to_string())
}

fn receipt_path(home: &Path, operation_id: &str) -> PathBuf {
    home.join(DIRECTORY)
        .join(format!("{}.json", digest(operation_id.as_bytes())))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Query without starting a provider or requiring that the chat still exists.
/// Corruption is an error, never a missing receipt and permission to dispatch.
pub fn read(
    home: &Path,
    chat_id: &str,
    operation_id: &str,
) -> Result<Option<CommandReceipt>, String> {
    validate_operation_id(operation_id)?;
    let path = receipt_path(home, operation_id);
    let mut bytes = Vec::new();
    match File::open(&path) {
        Ok(file) => file
            .take(MAX_RECEIPT + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Cannot read command receipt: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot read command receipt: {error}")),
    };
    if bytes.len() as u64 > MAX_RECEIPT {
        return Err("command receipt exceeds its size limit".into());
    }
    let stored: Stored = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Cannot decode command receipt: {error}"))?;
    if stored.version != 1 || stored.receipt.operation_id != operation_id {
        return Err("command receipt identity or version is invalid".into());
    }
    if stored.receipt.chat_id != chat_id {
        return Err(format!(
            "{OPERATION_CONFLICT}: operation_id belongs to another chat"
        ));
    }
    Ok(Some(stored.receipt))
}

/// The filesystem lock protects only reservation/settlement, never provider
/// I/O. A concurrent Stop must remain able to cancel a blocked command.
fn acquire(home: &Path) -> Result<File, String> {
    let directory = home.join(DIRECTORY);
    crate::paths::create_private_dir(&directory).map_err(|error| error.to_string())?;
    // Persist the directory entry too, before a reservation can authorize I/O.
    File::open(home)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("Cannot sync command receipt directory: {error}"))?;
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join(".lock"))
        .map_err(|error| format!("Cannot open command receipt lock: {error}"))?;
    lock.lock_exclusive()
        .map_err(|error| format!("Cannot lock command receipts: {error}"))?;
    Ok(lock)
}

fn write(home: &Path, receipt: &CommandReceipt) -> Result<(), String> {
    let path = receipt_path(home, &receipt.operation_id);
    let directory = home.join(DIRECTORY);
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec(&Stored {
        version: 1,
        receipt: receipt.clone(),
    })
    .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RECEIPT {
        return Err("command receipt exceeds its size limit".into());
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        File::open(&directory)?.sync_all()
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("Cannot persist command receipt: {error}"));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn reserve(
    home: &Path,
    chat_id: &str,
    operation_id: &str,
    command: &ChatCommand,
    require_running: bool,
) -> Result<Reservation, String> {
    reserve_guarded(home, chat_id, operation_id, command, require_running, None)
}

pub(super) fn reserve_guarded(
    home: &Path,
    chat_id: &str,
    operation_id: &str,
    command: &ChatCommand,
    require_running: bool,
    guard: Option<&super::wire::CommandGuard>,
) -> Result<Reservation, String> {
    validate_operation_id(operation_id)?;
    let command_sha256 = command_digest_guarded(command, require_running, guard)?;
    let _lock = acquire(home)?;
    if let Some(existing) = read(home, chat_id, operation_id)? {
        if existing.command_sha256 != command_sha256 {
            return Err(format!(
                "{OPERATION_CONFLICT}: operation_id belongs to another command"
            ));
        }
        return Ok(Reservation::Existing(existing));
    }
    let created = now();
    let receipt = CommandReceipt {
        operation_id: operation_id.into(),
        chat_id: chat_id.into(),
        command_sha256,
        status: CommandStatus::Uncertain,
        created_at_unix: created,
        updated_at_unix: created,
        error: None,
    };
    write(home, &receipt)?;
    Ok(Reservation::New(receipt))
}

pub(super) fn settle(home: &Path, receipt: &CommandReceipt) -> Result<(), String> {
    let _lock = acquire(home)?;
    let existing = read(home, &receipt.chat_id, &receipt.operation_id)?
        .ok_or_else(|| "command reservation disappeared before settlement".to_owned())?;
    if existing.status != CommandStatus::Uncertain
        || existing.command_sha256 != receipt.command_sha256
    {
        return Err("command reservation changed before settlement".into());
    }
    write(home, receipt)
}

impl CommandReceipt {
    pub(super) fn outcome(&self, status: CommandStatus, error: Option<String>) -> Self {
        Self {
            status,
            error,
            updated_at_unix: now(),
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests;
