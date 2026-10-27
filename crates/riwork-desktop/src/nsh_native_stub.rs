//! Temporary P1a seam. Replace with riwork_nsh_native::registry() when
//! crates/riwork-nsh-native lands; never substitute a JS stand-in module.
use nsh_core::element::Registry;

pub fn registry() -> Registry {
    Registry::ui_set("riwork/native")
}
