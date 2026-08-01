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
        }
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
        Ok(())
    }
}
