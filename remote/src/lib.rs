pub mod config;
pub mod connector;
pub mod crypto;
pub mod relay;
pub mod rpc;

pub const MAX_FRAME: usize = 262_144;
pub const MAX_PLAINTEXT: usize = 131_072;
pub const HANDSHAKE_SECONDS: u64 = 10;
