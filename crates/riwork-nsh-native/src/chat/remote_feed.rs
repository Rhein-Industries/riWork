//! The existing chat view over a paired host. Reads resume by contiguous cursor;
//! every queued mutation gets one RPC exchange and an explicit receipt.
use super::feed::{Backoff, Delivery, FeedMsg, Link, Sink};
use riwork_engine::{
    chat::{
        model::{ChatCommand, ChatEvent},
        wire::Envelope,
    },
    remote_hosts::RemoteError,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

pub type Rpc = Arc<dyn Fn(&str, &str, Value, Duration) -> Result<Value, RemoteError> + Send + Sync>;
#[derive(Clone)]
pub struct Target {
    pub host: String,
    pub chat: String,
    pub rpc: Rpc,
}
impl Target {
    pub fn new(host: String, chat: String) -> Result<Self, String> {
        if host.is_empty()
            || host.len() > 256
            || host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == ':')
        {
            return Err("invalid: remote host identity".into());
        }
        crate::service::validate_uuid(&chat)?;
        Ok(Self {
            host,
            chat,
            rpc: Arc::new(crate::remote::call_host),
        })
    }
    pub fn key(&self) -> String {
        format!("remote-chat:{}:{}", self.host, self.chat)
    }
    /// UUID-shaped storage identity, namespaced by the full host and chat pair.
    /// DraftStore deliberately permits only canonical UUID filenames.
    pub fn draft_key(&self) -> String {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(self.key().as_bytes());
        let mut bytes: [u8; 16] = hash[..16].try_into().expect("SHA-256 prefix");
        bytes[6] = (bytes[6] & 0x0f) | 0x80;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid::Uuid::from_bytes(bytes).to_string()
    }
    pub fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RemoteError> {
        (self.rpc)(&self.host, method, params, timeout)
    }
    pub fn command(&self, command: ChatCommand) -> Result<(), String> {
        // Wire Option fields are omitted: older remote validators reject explicit nulls.
        let mut value = serde_json::to_value(command).map_err(|e| format!("refused: {e}"))?;
        if let Some(fields) = value.as_object_mut() {
            fields.retain(|_, value| !value.is_null());
        }
        let reply = self
            .call(
                "chat.command",
                json!({"chat_id":self.chat,"command":value}),
                Duration::from_secs(90),
            )
            .map_err(command_error)?;
        if reply["status"] == "ok" {
            Ok(())
        } else {
            Err("broken: The host's command receipt could not be read; check the chat before trying again".into())
        }
    }
}

pub fn command_error(error: RemoteError) -> String {
    match error {
        RemoteError::Rpc { code, message }
            if matches!(
                code.as_str(),
                "invalid_params"
                    | "invalid_request"
                    | "not_found"
                    | "unsupported"
                    | "unsupported_method"
                    | "method_not_found"
                    | "refused"
                    | "permission_denied"
            ) =>
        {
            format!("refused: {message}")
        }
        other => format!("broken: {}. The command was not retried", other.message()),
    }
}

/// Validate the entire page before advancing; a corrupt suffix must not hide events.
pub fn page(value: Value, chat: &str, since: u64) -> Result<(Vec<Envelope>, u64, bool), String> {
    if value["chat_id"].as_str() != Some(chat) {
        return Err("The host answered for another chat".into());
    }
    let rows = value["events"]
        .as_array()
        .ok_or("The host's event page is missing")?;
    if rows.len() > 500 {
        return Err("The host's event page is too large".into());
    }
    let mut cursor = since;
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let seq = row["seq"].as_u64().ok_or("Invalid chat event sequence")?;
        if cursor.checked_add(1) != Some(seq) {
            return Err("Chat event sequence gap; reconnecting from the last applied event".into());
        }
        let event: ChatEvent = serde_json::from_value(row["event"].clone())
            .map_err(|e| format!("Unsupported chat event: {e}"))?;
        if matches!(&event, ChatEvent::Info { info } if info.id != chat) {
            return Err("The host returned metadata for another chat".into());
        }
        events.push(Envelope {
            chat_id: chat.into(),
            seq,
            event,
        });
        cursor = seq;
    }
    let next = value["next"].as_u64().ok_or("Missing chat cursor")?;
    let more = value["more"].as_bool().ok_or("Missing chat continuation")?;
    if next != cursor || (more && cursor == since) {
        return Err("Invalid chat continuation cursor".into());
    }
    Ok((events, cursor, more))
}

