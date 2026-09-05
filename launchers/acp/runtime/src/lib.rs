pub mod acp;
pub mod ais;
pub mod cancel;
pub mod files;
pub mod import_id;
pub mod journal;
pub mod tower;
pub mod transcript;
pub mod types;
pub mod usage;

use std::path::Path;

pub use ais::{serve, AisConfig, AisServer, HarnessSpec};
pub use journal::Journal;
pub use tower::TowerConfig;
pub use types::{BoundSession, AIS_CAPABILITIES};

pub fn default_state_dir(cwd: &Path, agent_id: &str) -> std::path::PathBuf {
    cwd.join(".treer").join("agents").join(agent_id)
}
