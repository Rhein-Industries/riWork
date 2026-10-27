//! Rust exports consumed by the isolated TSX fixture and the custom AppKit host.
use nsh_core::prelude::*;

#[native_module("riwork/native")]
fn native(registry: &mut Registry) {
    registry.module::<Probe>().class::<ReplayCounter>();
}

/// Identifies the native code that serves this window's bundle.
#[module]
pub struct Probe;

#[module]
impl Probe {
    /// Returns a window-specific native bridge identifier.
    pub fn identity(window_id: String) -> String {
        format!("riwork/native:0.3.0:{window_id}")
    }
}

/// Per-VM state in Rust; each streamed chunk crosses the TSX/native boundary.
#[class]
pub struct ReplayCounter {
    chunks: u32,
    bytes: u32,
    checksum: u32,
}

#[class]
impl ReplayCounter {
    /// Starts a fresh native stream counter.
    pub fn new() -> Self {
        Self {
            chunks: 0,
            bytes: 0,
            checksum: 2_166_136_261,
        }
    }

    /// Counts and hashes one UTF-8 chunk.
    pub fn append(&mut self, chunk: String) -> u32 {
        self.chunks += 1;
        self.bytes += chunk.len() as u32;
        for byte in chunk.bytes() {
            self.checksum = (self.checksum ^ u32::from(byte)).wrapping_mul(16_777_619);
        }
        self.chunks
    }

    /// Number of chunks received by this instance.
    #[getter]
    pub fn chunks(&self) -> u32 {
        self.chunks
    }

    /// Number of UTF-8 bytes received by this instance.
    #[getter]
    pub fn bytes(&self) -> u32 {
        self.bytes
    }

    /// FNV-1a over the received UTF-8 bytes, hexadecimal and zero padded.
    #[getter]
    pub fn checksum(&self) -> String {
        format!("{:08x}", self.checksum)
    }
}
