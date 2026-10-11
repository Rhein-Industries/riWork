//! Shared orchestration APIs with the desktop's theme-aware tab title.
pub use riwork_engine::orchestrators::*;

pub fn shown_tab_title(title: &str) -> String {
    shown_tab_title_with(title, |title| crate::ui_text::quiet(title).to_string())
}
