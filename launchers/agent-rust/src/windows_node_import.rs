//! Fresh Windows entry only. Legacy Node migration requires a separately reviewed importer.
use anyhow::{bail, Result};
use std::path::Path;

fn fresh(root: &Path, cwd: &Path, agent: &str) -> Result<()> {
    let store = crate::store::Store::open(root, cwd)?;
    store.recheck()?;
    if root.join(".node-backup-restored").try_exists()? {
        bail!("node_backup_restored_requires_node_startup");
    }
    if !agent.is_empty() {
        let source = cwd.join(".treer").join("agents").join(agent);
        // Any legacy source, including a dangling reparse point, refuses. Never
        // silently replace imported state or read provider-native history.
        match std::fs::symlink_metadata(source) {
            Ok(_) => bail!("windows_node_import_unsupported"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
pub fn prepare(
    root: &Path,
    cwd: &Path,
    agent: &str,
    _session: Option<&str>,
    importing: bool,
) -> Result<()> {
    if importing {
        bail!("windows_node_import_unsupported");
    }
    fresh(root, cwd, agent)
}
pub fn prepare_claude(
    root: &Path,
    cwd: &Path,
    agent: &str,
    _session: Option<&str>,
    seed: Option<&Path>,
) -> Result<()> {
    if seed.is_some() {
        bail!("windows_node_import_unsupported");
    }
    fresh(root, cwd, agent)
}
pub fn restore(_root: &Path, _cwd: &Path, _agent: &str) -> Result<()> {
    bail!("windows_node_restore_unsupported")
}
