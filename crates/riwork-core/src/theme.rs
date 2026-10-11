//! Serialized theme selection shared with the desktop.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    Ghostty,
    Native,
    RiWork,
    Catppuccin,
    TokyoNight,
    GruvboxLight,
    Hermes,
}

impl ThemeChoice {
    pub const ALL: [Self; 7] = [
        Self::Ghostty,
        Self::Native,
        Self::RiWork,
        Self::Catppuccin,
        Self::TokyoNight,
        Self::GruvboxLight,
        Self::Hermes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ghostty => "Follow Ghostty",
            Self::Native => "Native",
            Self::RiWork => "RiWork",
            Self::Catppuccin => "Catppuccin Mocha",
            Self::TokyoNight => "Tokyo Night",
            Self::GruvboxLight => "Gruvbox Light",
            Self::Hermes => "Hermes",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Ghostty => {
                "Use your Ghostty colors throughout RiWork. Configuration changes sync automatically."
            }
            Self::Native => {
                "Black on white, or white on black when macOS is dark. Follows the system appearance."
            }
            Self::RiWork => "RiWork's original dark palette with cyan and purple accents.",
            Self::Catppuccin => "A soft dark palette with pastel accents.",
            Self::TokyoNight => "A cool dark palette inspired by Tokyo at night.",
            Self::GruvboxLight => "A warm light palette with earthy accents.",
            Self::Hermes => "Deep cobalt, navy panels, warm cream prose and gold accents.",
        }
    }
}
