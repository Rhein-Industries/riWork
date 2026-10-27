use riwork_core::{
    settings::Settings,
    theme::{Palette, ThemeChoice},
};
use riwork_nsh_native::appearance::AppearanceDto;

#[test]
fn every_preset_token_is_exact() {
    // Golden values retained from the GPUI palette and ANSI tables, including
    // every derived token; changing the resolver cannot regenerate this fixture.
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/appearance-presets.json")).unwrap();
    for (key, theme, dark) in [
        ("ghostty", ThemeChoice::Ghostty, false),
        ("native", ThemeChoice::Native, false),
        ("native_dark", ThemeChoice::Native, true),
        ("ri_work", ThemeChoice::RiWork, false),
        ("catppuccin", ThemeChoice::Catppuccin, false),
        ("tokyo_night", ThemeChoice::TokyoNight, false),
        ("gruvbox_light", ThemeChoice::GruvboxLight, false),
        ("hermes", ThemeChoice::Hermes, false),
    ] {
        let settings = Settings {
            theme,
            ..Settings::default()
        };
        let dto = AppearanceDto::from_settings(&settings, dark, None);
        let mut actual = serde_json::to_value(&dto).unwrap();
        // Terminal-phase fields are checked against the same core override
        // below; the GPUI golden stays byte-for-byte unchanged.
        let fields = actual.as_object_mut().unwrap();
        for key in ["terminalLight", "terminalDark", "tabCloseBehavior"] {
            fields.remove(key).unwrap();
        }
        assert_eq!(actual, golden[key], "{key}");
        let shown = if dark {
            &dto.terminal_dark
        } else {
            &dto.terminal_light
        };
        assert_eq!(shown, &dto.terminal_override, "{key}");
        assert_eq!(dto.tab_close_behavior, "ask");
    }
}

#[test]
fn follow_ghostty_without_a_parser_uses_the_riwork_palette_and_keeps_the_error() {
    let fixture = tempfile::tempdir().unwrap();
    let existing = fixture.path().join("appearance.json");
    std::fs::write(&existing, "pre-existing companion appearance").unwrap();
    let before = std::fs::metadata(&existing).unwrap().modified().unwrap();
    let settings = Settings::default();
    let a = AppearanceDto::from_settings(&settings, false, None);
    let b = AppearanceDto::from_settings(&settings, true, None);
    assert_eq!(a, b);
    assert_eq!(a.palette.bg, format!("#{:06x}", Palette::RIWORK.bg));
    assert_eq!(a.palette.text, "#d3e1e6");
    assert!(
        a.error
            .as_deref()
            .unwrap()
            .contains("Following Ghostty is unavailable")
    );
    assert!(a.terminal_override.is_none());
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "pre-existing companion appearance"
    );
    assert_eq!(
        std::fs::metadata(&existing).unwrap().modified().unwrap(),
        before
    );
    let absent = fixture.path().join("no-publisher/appearance.json");
    let _ = AppearanceDto::from_settings(
        &settings,
        false,
        Some("io: retained settings diagnostic".into()),
    );
    assert!(!absent.exists());
}

#[test]
fn override_is_an_explicit_field_and_keeps_preset_and_native_preferences() {
    let mut settings = Settings::default();
    settings.use_riwork_colors = true;
    assert!(
        AppearanceDto::from_settings(&settings, false, None)
            .terminal_override
            .is_some()
    );
    settings.theme = ThemeChoice::Native;
    settings.native_terminal_colors = false;
    assert!(
        AppearanceDto::from_settings(&settings, false, None)
            .terminal_override
            .is_none()
    );
    assert!(settings.use_riwork_colors);
    settings.theme = ThemeChoice::TokyoNight;
    assert!(
        AppearanceDto::from_settings(&settings, false, None)
            .terminal_override
            .is_some()
    );
}