pub struct RemoteFeed {
    stop: Arc<AtomicBool>,
}
impl RemoteFeed {
    pub fn start(target: Target, sink: Sink) -> (Self, mpsc::Sender<Delivery>) {
        let stop = Arc::new(AtomicBool::new(false));
        let (commands, inbox) = mpsc::channel();
        let sending = target.clone();
        thread::spawn(move || {
            for delivery in inbox {
                // Unlike the local socket delivery path, NO mutation is automatically retried.
                let (command, receipt) = match delivery {
                    Delivery::Command { command, receipt } => (command, receipt),
                    Delivery::Submission { command, receipt } => (command, Some(receipt)),
                };
                let result = sending.command(command);
                if let Some(receipt) = receipt {
                    let _ = receipt.send(result);
                }
            }
        });
        let stopped = stop.clone();
        thread::spawn(move || {
            let mut cursor = 0;
            let mut failures = 0;
            let mut live = false;
            while !stopped.load(Ordering::Acquire) {
                let result = target.call("chat.events", json!({"chat_id":target.chat,"since":cursor,"wait_ms":2000,"max_events":500,"complete":true,"features":["rate_limits"]}), Duration::from_secs(15));
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                match result
                    .and_then(|v| page(v, &target.chat, cursor).map_err(RemoteError::Protocol))
                {
                    Ok((events, next, more)) => {
                        if !events.is_empty() && !sink(FeedMsg::Events(events)) {
                            break;
                        }
                        cursor = next;
                        failures = 0;
                        if !live {
                            if !sink(FeedMsg::Link(Link::Live)) {
                                break;
                            }
                            live = true;
                        }
                        if !more {
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                    Err(error) => {
                        live = false;
                        // A revoked/missing host is not proof that this chat was deleted.
                        if !sink(FeedMsg::Link(Link::Reconnecting)) {
                            break;
                        }
                        failures += 1;
                        if failures == 1 {
                            let _ = sink(FeedMsg::CommandFailed {
                                command: ChatCommand::Interrupt,
                                error: format!("Remote chat: {}", error.message()),
                            });
                        }
                        let delay = Backoff::DEFAULT.delay(failures);
                        let until = std::time::Instant::now() + delay;
                        while !stopped.load(Ordering::Acquire) && std::time::Instant::now() < until
                        {
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            }
        });
        (Self { stop }, commands)
    }
}
impl Drop for RemoteFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_identity_and_cursor_are_strict_and_never_skip_a_gap() {
        let event = json!({"seq":1,"event":{"event":"state","state":{"state":"stopped"}}});
        let valid = json!({"chat_id":"chat","events":[event.clone()],"next":1,"more":false});
        assert_eq!(page(valid.clone(), "chat", 0).unwrap().1, 1);
        assert!(page(valid.clone(), "another", 0).is_err());
        assert!(page(valid, "chat", 1).is_err());
        assert!(page(json!({"chat_id":"chat","events":[event,{"seq":3,"event":{"event":"state","state":{"state":"stopped"}}}],"next":3,"more":false}), "chat", 0).is_err());
        assert!(
            page(
                json!({"chat_id":"chat","events":[],"next":7,"more":false}),
                "chat",
                0
            )
            .is_err()
        );
    }
    #[test]
    fn ambiguous_remote_submission_is_sent_once_and_host_scoped() {
        use std::sync::Mutex;
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        let target = Target {
            host: "host-b".into(),
            chat: "chat".into(),
            rpc: Arc::new(move |host, method, params, _| {
                captured
                    .lock()
                    .unwrap()
                    .push((host.to_owned(), method.to_owned(), params));
                Err(RemoteError::Timeout)
            }),
        };
        assert!(
            target
                .command(ChatCommand::Send {
                    text: "hello".into()
                })
                .unwrap_err()
                .starts_with("broken:")
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "host-b");
        assert_eq!(calls[0].2["chat_id"], "chat");
        assert_ne!(
            target.key(),
            Target {
                host: "host-a".into(),
                ..target.clone()
            }
            .key()
        );
    }
}
