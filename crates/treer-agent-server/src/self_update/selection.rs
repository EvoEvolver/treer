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

    /// Select resources, without taking locks or granting mutation authority.
    /// Submitting callers must obtain explicit shared-binary consent before
    /// passing true. A private worker may use true only to rediscover resources
    /// bound by its Prepared receipt: it still needs the authenticated parent's
    /// SAME lock descriptions and sealed OwnedActivation before activation.
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
    pub(super) fn health_address(&self) -> Result<std::net::SocketAddr> {
        let mut address: std::net::SocketAddr = self
            .enrollment
            .controller
            .listen
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid_update_health_address"))?;
        // Connect to the local family-specific loopback for wildcard listeners;
        // never use an ambient TREER_AGENT_SERVER_URL or resolve a DNS name.
        if address.ip().is_unspecified() {
            address.set_ip(if address.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        if address.port() == 0 {
            bail!("invalid_update_health_address");
        }
        Ok(address)
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
    /// Caller must authenticate the job-local activator on this private socket
    /// before sending. Keep self alive until ownership is durably acknowledged.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(super) fn send(&self, stream: &std::os::unix::net::UnixStream) -> Result<()> {
        let mut files: Vec<_> = self.directories.iter().map(|(_, _, lock)| lock).collect();
        files.push(&self._enrollment);
        treer_platform::lock_transfer::send(stream, &files)
            .map_err(|_| anyhow::anyhow!("update_lock_transfer_failed"))
    }

    /// Receive before starting activator threads (Darwin CLOEXEC requirement).
    /// No caller-provided destination is accepted: all descriptor inodes must
    /// match the selected checked installation/enrollment lock names.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(super) fn receive(
        selected: &Selected,
        stream: &std::os::unix::net::UnixStream,
    ) -> Result<Self> {
        let parent = selected
            .controller_path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid_update_installation"))?;
        let mut directories = Vec::new();
        for path in [parent, selected.cli_parent.as_path()] {
            let directory = InstallationDirectory::open(path)?;
            directories.push((directory.identity()?, path.to_path_buf(), directory));
        }
        directories.sort_by_key(|(identity, _, _)| *identity);
        directories.dedup_by_key(|(identity, _, _)| *identity);
        let files = treer_platform::lock_transfer::receive(stream, directories.len() + 1)
            .map_err(|_| anyhow::anyhow!("update_lock_transfer_failed"))?;
        let mut files = files.into_iter();
        let mut held = Vec::new();
        for (_, path, directory) in directories {
            let expected = directory.existing_lock()?;
            let file = treer_platform::lock_transfer::adopt(files.next().unwrap(), &expected)
                .map_err(|_| anyhow::anyhow!("update_lock_transfer_mismatch"))?;
            held.push((path, directory, file));
        }
        let config = PrivateDirectory::open(&selected.config_root)?;
        let expected = config.read_file(&format!("{}-self-update.lock", selected.server_id()))?;
        let enrollment = treer_platform::lock_transfer::adopt(files.next().unwrap(), &expected)
            .map_err(|_| anyhow::anyhow!("update_lock_transfer_mismatch"))?;
        let fresh = Inventory::load(
            &selected.config_root,
            &selected.enrollment.controller.install_hostname,
        )?
        .select(Some(selected.workspace()), selected.accept_shared_binaries)?;
        if fresh.enrollment != selected.enrollment
            || fresh.cli_parent != selected.cli_parent
            || fresh.shared_server_ids != selected.shared_server_ids
        {
            bail!("update_enrollment_changed");
        }
        Ok(Self {
            directories: held,
            _enrollment: enrollment,
            binding: selected.enrollment.clone(),
            config_root: selected.config_root.clone(),
        })
    }

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
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod survival_tests;

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::{
        io::Write,
        os::unix::fs::{symlink, DirBuilderExt},
    };
    use uuid::Uuid;

    pub(in crate::self_update) struct Fixture {
        pub(in crate::self_update) path: PathBuf,
        config: PrivateDirectory,
        pub(in crate::self_update) bin: PathBuf,
    }
    impl Fixture {
        pub(in crate::self_update) fn new() -> Self {
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
        pub(in crate::self_update) fn enrollment(
            &self,
            server: &str,
            workspace: &str,
            controller: &Path,
        ) {
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
        pub(in crate::self_update) fn inventory(&self) -> Inventory {
            Inventory::load(&self.path.join("config"), "fixture-host").unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.path).unwrap();
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn durable_ownership_handoff_runs_in_an_isolated_process() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "self_update::selection::tests::ownership_case",
                "--ignored",
                "--nocapture",
            ])
            .env_clear()
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[ignore = "isolated subprocess fixture, invoked by the outer ownership test"]
    fn ownership_case() {
        use super::super::{
            handoff::{AuthenticatedPeer, Endpoint},
            images::digest_file,
            Journal,
        };
        use std::{
            os::unix::fs::PermissionsExt,
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        use treer_platform::process_image::ProcessImage;

        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        f.enrollment("srv_b", "ws_b", &f.bin.join("treer-agent-server"));
        let selected = f.inventory().select(Some("ws_a"), true).unwrap();
        let other = f.inventory().select(Some("ws_b"), true).unwrap();
        let short = std::fs::canonicalize("/tmp")
            .unwrap()
            .join(format!("to-{}", Uuid::new_v4().simple()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&short)
            .unwrap();
        struct Jobs(PathBuf);
        impl Drop for Jobs {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        let jobs = Jobs(short);
        let root = PrivateDirectory::open(&jobs.0).unwrap();
        let own = ProcessImage::capture(std::process::id()).unwrap();
        let digest = digest_file(own.executable()).unwrap();
        // Synthetic installation only: make its old Controller a real native
        // image, then exercise the production job-local copy in normal handoff.
        std::fs::copy(
            std::env::current_exe().unwrap(),
            f.bin.join("treer-agent-server"),
        )
        .unwrap();
        std::fs::set_permissions(
            f.bin.join("treer-agent-server"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        for mode in [
            "normal",
            "wrong-image",
            "attached",
            "exit-before-ack",
            "wrong-job",
            "existing-record",
        ] {
            let held = selected.lock().unwrap();
            let mut receipt = super::super::tests::receipt();
            receipt.workspace_id = "ws_a".into();
            receipt.server_id = "srv_a".into();
            receipt.publications = super::super::publication::Publications::new(
                digest.clone(),
                "0".repeat(64),
                &receipt.controller_sha256,
                &receipt.cli_sha256,
            );
            let journal = Journal::create(&root, &receipt).unwrap();
            let job = jobs.0.join(receipt.job_id.to_string());
            let activator = if mode == "normal" || mode == "wrong-image" {
                Some(
                    super::super::activator::ActivatorImage::prepare(&selected, &held, &journal)
                        .unwrap(),
                )
            } else {
                None
            };
            Endpoint::mark_prepared(&journal, &job).unwrap();
            if let Some(activator) = &activator {
                activator.verify(&journal).unwrap();
            }
            let prepared = journal.read().unwrap();
            if mode == "existing-record" {
                journal
                    .directory
                    .create_file("ownership.json")
                    .unwrap()
                    .write_all(b"uncertain previous takeover")
                    .unwrap();
            }
            let endpoint = Endpoint::bind(&journal, &job).unwrap();
            let mut child = Child(
                Command::new(if mode == "normal" {
                    job.join("activator")
                } else {
                    std::env::current_exe().unwrap()
                })
                .args([
                    "--exact",
                    "self_update::selection::tests::ownership_child",
                    "--ignored",
                ])
                .env_clear()
                .env("TREER_TEST_CONFIG", f.path.join("config"))
                .env("TREER_TEST_JOBS", &jobs.0)
                .env("TREER_TEST_JOB", receipt.job_id.to_string())
                .env("TREER_TEST_PARENT", own.pid().to_string())
                .env("TREER_TEST_DIGEST", &digest)
                .env("TREER_TEST_MODE", mode)
                .stdin(Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
            );
            if mode == "wrong-image" {
                let deadline = Instant::now() + Duration::from_secs(30);
                loop {
                    if let Some(status) = child.0.try_wait().unwrap() {
                        assert!(status.success());
                        break;
                    }
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert_eq!(journal.read().unwrap(), prepared);
                assert!(!job.join("ownership.json").exists());
                assert!(selected.lock().is_err());
                assert!(other.lock().is_err());
                drop(held);
                assert!(selected.lock().is_ok());
                continue;
            }
            if mode == "normal" {
                // Image verification hashes the debug binary before connect;
                // keep that startup work outside the socket exchange budget.
                let deadline = Instant::now() + Duration::from_secs(30);
                while !job.join("test-child-bootstrapped").exists() {
                    assert!(Instant::now() < deadline);
                    assert!(child.0.try_wait().unwrap().is_none());
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            let image = ProcessImage::capture(child.0.id()).unwrap();
            if mode == "attached" {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match endpoint.accept(&image, own.executable(), &digest) {
                        Ok(None) => {
                            assert!(Instant::now() < deadline);
                            assert!(child.0.try_wait().unwrap().is_none());
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => {
                            assert_eq!(error.to_string(), "update_handoff_session_unavailable");
                            break;
                        }
                        Ok(Some(_)) => panic!("attached child must not get a transfer capability"),
                    }
                }
                assert_eq!(
                    endpoint
                        .accept(&image, own.executable(), &digest)
                        .err()
                        .unwrap()
                        .to_string(),
                    "update_handoff_aborted"
                );
                assert_eq!(journal.read().unwrap(), prepared);
                assert!(!job.join("ownership.json").exists());
                assert!(selected.lock().is_err());
                assert!(other.lock().is_err());
                drop(child.0.stdin.take());
                assert!(child.0.wait().unwrap().success());
                drop(held);
                assert!(selected.lock().is_ok());
                continue;
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            let peer: AuthenticatedPeer = loop {
                let verified = activator
                    .as_ref()
                    .map(|image| image.image())
                    .unwrap_or(own.executable());
                if let Some(peer) = endpoint.accept(&image, verified, &digest).unwrap() {
                    break peer;
                }
                assert!(Instant::now() < deadline);
                assert!(child.0.try_wait().unwrap().is_none());
                std::thread::sleep(Duration::from_millis(5));
            };
            if mode == "normal" || mode == "wrong-job" {
                // Hashing this unoptimized test executable is separate from
                // the ownership exchange. Begin only with both peers authed.
                std::fs::write(job.join("test-parent-authed"), b"ready").unwrap();
                let auth_deadline = Instant::now() + Duration::from_secs(30);
                while !job.join("test-child-authed").exists() {
                    assert!(Instant::now() < auth_deadline);
                    assert!(child.0.try_wait().unwrap().is_none());
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            // An authenticated image alone is not detached from the submitting
            // exec's process group/session. Prove that before transferring locks.
            image.verify_private_session().unwrap();
            let accepted = peer.offer_ownership(&journal, &selected, &held);
            if mode == "normal" {
                assert_eq!(accepted.unwrap().prepared(), &prepared);
                let ready_deadline = Instant::now() + Duration::from_secs(5);
                while !job.join("test-child-ready").exists() {
                    assert!(
                        child.0.try_wait().unwrap().is_none(),
                        "activator exited before lock probe"
                    );
                    assert!(Instant::now() < ready_deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
                let stored: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(job.join("ownership.json")).unwrap())
                        .unwrap();
                assert_eq!(stored["pid"], child.0.id());
                assert_eq!(stored["prepared"]["job_id"], receipt.job_id.to_string());
                drop(held);
                assert!(selected.lock().is_err());
                assert!(other.lock().is_err());
            } else {
                assert!(accepted.is_err());
                assert!(selected.lock().is_err());
                assert_eq!(journal.read().unwrap(), prepared);
                if mode == "existing-record" {
                    assert_eq!(
                        std::fs::read(job.join("ownership.json")).unwrap(),
                        b"uncertain previous takeover"
                    );
                } else {
                    assert!(!job.join("ownership.json").exists());
                }
                drop(held);
            }
            std::fs::write(job.join("test-child-release"), b"release").unwrap();
            drop(child.0.stdin.take());
            assert!(child.0.wait().unwrap().success(), "child failed: {mode}");
            assert!(selected.lock().is_ok());
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[ignore = "owned activator fixture, invoked by ownership_case"]
    fn ownership_child() {
        use super::super::{handoff::AuthenticatedPeer, Journal};
        use std::{io::Read, os::unix::net::UnixStream};
        use treer_platform::process_image::ProcessImage;

        let config = PathBuf::from(std::env::var_os("TREER_TEST_CONFIG").unwrap());
        let jobs = PathBuf::from(std::env::var_os("TREER_TEST_JOBS").unwrap());
        let job = std::env::var("TREER_TEST_JOB").unwrap();
        let mode = std::env::var("TREER_TEST_MODE").unwrap();
        if mode != "attached" && mode != "normal" && mode != "wrong-image" {
            treer_platform::process_image::enter_private_session().unwrap();
        }
        let inventory = Inventory::load(&config, "fixture-host").unwrap();
        let selected = inventory.select(Some("ws_a"), true).unwrap();
        let root = PrivateDirectory::open(&jobs).unwrap();
        let journal = Journal::open(&root, &job).unwrap();
        let running = if mode == "normal" || mode == "wrong-image" {
            use super::super::activator::RunningActivator;
            let other = inventory.select(Some("ws_b"), true).unwrap();
            assert_eq!(
                RunningActivator::enter(&other, &journal)
                    .err()
                    .unwrap()
                    .to_string(),
                "update_activator_job_mismatch"
            );
            if mode == "wrong-image" {
                // Same native bytes, different inode: reject before setsid or
                // connecting, without a transfer capability or journal writes.
                assert_eq!(
                    RunningActivator::enter(&selected, &journal)
                        .err()
                        .unwrap()
                        .to_string(),
                    "update_activator_image_conflict"
                );
                assert!(ProcessImage::capture(std::process::id())
                    .unwrap()
                    .verify_private_session()
                    .is_err());
                return;
            }
            let running = RunningActivator::enter(&selected, &journal).unwrap();
            running.verify(&selected, &journal).unwrap();
            assert!(running.verify(&other, &journal).is_err());
            std::fs::write(jobs.join(&job).join("test-child-bootstrapped"), b"ready").unwrap();
            Some(running)
        } else {
            None
        };
        let mut stream = UnixStream::connect(jobs.join(&job).join("handoff.sock")).unwrap();
        let parent =
            ProcessImage::capture(std::env::var("TREER_TEST_PARENT").unwrap().parse().unwrap())
                .unwrap();
        let digest = std::env::var("TREER_TEST_DIGEST").unwrap();
        if mode == "attached" || mode == "exit-before-ack" || mode == "existing-record" {
            // Keep the authenticated peer alive until parent consumes the
            // connection. Then EOF makes the transfer fail before any ACK.
            stream.read_to_end(&mut Vec::new()).unwrap();
            return;
        }
        let peer =
            AuthenticatedPeer::authenticate(stream, &parent, parent.executable(), &digest).unwrap();
        std::fs::write(jobs.join(&job).join("test-child-authed"), b"ready").unwrap();
        let auth_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !jobs.join(&job).join("test-parent-authed").exists() {
            assert!(std::time::Instant::now() < auth_deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let result = if mode == "wrong-job" {
            // Same job ID but another selected enrollment is not interchangeable.
            let other = inventory.select(Some("ws_b"), true).unwrap();
            peer.take_ownership(&journal, &other)
        } else {
            peer.take_ownership(&journal, &selected)
        };
        if mode == "wrong-job" {
            assert!(result.is_err());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !jobs.join(&job).join("test-child-release").exists() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            return;
        }
        let owned = result.unwrap();
        if let Some(running) = &running {
            running.verify(&selected, &journal).unwrap();
        }
        assert_eq!(owned.prepared(), &journal.read().unwrap());
        assert!(owned
            .locks()
            .directory(&selected, Binary::Controller)
            .is_ok());
        std::fs::write(jobs.join(&job).join("test-child-ready"), b"ready").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !jobs.join(&job).join("test-child-release").exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(owned);
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn transferred_locks_have_no_unlock_gap_and_cannot_adopt_another_enrollment() {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        f.enrollment("srv_b", "ws_b", &f.bin.join("treer-agent-server"));
        let a = f.inventory().select(Some("ws_a"), true).unwrap();
        let b = f.inventory().select(Some("ws_b"), true).unwrap();
        let held = a.lock().unwrap();
        let (sender, receiver) = std::os::unix::net::UnixStream::pair().unwrap();
        held.send(&sender).unwrap();
        assert!(SelectionLocks::receive(&b, &receiver).is_err());
        assert!(a.lock().is_err());
        let (sender, receiver) = std::os::unix::net::UnixStream::pair().unwrap();
        let process =
            treer_platform::process_image::ProcessImage::capture(std::process::id()).unwrap();
        let digest = super::super::images::digest_file(process.executable()).unwrap();
        let sender = super::super::handoff::AuthenticatedPeer::authenticate(
            sender,
            &process,
            process.executable(),
            &digest,
        )
        .unwrap();
        let receiver = super::super::handoff::AuthenticatedPeer::authenticate(
            receiver,
            &process,
            process.executable(),
            &digest,
        )
        .unwrap();
        sender.send_locks(&held).unwrap();
        let adopted = receiver.receive_locks(&a).unwrap();
        drop(held);
        assert!(a.lock().is_err());
        assert!(b.lock().is_err());
        assert!(adopted.directory(&a, Binary::Controller).is_ok());
        drop(adopted);
        assert!(a.lock().is_ok());
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
    fn health_address_uses_only_saved_numeric_listener_with_family_specific_loopback() {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        let mut selected = f.inventory().select(None, false).unwrap();
        for (listen, expected) in [
            ("0.0.0.0:1234", "127.0.0.1:1234"),
            ("[::]:1234", "[::1]:1234"),
            ("192.0.2.1:1234", "192.0.2.1:1234"),
        ] {
            selected.enrollment.controller.listen = listen.into();
            assert_eq!(selected.health_address().unwrap().to_string(), expected);
        }
        for invalid in ["127.0.0.1:0", "localhost:1234", "http://127.0.0.1:1234/"] {
            selected.enrollment.controller.listen = invalid.into();
            assert!(selected.health_address().is_err());
        }
    }

    #[tokio::test]
    async fn real_health_observation_binds_enrollment_locks_and_image_not_just_http_success() {
        use super::super::{health::Monitor, safety::ControllerExpectation};
        use sha2::{Digest, Sha256};
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };

        let f = Fixture::new();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        let config = f.path.join("config/srv_a-controller.json");
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        json["listen"] = listener.local_addr().unwrap().to_string().into();
        std::fs::write(config, serde_json::to_vec(&json).unwrap()).unwrap();
        let selected = f.inventory().select(None, false).unwrap();
        let locks = selected.lock().unwrap();
        let mut expected = ControllerExpectation {
            workspace: "ws_other".into(),
            server: "srv_a".into(),
            previous_epoch: "old-epoch".into(),
            artifact_source_sha: "a".repeat(40),
            executable_sha256: format!("{:x}", Sha256::digest(b"controller")),
        };
        let monitor = Monitor::new().unwrap();
        assert_eq!(
            monitor
                .observe(&selected, &locks, &expected)
                .await
                .err()
                .unwrap()
                .to_string(),
            "update_health_binding_mismatch"
        );
        expected.workspace = "ws_a".into();
        let digest = expected.executable_sha256.clone();
        expected.executable_sha256 = "b".repeat(64);
        assert_eq!(
            monitor
                .observe(&selected, &locks, &expected)
                .await
                .err()
                .unwrap()
                .to_string(),
            "update_controller_health_mismatch"
        );
        // Neither precondition failure may contact a Controller.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
        expected.executable_sha256 = digest;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
                assert!(request.len() < 8192);
            }
            let body = serde_json::json!({"service":"treer-agent-server", "workspace_id":"ws_a", "server_id":"srv_a", "controller_epoch":"new-epoch", "controller_build":{"version":"0.1.14", "git_commit":"a".repeat(40)}, "proxy_connected":true}).to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        assert_eq!(
            monitor.observe(&selected, &locks, &expected).await.unwrap(),
            "new-epoch"
        );
        server.await.unwrap();
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
