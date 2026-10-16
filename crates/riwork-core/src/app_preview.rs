//! Cooperative application preview protocol and persisted launch settings.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PREVIEW_PROTOCOL_VERSION: u32 = 1;
pub const MAX_PREVIEW_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PREVIEW_INPUT_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_PREVIEW_DIMENSION: u32 = 2048;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMode {
    #[default]
    Cooperative,
    WindowCapture,
    IosSimulator,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PreviewSpec {
    pub command: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub mode: PreviewMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ios: Option<IosPreviewSpec>,
}

/// A local Xcode simulator build. RiWork always creates a separate device; a
/// user's existing booted simulator is never selected or shut down.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IosPreviewSpec {
    /// An `.xcodeproj` or `.xcworkspace`, relative to the working folder.
    #[serde(default)]
    pub project: Option<PathBuf>,
    pub scheme: String,
    #[serde(default)]
    pub bundle_id: Option<String>,
    /// CoreSimulator device-type identifier or exact display name.
    #[serde(default)]
    pub device_type: Option<String>,
    /// Installed iOS runtime identifier or exact display name.
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default = "debug_configuration")]
    pub configuration: String,
}

fn debug_configuration() -> String {
    "Debug".into()
}

impl Default for IosPreviewSpec {
    fn default() -> Self {
        Self {
            project: None,
            scheme: String::new(),
            bundle_id: None,
            device_type: None,
            runtime: None,
            configuration: debug_configuration(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMouseButton {
    Left,
    Right,
    Middle,
}

/// Coordinates are pixels in the streamed frame; an iOS adapter maps them into
/// device coordinates. Cooperative resize dimensions are logical pixels at one.
/// Key names use GPUI syntax, for example `cmd-a`, `backspace`, and `enter`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreviewInput {
    Click {
        x: f32,
        y: f32,
        button: PreviewMouseButton,
    },
    PointerDown {
        x: f32,
        y: f32,
        button: PreviewMouseButton,
    },
    PointerUp {
        x: f32,
        y: f32,
        button: PreviewMouseButton,
    },
    PointerMove {
        x: f32,
        y: f32,
    },
    Scroll {
        x: f32,
        y: f32,
        delta_x: f32,
        delta_y: f32,
    },
    Text {
        text: String,
    },
    Key {
        key: String,
    },
    KeyUp {
        key: String,
    },
    Resize {
        width: u32,
        height: u32,
    },
    Focus {
        focused: bool,
    },
    ShowWindow,
    /// Publish a new frame even when the application's pixels are unchanged.
    Refresh,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PreviewClientMessage {
    Hello {
        token: String,
        pid: u32,
        protocol: u32,
    },
    Frame {
        png: String,
        width: u32,
        height: u32,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PreviewHostMessage {
    Ready { protocol: u32 },
    Input { input: PreviewInput },
    Visibility { visible: bool },
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_uses_documented_tags_and_round_trips_unicode_input() {
        let message = PreviewHostMessage::Input {
            input: PreviewInput::Text {
                text: "hello 👋".into(),
            },
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type":"input", "input":{"kind":"text", "text":"hello 👋"}})
        );
        let decoded: PreviewHostMessage = serde_json::from_value(json).unwrap();
        assert!(
            matches!(decoded, PreviewHostMessage::Input { input: PreviewInput::Text { text } } if text == "hello 👋")
        );
    }
}
