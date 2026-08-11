mod io;
mod model;

pub use io::{load, load_or_default, save};
pub use model::{
    AppearanceConfig, CompatibilityConfig, Config, GeneralConfig, HotCornerConfig, IgnoreRule,
    InputConfig, PrivacyConfig, WindowRule, CURRENT_SCHEMA_VERSION,
};
