use std::io::Write;
use groveshell_config::{load, load_or_default, save, Config, IgnoreRule};

const EXAMPLE_TOML: &str = r#"
schema_version = 1

[general]
start_with_windows = false
workspace_backend = "managed"

[input]
move_modifier = "Alt"
move_button = "Left"
resize_button = "Right"

[hot_corners.top_left]
action = "activities"
delay_ms = 150
disable_in_fullscreen = true

[appearance]
top_bar_height = 32
dock_mode = "overview"
animation_scale = 1.0

[[window_rules]]
match_exe = "devenv.exe"
workspace = "Development"
decoration = "native"
"#;

fn write_temp_toml(contents: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::Builder::new()
        .suffix(".toml")
        .tempfile()
        .expect("create temp file");
    file.write_all(contents.as_bytes())
        .expect("write temp file");
    file.flush().expect("flush temp file");
    file
}

#[test]
fn default_config_has_current_schema_version_and_sane_defaults() {
    let config = Config::default();
    assert_eq!(
        config.schema_version,
        groveshell_config::CURRENT_SCHEMA_VERSION
    );
    assert_eq!(config.general.workspace_backend, "managed");
    assert_eq!(config.input.move_modifier, "Alt");
    assert_eq!(config.appearance.top_bar_height, 32);
    assert!(config.hot_corners.is_empty());
    assert!(config.window_rules.is_empty());
}

#[test]
fn load_missing_file_falls_back_to_default_via_load_or_default() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let missing_path = dir.path().join("does-not-exist.toml");

    let config = load_or_default(&missing_path);

    assert_eq!(config, Config::default());
}

#[test]
fn load_valid_toml_matches_the_documented_example() {
    let file = write_temp_toml(EXAMPLE_TOML);

    let config = load(file.path()).expect("valid example config should load");

    assert_eq!(config.schema_version, 1);
    assert!(!config.general.start_with_windows);
    assert_eq!(config.general.workspace_backend, "managed");
    assert_eq!(config.input.move_modifier, "Alt");
    assert_eq!(config.input.move_button, "Left");
    assert_eq!(config.input.resize_button, "Right");
    let top_left = config
        .hot_corners
        .get("top_left")
        .expect("top_left hot corner should be present");
    assert_eq!(top_left.action, "activities");
    assert_eq!(top_left.delay_ms, 150);
    assert!(top_left.disable_in_fullscreen);
    assert_eq!(config.appearance.top_bar_height, 32);
    assert_eq!(config.appearance.dock_mode, "overview");
    assert_eq!(config.window_rules.len(), 1);
    assert_eq!(
        config.window_rules[0].match_exe.as_deref(),
        Some("devenv.exe")
    );
}

#[test]
fn load_rejects_unsupported_schema_version() {
    let file = write_temp_toml("schema_version = 99\n");

    let result = load(file.path());

    assert!(result.is_err(), "schema_version 99 must be rejected");
}

#[test]
fn load_or_default_falls_back_when_file_is_invalid() {
    let file = write_temp_toml("this is not valid toml [[[");

    let config = load_or_default(file.path());

    assert_eq!(config, Config::default());
}

#[test]
fn save_then_load_round_trips_exactly() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("config.toml");
    let mut config = Config::default();
    config.general.start_with_windows = true;
    config.appearance.animation_scale = 0.5;

    save(&path, &config).expect("save should succeed");
    let loaded = load(&path).expect("load should succeed");

    assert_eq!(loaded, config);
}

#[test]
fn save_creates_a_backup_of_the_previous_file() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("config.toml");
    let backup_path = path.with_extension("toml.bak");

    save(&path, &Config::default()).expect("first save should succeed");
    assert!(
        !backup_path.exists(),
        "no backup expected before a second save"
    );

    let mut second = Config::default();
    second.appearance.top_bar_height = 40;
    save(&path, &second).expect("second save should succeed");

    assert!(
        backup_path.exists(),
        "backup should exist after the second save"
    );
    let backup_config = load(&backup_path).expect("backup should be valid config");
    assert_eq!(backup_config, Config::default());
}

#[test]
fn default_config_has_sane_defaults_for_new_appearance_and_input_fields() {
    let config = Config::default();
    assert_eq!(config.appearance.dock_icon_size, 44);
    assert_eq!(config.appearance.dock_alignment, "center");
    assert!(!config.appearance.top_bar_blur);
    assert!(!config.appearance.overview_blur);
    assert!(!config.appearance.reduced_motion);
    assert_eq!(config.input.overview_modifier, "Super");
}

