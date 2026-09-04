pub mod acp;
pub mod ais;
pub mod cancel;
pub mod files;
pub mod import_id;
pub mod journal;
pub mod local_sessions;
pub mod transcript;
pub mod types;
pub mod ui;

use anyhow::{bail, Result};
use std::path::Path;

pub use ais::{serve, AisConfig, AisServer, HarnessSpec};
pub use import_id::{bind_import_target, parse_session_ref, scoped_session_id};
pub use journal::Journal;
pub use local_sessions::{find_local_session, list_local_sessions, LocalSessionHomes};
pub use types::{BoundSession, SessionCandidate, AIS_CAPABILITIES};
pub use ui::{
    discover_installed_dist, install as install_host_ui, show as show_host_ui, InstallOptions,
    UiStatus, DEFAULT_UI_GIT, TREER_EMBED_UI_QUERY,
};

/// Bind a local harness session to this Agent's journal.
///
/// The ACP session is loaded later when `treer-acp` starts with the same
/// state dir. Imported turns are seeded into the transcript so history
/// survives before the harness is resumed.
pub fn bind_local_session(
    journal: &Journal,
    homes: &LocalSessionHomes,
    selected_harness: &str,
    session_ref: &str,
) -> Result<BoundSession> {
    let parsed = parse_session_ref(session_ref);
    if parsed.raw_id.is_empty() {
        bail!("session id is required");
    }
    let harness = bind_import_target(selected_harness, parsed.agent_id.as_deref());
    let Some(candidate) = find_local_session(homes, &harness, &parsed.raw_id) else {
        bail!("no local {harness} session matches {session_ref}");
    };
    if candidate.cwd.is_empty() {
        bail!("imported session is missing a working directory");
    }
    journal.seed_imported_turns(&candidate.turns)?;
    journal.bind_session(&harness, &candidate.session_id, Path::new(&candidate.cwd))
}

pub fn default_state_dir(cwd: &Path, agent_id: &str) -> std::path::PathBuf {
    cwd.join(".treer").join("agents").join(agent_id)
}
