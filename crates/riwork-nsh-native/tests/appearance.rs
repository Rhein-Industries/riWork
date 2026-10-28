use riwork_core::{settings::Settings, theme::Palette};
use riwork_nsh_native::appearance::AppearanceDto;

#[test]
fn every_native_token_is_exact() {
    // Golden values retained from the GPUI palette and ANSI tables, including
    // every derived token; changing the resolver cannot regenerate this fixture.
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/appearance-presets.json")).unwrap();
    for (key, dark) in [("native", false), ("native_dark", true)] {
        let settings = Settings::default();
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
fn the_default_is_native_follows_the_system_and_writes_no_appearance_file() {
    let fixture = tempfile::tempdir().unwrap();
    let existing = fixture.path().join("appearance.json");
    std::fs::write(&existing, "pre-existing companion appearance").unwrap();
    let before = std::fs::metadata(&existing).unwrap().modified().unwrap();
    let settings = Settings::default();
    let light = AppearanceDto::from_settings(&settings, false, None);
    let dark = AppearanceDto::from_settings(&settings, true, None);
    assert_eq!(light.selected, "native");
    assert_eq!(light.palette.bg, format!("#{:06x}", Palette::NATIVE_LIGHT.bg));
    assert_eq!(dark.palette.bg, format!("#{:06x}", Palette::NATIVE_DARK.bg));
    assert!(light.error.is_none());
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "pre-existing companion appearance"
    );
    assert_eq!(
        std::fs::metadata(&existing).unwrap().modified().unwrap(),
        before
    );
    let absent = fixture.path().join("no-publisher/appearance.json");
    let retained = AppearanceDto::from_settings(
        &settings,
        false,
        Some("io: retained settings diagnostic".into()),
    );
    assert_eq!(
        retained.error.as_deref(),
        Some("io: retained settings diagnostic")
    );
    assert!(!absent.exists());
}

#[test]
fn override_is_an_explicit_field_that_follows_native_terminal_colors() {
    let mut settings = Settings::default();
    assert!(
        AppearanceDto::from_settings(&settings, false, None)
            .terminal_override
            .is_some()
    );
    settings.native_terminal_colors = false;
    assert!(
        AppearanceDto::from_settings(&settings, false, None)
            .terminal_override
            .is_none()
    );
}