#[test]
fn load_rejects_unknown_dock_alignment() {
    let file = write_temp_toml(
        "schema_version = 1\n[appearance]\ndock_alignment = \"top\"\n",
    );
    assert!(load(file.path()).is_err());
}

#[test]
fn load_rejects_unknown_overview_modifier() {
    let file = write_temp_toml(
        "schema_version = 1\n[input]\noverview_modifier = \"Ctrl\"\n",
    );
    assert!(load(file.path()).is_err());
}

#[test]
fn default_config_has_private_defaults_and_no_ignore_rules() {
    let config = Config::default();
    assert!(
        config.privacy.redact_window_titles,
        "window titles must be redacted by default"
    );
    assert!(!config.privacy.telemetry, "telemetry must be off by default");
    assert!(!config.appearance.high_contrast);
    assert!(config.compatibility.ignore.is_empty());
}

#[test]
fn load_parses_privacy_and_compatibility_sections() {
    let toml = r#"
schema_version = 1

[privacy]
redact_window_titles = false
telemetry = true

[appearance]
high_contrast = true

[[compatibility.ignore]]
exe = "Widgets.exe"

[[compatibility.ignore]]
class = "Shell_TrayWnd"
title = "Taskbar"
"#;
    let file = write_temp_toml(toml);
    let config = load(file.path()).expect("valid config should load");

    assert!(!config.privacy.redact_window_titles);
    assert!(config.privacy.telemetry);
    assert!(config.appearance.high_contrast);
    assert_eq!(config.compatibility.ignore.len(), 2);
    assert_eq!(config.compatibility.ignore[0].exe.as_deref(), Some("Widgets.exe"));
    assert_eq!(config.compatibility.ignore[1].class.as_deref(), Some("Shell_TrayWnd"));
    assert_eq!(config.compatibility.ignore[1].title.as_deref(), Some("Taskbar"));
}

#[test]
fn load_rejects_an_empty_ignore_rule() {
    // A `[[compatibility.ignore]]` with no keys would otherwise match
    // nothing, but it's a config mistake worth catching loudly rather than
    // silently doing nothing.
    let toml = "schema_version = 1\n[[compatibility.ignore]]\n";
    let file = write_temp_toml(toml);
    assert!(load(file.path()).is_err());
}

#[test]
fn ignore_rule_matching_honors_and_of_present_fields() {
    // exe only, case-insensitive.
    let exe_rule = IgnoreRule { exe: Some("Widgets.exe".into()), ..Default::default() };
    assert!(exe_rule.matches(Some("widgets.exe"), "AnyClass", "any title"));
    assert!(!exe_rule.matches(Some("notepad.exe"), "AnyClass", "any title"));
    assert!(!exe_rule.matches(None, "AnyClass", "any title"));

    // exe AND title: both must hold.
    let combo = IgnoreRule {
        exe: Some("game.exe".into()),
        title: Some("overlay".into()),
        ..Default::default()
    };
    assert!(combo.matches(Some("game.exe"), "UnrealWindow", "Steam Overlay"));
    assert!(!combo.matches(Some("game.exe"), "UnrealWindow", "Main Window"));

    // class exact, title substring.
    let class_rule = IgnoreRule { class: Some("Shell_TrayWnd".into()), ..Default::default() };
    assert!(class_rule.matches(None, "shell_traywnd", ""));
    assert!(!class_rule.matches(None, "Shell_TrayWnd_Extra", ""));

    // Empty rule never matches.
    let empty = IgnoreRule::default();
    assert!(!empty.matches(Some("anything.exe"), "AnyClass", "any title"));
}

#[test]
fn privacy_and_compatibility_round_trip_through_save_and_load() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("config.toml");
    let mut config = Config::default();
    config.privacy.redact_window_titles = false;
    config.appearance.high_contrast = true;
    config.compatibility.ignore.push(IgnoreRule {
        exe: Some("obs64.exe".into()),
        ..Default::default()
    });

    save(&path, &config).expect("save should succeed");
    let loaded = load(&path).expect("load should succeed");
    assert_eq!(loaded, config);
}

#[test]
fn load_accepts_every_valid_dock_alignment_and_overview_modifier() {
    for alignment in ["left", "center", "right"] {
        let toml = format!("schema_version = 1\n[appearance]\ndock_alignment = \"{alignment}\"\n");
        let file = write_temp_toml(&toml);
        assert!(load(file.path()).is_ok(), "{alignment} should be accepted");
    }
    for modifier in ["Super", "Alt", "CtrlAlt"] {
        let toml = format!("schema_version = 1\n[input]\noverview_modifier = \"{modifier}\"\n");
        let file = write_temp_toml(&toml);
        assert!(load(file.path()).is_ok(), "{modifier} should be accepted");
    }
}
