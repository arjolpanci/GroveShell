mod io;
mod model;

pub use io::{load, load_or_default, save};
pub use model::{
    AppearanceConfig, Config, GeneralConfig, HotCornerConfig, InputConfig, WindowRule,
    CURRENT_SCHEMA_VERSION,
};
