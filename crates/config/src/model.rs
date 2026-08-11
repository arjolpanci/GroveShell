use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use groveshell_common::{Error, Result};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub input: InputConfig,
    #[serde(default)]
    pub hot_corners: BTreeMap<String, HotCornerConfig>,
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub privacy: PrivacyConfig,
    #[serde(default)]
    pub compatibility: CompatibilityConfig,
    #[serde(default)]
    pub window_rules: Vec<WindowRule>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            general: GeneralConfig::default(),
            input: InputConfig::default(),
            hot_corners: BTreeMap::new(),
            appearance: AppearanceConfig::default(),
            privacy: PrivacyConfig::default(),
            compatibility: CompatibilityConfig::default(),
            window_rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub start_with_windows: bool,
    pub workspace_backend: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            workspace_backend: "managed".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub move_modifier: String,
    pub move_button: String,
    pub resize_button: String,
    pub overview_modifier: String,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            move_modifier: "Alt".to_string(),
            move_button: "Left".to_string(),
            resize_button: "Right".to_string(),
            overview_modifier: "Super".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HotCornerConfig {
    pub action: String,
    pub delay_ms: u32,
    pub disable_in_fullscreen: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    pub top_bar_height: u32,
    pub dock_mode: String,
    pub animation_scale: f32,
    pub dock_icon_size: u32,
    pub dock_alignment: String,
    pub top_bar_blur: bool,
    pub overview_blur: bool,
    pub reduced_motion: bool,
    /// When set, the shell surfaces (top bar, dock, overview) render with a
    /// high-contrast palette — pure black/white with a bright accent —
    /// instead of the default dark-grey theme. Independent of Windows' own
    /// high-contrast mode; this only restyles GroveShell's own windows,
    /// which is the most it can honor without a system theme integration.
    pub high_contrast: bool,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            top_bar_height: 32,
            dock_mode: "overview".to_string(),
            animation_scale: 1.0,
            dock_icon_size: 44,
            dock_alignment: "center".to_string(),
            top_bar_blur: false,
            overview_blur: false,
            reduced_motion: false,
            high_contrast: false,
        }
    }
}

/// Privacy controls (PROJECT_PLAN §12). Everything here defaults to the
/// most private setting so a fresh install never emits data the user
/// didn't opt into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig {
    /// Replace window titles with `"<redacted>"` anywhere they'd otherwise
    /// be written to disk — currently the `groveshell-cli diagnostics`
    /// bundle. Titles can contain document names, URLs, and chat contents,
    /// so a diagnostics bundle shared with a bug report shouldn't leak them
    /// unless the user allows it.
    pub redact_window_titles: bool,
    /// Master opt-in for any future usage/telemetry reporting. There is no
    /// telemetry transport today; this exists so the default is recorded as
    /// "off" now and honored the moment one is added.
    pub telemetry: bool,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            redact_window_titles: true,
            telemetry: false,
        }
    }
}

/// Compatibility policy (PROJECT_PLAN §16 "compatibility rules and ignore
/// list"). Windows matching any ignore rule are never managed, hidden,
/// moved, or shown in the overview — the escape hatch for apps that resist
/// being repositioned or that must stay on every workspace (launchers,
/// on-screen keyboards, some games' overlays).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CompatibilityConfig {
    pub ignore: Vec<IgnoreRule>,
}

/// One ignore rule. All present fields must match (logical AND) for the
/// rule to apply; an empty rule (no fields) matches nothing so a stray
/// `[[compatibility.ignore]]` with no keys can't accidentally hide every
/// window. Matching is case-insensitive; `title` is a substring match,
/// `exe` and `class` are exact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct IgnoreRule {
    /// Executable file name without directory, e.g. `"Widgets.exe"`.
    pub exe: Option<String>,
    /// Win32 window class name, e.g. `"Shell_TrayWnd"`.
    pub class: Option<String>,
    /// Case-insensitive substring of the window title.
    pub title: Option<String>,
}

impl IgnoreRule {
    /// Whether this rule matches a window with the given (best-effort)
    /// executable name, class, and title. A rule with no fields set never
    /// matches. Kept pure and side-effect free so it can be unit tested and
    /// reused by `groveshell-window-model` without a Windows dependency.
    pub fn matches(&self, exe: Option<&str>, class: &str, title: &str) -> bool {
        if self.exe.is_none() && self.class.is_none() && self.title.is_none() {
            return false;
        }
        if let Some(want) = &self.exe {
            match exe {
                Some(have) if have.eq_ignore_ascii_case(want) => {}
                _ => return false,
            }
        }
        if let Some(want) = &self.class {
            if !class.eq_ignore_ascii_case(want) {
                return false;
            }
        }
        if let Some(want) = &self.title {
            if !title.to_ascii_lowercase().contains(&want.to_ascii_lowercase()) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WindowRule {
    pub match_exe: Option<String>,
    pub workspace: Option<String>,
    pub decoration: Option<String>,
}

impl Config {
    /// Rejects configs with an unsupported schema version or nonsensical
    /// values. Called by both `load` (before returning) and `save` (before
    /// writing), so a caller can never persist or accept a config that
    /// fails these checks.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(Error::InvalidConfig(format!(
                "unsupported schema_version {} (expected {})",
                self.schema_version, CURRENT_SCHEMA_VERSION
            )));
        }
        for (name, corner) in &self.hot_corners {
            if !matches!(corner.action.as_str(), "" | "activities" | "none") {
                return Err(Error::InvalidConfig(format!(
                    "hot_corners.{name}: unknown action '{}'",
                    corner.action
                )));
            }
        }
        if self.appearance.animation_scale < 0.0 {
            return Err(Error::InvalidConfig(
                "appearance.animation_scale must be >= 0".to_string(),
            ));
        }
        if !matches!(self.appearance.dock_alignment.as_str(), "left" | "center" | "right") {
            return Err(Error::InvalidConfig(format!(
                "appearance.dock_alignment: unknown value '{}'",
                self.appearance.dock_alignment
            )));
        }
        if !matches!(self.input.overview_modifier.as_str(), "Super" | "Alt" | "CtrlAlt") {
            return Err(Error::InvalidConfig(format!(
                "input.overview_modifier: unknown value '{}'",
                self.input.overview_modifier
            )));
        }
        for (i, rule) in self.compatibility.ignore.iter().enumerate() {
            if rule.exe.is_none() && rule.class.is_none() && rule.title.is_none() {
                return Err(Error::InvalidConfig(format!(
                    "compatibility.ignore[{i}]: rule must set at least one of exe, class, or title"
                )));
            }
        }
        Ok(())
    }
}
