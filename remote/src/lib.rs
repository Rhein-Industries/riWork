pub mod appearance;
#[cfg(unix)]
pub mod bridge;
pub mod client;
#[cfg(unix)]
pub mod client_cli;
#[cfg(unix)]
pub mod client_daemon;
pub mod config;
pub mod connector;
pub mod crypto;
pub mod lanes;
pub mod link;
pub mod pty;
pub mod relay;
pub mod rpc;
pub mod upload;
pub mod viewport;

pub const MAX_FRAME: usize = 262_144;
pub const MAX_PLAINTEXT: usize = 131_072;
pub const HANDSHAKE_SECONDS: u64 = 10;

/// Single-line, bounded form of an untrusted or diagnostic string for stderr.
/// Log lines never carry payloads or secrets, and control characters from a
/// peer must not be able to forge extra lines.
pub(crate) fn log_safe(text: &str) -> String {
    let mut out: String = text
        .chars()
        .take(200)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.chars().count() > 200 {
        out.push_str("...");
    }
    out
}
