//! The `appearance.get` result: `riwork appearance --json` checked before it
//! goes to the phone.
//!
//! The desktop crate's `src/appearance_file.rs` defines and validates the same
//! document. The two accept and reject the same input, and the matrices in both
//! crates' tests must stay identical. This side also serves as a guard against
//! a CLI that prints something else.
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;

/// The largest document, and the largest CLI output taken for one.
pub const MAX_BYTES: usize = 16 * 1024;
const VERSION: u8 = 1;

/// `#rrggbb` in either case; it is sent lowercase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb(u32);

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("#{:06x}", self.0))
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let digits = text.strip_prefix('#').unwrap_or_default();
        if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(de::Error::custom("expected a #rrggbb color"));
        }
        u32::from_str_radix(digits, 16)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Serialize, Deserialize)]
struct Palette {
    bg: Rgb,
    panel: Rgb,
    panel_active: Rgb,
    divider: Rgb,
    cyan: Rgb,
    magenta: Rgb,
    gold: Rgb,
    text: Rgb,
    muted: Rgb,
}

#[derive(Serialize, Deserialize)]
struct Terminal {
    background: Rgb,
    foreground: Rgb,
    palette: [Rgb; 16],
}

#[derive(Serialize, Deserialize)]
struct Document {
    v: u8,
    updated_at: u64,
    dark: bool,
    palette: Palette,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal: Option<Terminal>,
}

/// The document re-serialized in the contract's shape (lowercase colors, known
/// fields only), or None when it is over 16 KiB or not a version 1 document.
pub fn validate(bytes: &[u8]) -> Option<Value> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let document: Document = serde_json::from_slice(bytes).ok()?;
    if document.v != VERSION {
        return None;
    }
    serde_json::to_value(&document).ok()
}
