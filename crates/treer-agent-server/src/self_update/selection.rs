//! Read-only enrollment selection. Never choose installed images from PATH,
//! TREER_BIN, or current_exe (which may be the job-local activator).
//! Configurations contain secrets: decode a minimal projection and expose only
//! bounded fixed diagnostics, never serde errors, paths or credential fields.
use std::{
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
};

use anyhow::{bail, Result};
use serde::Deserialize;
use treer_platform::{
    installation_directory::InstallationDirectory, private_directory::PrivateDirectory,
};
use url::Url;

use super::{artifacts::Binary, identifier};

const CONFIG_LIMIT: u64 = 256 * 1024;
const INVENTORY_LIMIT: usize = 1024;

#[derive(Clone, PartialEq, Eq, Deserialize)]
struct ControllerRecord {
    #[serde(deserialize_with = "deserialize_proxy")]
    proxy: Url,
    workspace: String,
    server_id: String,
    root: PathBuf,
    listen: String,
    host_socket: PathBuf,
    install_hostname: String,
}

fn deserialize_proxy<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Url, D::Error> {
    let value = String::deserialize(deserializer)?;
    Url::parse(&value).map_err(|_| serde::de::Error::custom("invalid update enrollment URL"))
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
struct HostRecord {
    socket_path: PathBuf,
    controller_path: PathBuf,
    controller_config_path: PathBuf,
    root: PathBuf,
}

#[derive(Clone, PartialEq, Eq)]
struct Enrollment {
    controller: ControllerRecord,
    host: HostRecord,
}

/// This inventory is a bounded snapshot, not a lock or authorization claim.
/// Select again after acquiring installation/enrollment locks before mutation.
pub struct Inventory {
    config_root: PathBuf,
    entries: Vec<Enrollment>,
}

/// Private binding for the engine. Intentionally not Debug/Serialize: addresses
/// and filesystem paths must not accidentally enter a public receipt or log.
pub struct Selected {
    config_root: PathBuf,
    enrollment: Enrollment,
    cli_parent: PathBuf,
    shared_server_ids: Vec<String>,
    accept_shared_binaries: bool,
}

fn physical_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

fn read<T: serde::de::DeserializeOwned>(directory: &PrivateDirectory, name: &str) -> Result<T> {
    let file = directory
        .read_file(name)
        .map_err(|_| anyhow::anyhow!("unsafe_update_enrollment"))?;
    let mut bytes = Vec::new();
    file.take(CONFIG_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("update_enrollment_read_failed"))?;
    if bytes.len() as u64 > CONFIG_LIMIT {
        bail!("update_enrollment_too_large");
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid_update_enrollment"))
}

impl Inventory {
    pub fn local() -> Result<Self> {
        crate::service::self_update_inventory()
    }

    /// Paths and hostname come from the local service configuration resolver,
    /// not remote arguments. No files are created or chmodded by enumeration.
    pub fn load(config_root: &Path, hostname: &str) -> Result<Self> {
        if !physical_path(config_root)
            || hostname.is_empty()
            || hostname.len() > 255
            || hostname.chars().any(char::is_control)
        {
            bail!("invalid_update_enrollment_source");
        }
        let directory = PrivateDirectory::open(config_root)
            .map_err(|_| anyhow::anyhow!("unsafe_update_enrollment"))?;
        let entries = std::fs::read_dir(config_root)
            .map_err(|_| anyhow::anyhow!("update_enrollment_read_failed"))?;
        let mut result = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index >= INVENTORY_LIMIT {
                bail!("update_enrollment_inventory_too_large");
            }
            let entry = entry.map_err(|_| anyhow::anyhow!("update_enrollment_read_failed"))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(server_id) = name.strip_suffix("-controller.json") else {
                continue;
            };
            if !identifier(server_id, "srv_") {
                bail!("invalid_update_enrollment");
            }
            let controller: ControllerRecord = read(&directory, name)?;
            if controller.install_hostname != hostname {
                continue;
            }
            if controller.server_id != server_id || !identifier(&controller.workspace, "ws_") {
                bail!("update_enrollment_binding_mismatch");
            }
            let host: HostRecord = read(&directory, &format!("{server_id}-host.json"))?;
            if host.controller_config_path != config_root.join(name)
                || host.socket_path != controller.host_socket
                || host.root != controller.root
                || !physical_path(&host.controller_path)
                || host.controller_path.file_name().and_then(|s| s.to_str())
                    != Some(Binary::Controller.name())
                || !physical_path(&host.root)
                || !physical_path(&host.socket_path)
                || controller.listen.parse::<std::net::SocketAddr>().is_err()
                || !controller.proxy.username().is_empty()
                || controller.proxy.password().is_some()
                || controller.proxy.query().is_some()
                || controller.proxy.fragment().is_some()
                || !matches!(controller.proxy.scheme(), "http" | "https")
                || controller.proxy.host().is_none()
            {
                bail!("update_enrollment_binding_mismatch");
            }
            result.push(Enrollment { controller, host });
        }
        result.sort_by(|a, b| a.controller.server_id.cmp(&b.controller.server_id));
        Ok(Self {
            config_root: config_root.to_path_buf(),
            entries: result,
        })
    }

    pub fn select(
        &self,
        workspace: Option<&str>,
        accept_shared_binaries: bool,
    ) -> Result<Selected> {
        if workspace.is_some_and(|id| !identifier(id, "ws_")) {
            bail!("invalid_update_workspace");
        }
        let mut matching = self
            .entries
            .iter()
            .filter(|e| workspace.is_none_or(|w| w == e.controller.workspace));
        let selected = matching
            .next()
            .ok_or_else(|| anyhow::anyhow!("update_enrollment_not_found"))?;
        if matching.next().is_some() {
            bail!("update_workspace_selection_required");
        }
        let cli_parent = resolve_cli_parent(&selected.host.controller_path)?;
        let mut shared = Vec::new();
        for other in &self.entries {
            if other.controller.server_id == selected.controller.server_id {
                continue;
            }
            // Different Controller installations can still share ~/.local/bin.
            // Check both parents; do not silently treat that as independent.
            let shares = other.host.controller_path == selected.host.controller_path
                || resolve_cli_parent(&other.host.controller_path)? == cli_parent;
            if shares {
                shared.push(other.controller.server_id.clone());
            }
        }
        if !shared.is_empty() && !accept_shared_binaries {
            bail!("update_shared_binaries_ack_required");
        }
        Ok(Selected {
            config_root: self.config_root.clone(),
            enrollment: selected.clone(),
            cli_parent,
            shared_server_ids: shared,
            accept_shared_binaries,
        })
    }
}

fn resolve_cli_parent(controller: &Path) -> Result<PathBuf> {
    let parent = controller
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid_update_installation"))?;
    let directory = InstallationDirectory::open(parent)
        .map_err(|_| anyhow::anyhow!("unsafe_update_installation"))?;
    directory
        .read_file(Binary::Controller.name())
        .map_err(|_| anyhow::anyhow!("unsafe_update_installation"))?;
    let mut candidates = Vec::new();
    match directory.read_file(Binary::Cli.name()) {
        Ok(_) => candidates.push(parent.to_path_buf()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => bail!("unsafe_update_installation"),
    }
    if parent.file_name().is_some_and(|s| s == "treer") {
        if let Some(libexec) = parent
            .parent()
            .filter(|p| p.file_name().is_some_and(|s| s == "libexec"))
        {
            let local = libexec
                .parent()
                .ok_or_else(|| anyhow::anyhow!("invalid_update_installation"))?;
            let bin = local.join("bin");
            match InstallationDirectory::open(&bin) {
                Ok(dir) => match dir.read_file(Binary::Cli.name()) {
                    Ok(_) => candidates.push(bin),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => bail!("unsafe_update_installation"),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => bail!("unsafe_update_installation"),
            }
        }
    }
    if candidates.len() != 1 {
        bail!("update_cli_installation_ambiguous_or_missing");
    }
    Ok(candidates.remove(0))
}

/// Holds every participating installation-parent lock in stable physical order,
/// then the selected enrollment lock. Dropping on any error releases all locks.
pub struct SelectionLocks {
    directories: Vec<(PathBuf, InstallationDirectory, File)>,
    _enrollment: File,
    binding: Enrollment,
    config_root: PathBuf,
}

impl Selected {
    pub fn workspace(&self) -> &str {
        &self.enrollment.controller.workspace
    }
    pub fn server_id(&self) -> &str {
        &self.enrollment.controller.server_id
    }
    pub fn proxy(&self) -> &Url {
        &self.enrollment.controller.proxy
    }
    pub fn controller_path(&self) -> &Path {
        &self.enrollment.host.controller_path
    }
    pub fn cli_path(&self) -> PathBuf {
        self.cli_parent.join(Binary::Cli.name())
    }
    pub fn host_socket(&self) -> &Path {
        &self.enrollment.host.socket_path
    }
    pub fn shared_server_ids(&self) -> &[String] {
        &self.shared_server_ids
    }

    pub fn lock(&self) -> Result<SelectionLocks> {
        let controller_parent = self
            .controller_path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid_update_installation"))?;
        let mut directories = Vec::new();
        for path in [controller_parent, self.cli_parent.as_path()] {
            let directory = InstallationDirectory::open(path)?;
            let identity = directory.identity()?;
            directories.push((identity, path.to_path_buf(), directory));
        }
        directories.sort_by_key(|(identity, _, _)| *identity);
        directories.dedup_by_key(|(identity, _, _)| *identity);
        let mut held = Vec::new();
        for (_, path, directory) in directories {
            let lock = directory.lock()?;
            held.push((path, directory, lock));
        }
        let config = PrivateDirectory::open(&self.config_root)?;
        let enrollment = config.lock(&format!("{}-self-update.lock", self.server_id()))?;
        // Catch enrollment changes between inventory and acquiring its lock.
        let controller: ControllerRecord =
            read(&config, &format!("{}-controller.json", self.server_id()))?;
        let host: HostRecord = read(&config, &format!("{}-host.json", self.server_id()))?;
        if controller != self.enrollment.controller || host != self.enrollment.host {
            bail!("update_enrollment_changed");
        }
        let fresh = Inventory::load(
            &self.config_root,
            &self.enrollment.controller.install_hostname,
        )?
        .select(Some(self.workspace()), self.accept_shared_binaries)?;
        if fresh.enrollment != self.enrollment
            || fresh.cli_parent != self.cli_parent
            || fresh.shared_server_ids != self.shared_server_ids
        {
            bail!("update_enrollment_changed");
        }
        Ok(SelectionLocks {
            directories: held,
            _enrollment: enrollment,
            binding: self.enrollment.clone(),
            config_root: self.config_root.clone(),
        })
    }
}

impl SelectionLocks {
    pub fn directory(&self, selected: &Selected, binary: Binary) -> Result<&InstallationDirectory> {
        if self.binding != selected.enrollment || self.config_root != selected.config_root {
            bail!("update_installation_lock_mismatch");
        }
        let path = match binary {
            Binary::Controller => selected
                .controller_path()
                .parent()
                .ok_or_else(|| anyhow::anyhow!("invalid_update_installation"))?,
            Binary::Cli => &selected.cli_parent,
        };
        self.directories
            .iter()
            .find(|(p, _, _)| p == path)
            .map(|(_, d, _)| d)
            .ok_or_else(|| anyhow::anyhow!("update_installation_lock_mismatch"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        os::unix::fs::{symlink, DirBuilderExt},
    };
    use uuid::Uuid;

    struct Fixture {
        path: PathBuf,
        config: PrivateDirectory,
        bin: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("treer-update-selection-{}", Uuid::new_v4()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            let root = PrivateDirectory::open(&path).unwrap();
            let config = root.create_child("config").unwrap();
            root.create_child("bin").unwrap();
            let bin = path.join("bin");
            let files = InstallationDirectory::open(&bin).unwrap();
            files
                .create_file("treer-agent-server")
                .unwrap()
                .write_all(b"controller")
                .unwrap();
            files
                .create_file("treer")
                .unwrap()
                .write_all(b"cli")
                .unwrap();
            Self { path, config, bin }
        }
        fn enrollment(&self, server: &str, workspace: &str, controller: &Path) {
            for (name, json) in [
                (
                    format!("{server}-controller.json"),
                    serde_json::json!({
                        "proxy": "https://example.invalid/", "workspace": workspace, "server_id": server,
                        "machine_token": "synthetic-secret-not-to-display", "operator_credential": "synthetic-operator",
                        "root": self.path.join("workloads"), "listen": "127.0.0.1:1234",
                        "host_socket": self.path.join(format!("{server}.sock")), "install_hostname": "fixture-host"
                    }),
                ),
                (
                    format!("{server}-host.json"),
                    serde_json::json!({
                        "socket_path": self.path.join(format!("{server}.sock")), "controller_path": controller,
                        "controller_config_path": self.path.join(format!("config/{server}-controller.json")),
                        "root": self.path.join("workloads")
                    }),
                ),
            ] {
                self.config
                    .create_file(&name)
                    .unwrap()
                    .write_all(&serde_json::to_vec(&json).unwrap())
                    .unwrap();
            }
        }
        fn inventory(&self) -> Inventory {
            Inventory::load(&self.path.join("config"), "fixture-host").unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.path).unwrap();
        }
    }

    #[test]
    fn selection_is_explicit_and_shared_installations_require_separate_acknowledgment() {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        f.enrollment("srv_b", "ws_b", &f.bin.join("treer-agent-server"));
        let inventory = f.inventory();
        assert!(inventory.select(None, true).is_err());
        assert!(inventory.select(Some("ws_a"), false).is_err());
        assert!(inventory.select(Some("ws_missing"), true).is_err());
        let a = inventory.select(Some("ws_a"), true).unwrap();
        assert_eq!(a.server_id(), "srv_a");
        assert_eq!(a.workspace(), "ws_a");
        assert_eq!(a.cli_path(), f.bin.join("treer"));
        assert_eq!(a.shared_server_ids(), &["srv_b"]);
        let b = inventory.select(Some("ws_b"), true).unwrap();
        let held = a.lock().unwrap();
        assert!(b.lock().is_err());
        assert!(held.directory(&b, Binary::Controller).is_err());
        assert!(held.directory(&a, Binary::Controller).is_ok());
        assert!(held.directory(&a, Binary::Cli).is_ok());
        drop(held);
        assert!(b.lock().is_ok());
    }

    #[test]
    fn different_controller_parents_still_lock_the_shared_cli_parent() {
        let f = Fixture::new();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(f.path.join("libexec"))
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(f.path.join("libexec/treer"))
            .unwrap();
        let controller = f.path.join("libexec/treer/treer-agent-server");
        InstallationDirectory::open(controller.parent().unwrap())
            .unwrap()
            .create_file("treer-agent-server")
            .unwrap();
        f.enrollment("srv_a", "ws_a", &controller);
        f.enrollment("srv_b", "ws_b", &f.bin.join("treer-agent-server"));
        let inventory = f.inventory();
        assert!(inventory.select(Some("ws_a"), false).is_err());
        let a = inventory.select(Some("ws_a"), true).unwrap();
        let b = inventory.select(Some("ws_b"), true).unwrap();
        assert_eq!(a.cli_path(), b.cli_path());
        let held = a.lock().unwrap();
        assert!(b.lock().is_err());
        assert!(held.directory(&a, Binary::Cli).is_ok());
        drop(held);
        assert!(b.lock().is_ok());
    }

    #[test]
    fn ambiguous_cli_layout_and_symlinked_images_fail_closed() {
        let f = Fixture::new();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(f.path.join("libexec"))
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(f.path.join("libexec/treer"))
            .unwrap();
        let parent = f.path.join("libexec/treer");
        let dir = InstallationDirectory::open(&parent).unwrap();
        dir.create_file("treer-agent-server").unwrap();
        dir.create_file("treer").unwrap();
        f.enrollment("srv_a", "ws_a", &parent.join("treer-agent-server"));
        assert!(f.inventory().select(None, false).is_err());
        dir.remove_file("treer").unwrap();
        assert!(f.inventory().select(None, false).is_ok());
        dir.remove_file("treer-agent-server").unwrap();
        symlink(
            f.bin.join("treer-agent-server"),
            parent.join("treer-agent-server"),
        )
        .unwrap();
        assert!(f.inventory().select(None, false).is_err());
    }

    #[test]
    fn changed_or_cross_bound_enrollment_is_rejected_without_secret_diagnostics() {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        let selected = f.inventory().select(None, false).unwrap();
        let path = f.path.join("config/srv_a-host.json");
        let mut host: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        host["root"] = serde_json::json!(f.path.join("other-root"));
        std::fs::write(&path, serde_json::to_vec(&host).unwrap()).unwrap();
        let error = selected.lock().err().unwrap().to_string();
        assert_eq!(error, "update_enrollment_changed");
        assert!(!error.contains("synthetic"));
        assert!(!error.contains(f.path.to_str().unwrap()));
        assert!(Inventory::load(&f.path.join("config"), "fixture-host").is_err());
        // A failed revalidation released the installation lock.
        assert!(InstallationDirectory::open(&f.bin).unwrap().lock().is_ok());
    }

    #[test]
    fn private_bounded_records_reject_symlinks_oversize_and_wrong_filename_ids() {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        std::fs::rename(
            f.path.join("config/srv_a-controller.json"),
            f.path.join("config/srv_other-controller.json"),
        )
        .unwrap();
        assert!(Inventory::load(&f.path.join("config"), "fixture-host").is_err());
        std::fs::rename(
            f.path.join("config/srv_other-controller.json"),
            f.path.join("config/srv_a-controller.json"),
        )
        .unwrap();
        let path = f.path.join("config/srv_a-host.json");
        std::fs::rename(&path, f.path.join("config/host-saved")).unwrap();
        symlink("host-saved", &path).unwrap();
        assert!(Inventory::load(&f.path.join("config"), "fixture-host").is_err());
        std::fs::remove_file(&path).unwrap();
        let file = f.config.create_file("srv_a-host.json").unwrap();
        file.set_len(CONFIG_LIMIT + 1).unwrap();
        assert_eq!(
            Inventory::load(&f.path.join("config"), "fixture-host")
                .err()
                .unwrap()
                .to_string(),
            "update_enrollment_too_large"
        );
    }

    #[test]
    fn enrollment_added_after_selection_cannot_bypass_shared_binary_ack() {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        let selected = f.inventory().select(None, false).unwrap();
        f.enrollment("srv_b", "ws_b", &f.bin.join("treer-agent-server"));
        assert_eq!(
            selected.lock().err().unwrap().to_string(),
            "update_shared_binaries_ack_required"
        );
        assert!(f
            .inventory()
            .select(Some("ws_a"), true)
            .unwrap()
            .lock()
            .is_ok());
    }
}
