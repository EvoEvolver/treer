//! Minimal saved-enrollment projection. Never deserialize/log credentials or
//! infer an installation from current_exe, PATH, a stage, or runtime state root.
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

fn refused() -> anyhow::Error {
    anyhow!("windows_update_installation_unconfirmed")
}

#[derive(Deserialize)]
struct ControllerRecord {
    workspace: String,
    server_id: String,
    root: PathBuf,
    listen: String,
    host_socket: PathBuf,
    install_hostname: String,
}
#[derive(Deserialize)]
struct HostRecord {
    controller_path: PathBuf,
    controller_config_path: PathBuf,
    socket_path: PathBuf,
    root: PathBuf,
}
#[cfg_attr(not(windows), allow(dead_code))]
struct Enrollment {
    controller: ControllerRecord,
    host: HostRecord,
}

impl Enrollment {
    fn parse(
        directory: &Path,
        name: &str,
        controller: &[u8],
        host: &[u8],
        hostname: &str,
    ) -> Result<Option<Self>> {
        let server = name.strip_suffix("-controller.json").ok_or_else(refused)?;
        let controller: ControllerRecord =
            serde_json::from_slice(controller).map_err(|_| refused())?;
        if !crate::windows_update_intent::identifier(server, "srv_")
            || controller.server_id != server
            || !crate::windows_update_intent::identifier(&controller.workspace, "ws_")
        {
            return Err(refused());
        }
        if controller.install_hostname != hostname {
            return Ok(None);
        }
        let host: HostRecord = serde_json::from_slice(host).map_err(|_| refused())?;
        if host.controller_config_path != directory.join(name)
            || host.root != controller.root
            || host.socket_path != controller.host_socket
            || controller.listen.parse::<std::net::SocketAddr>().is_err()
            || host.controller_path.file_name()
                != Some(std::ffi::OsStr::new("treer-agent-server.exe"))
        {
            return Err(refused());
        }
        Ok(Some(Self { controller, host }))
    }
}

#[cfg(windows)]
pub(crate) mod native {
    use super::*;
    use treer_platform::{
        windows_update_inputs::{LockedInstallation, PinnedPair},
        windows_update_preparation::{ConfigDirectory, ConfigSnapshot},
    };
    pub(crate) struct Selection {
        directory: ConfigDirectory,
        names: Vec<String>,
        _snapshots: Vec<ConfigSnapshot>,
        entries: Vec<(Enrollment, PinnedPair)>,
        selected: usize,
        shared: Vec<String>,
        catalog: crate::controller_update::catalog::Catalog,
    }
    impl Selection {
        pub(crate) fn local(workspace: &str, server: &str) -> Result<Self> {
            let directory = crate::service::controller_update_config_directory()?;
            Self::load(
                &directory,
                &crate::service::current_hostname()?,
                workspace,
                server,
            )
        }
        fn load(directory: &Path, hostname: &str, workspace: &str, server: &str) -> Result<Self> {
            let load = || -> Result<Self> {
                let files = ConfigDirectory::open(directory)?;
                let names = files.controller_names()?;
                let snapshot = files.read("controller-update-catalog.json")?;
                let catalog = crate::controller_update::catalog::Catalog::parse(snapshot.bytes())?;
                let mut entries = Vec::new();
                let mut total = snapshot.bytes().len();
                let mut snapshots = vec![snapshot];
                for name in &names {
                    let server = name.strip_suffix("-controller.json").ok_or_else(refused)?;
                    if !crate::windows_update_intent::identifier(server, "srv_") {
                        return Err(refused());
                    }
                    let controller = files.read(name)?;
                    let host = files.read(&format!("{server}-host.json"))?;
                    total += controller.bytes().len() + host.bytes().len();
                    if total > 16 * 1024 * 1024 {
                        return Err(refused());
                    }
                    if let Some(enrollment) = Enrollment::parse(
                        directory,
                        name,
                        controller.bytes(),
                        host.bytes(),
                        hostname,
                    )? {
                        let path = &enrollment.host.controller_path;
                        if !path.is_absolute()
                            || path.components().any(|c| {
                                matches!(
                                    c,
                                    std::path::Component::ParentDir | std::path::Component::CurDir
                                )
                            })
                        {
                            return Err(refused());
                        }
                        let pair = PinnedPair::open(path.parent().ok_or_else(refused)?)?;
                        entries.push((enrollment, pair));
                    }
                    snapshots.extend([controller, host]);
                }
                let matches: Vec<_> = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| {
                        e.0.controller.workspace == workspace && e.0.controller.server_id == server
                    })
                    .map(|(i, _)| i)
                    .collect();
                if matches.len() != 1 {
                    return Err(refused());
                }
                let selected = matches[0];
                let identity = entries[selected].1.pair_identity()?;
                let mut shared = Vec::new();
                for (index, (entry, pair)) in entries.iter().enumerate() {
                    let other = pair.pair_identity()?;
                    if index != selected
                        && (identity.controller == other.controller || identity.cli == other.cli)
                    {
                        shared.push(entry.controller.server_id.clone());
                    }
                }
                shared.sort();
                shared.dedup();
                Ok(Self {
                    directory: files,
                    names,
                    _snapshots: snapshots,
                    entries,
                    selected,
                    shared,
                    catalog,
                })
            };
            load().map_err(|_| refused())
        }
        pub(crate) fn shared(&self) -> &[String] {
            &self.shared
        }
        pub(crate) fn confirm(
            &self,
            dispatch: &treer_protocol::controller_update_runtime::DispatchBinding,
            expected_shared: &[String],
        ) -> Result<()> {
            self.catalog.select_plan(dispatch)?;
            let entry = &self.entries[self.selected].0.controller;
            if dispatch.workspace_id != entry.workspace
                || dispatch.server_id != entry.server_id
                || expected_shared != self.shared
                || (!self.shared.is_empty() && !self.catalog.accept_shared_binaries)
            {
                return Err(refused());
            }
            Ok(())
        }
        pub(crate) fn check(&self, runtime: &crate::controller::ControllerRuntime) -> Result<()> {
            if self.names != self.directory.controller_names().map_err(|_| refused())? {
                return Err(refused());
            }
            let entry = &self.entries[self.selected].0.controller;
            runtime.check_update_enrollment(&entry.workspace, &entry.server_id, &entry.root)
        }
        pub(crate) fn lock(&self) -> Result<LockedInstallation> {
            // Independently pin exact physical identities under the originals.
            let pair = &self.entries[self.selected].1;
            let reopened = PinnedPair::open_matching(pair.directory_path(), pair.pair_identity()?)
                .map_err(|_| refused())?;
            reopened.lock_installation().map_err(|_| refused())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_update_installation_stage_projection_exact_saved_binding_no_secrets() {
        let dir = std::env::temp_dir();
        let root = dir.join("machine");
        let c = serde_json::json!({"workspace":"ws_test","server_id":"srv_test","root":root,"listen":"127.0.0.1:8790","host_socket":"pipe","install_hostname":"fixture","machine_token":"must-never-enter-diagnostic"});
        let h = serde_json::json!({"controller_path":root.join("treer-agent-server.exe"),"controller_config_path":dir.join("srv_test-controller.json"),"root":root,"socket_path":"pipe"});
        let parse = |c: &serde_json::Value, h: &serde_json::Value| {
            Enrollment::parse(
                &dir,
                "srv_test-controller.json",
                &serde_json::to_vec(c).unwrap(),
                &serde_json::to_vec(h).unwrap(),
                "fixture",
            )
        };
        assert!(parse(&c, &h).unwrap().is_some());
        for field in ["workspace", "server_id", "listen"] {
            let mut bad = c.clone();
            bad[field] = serde_json::json!("invalid");
            assert_eq!(
                parse(&bad, &h).err().unwrap().to_string(),
                "windows_update_installation_unconfirmed"
            );
        }
        let mut bad = h.clone();
        bad["controller_config_path"] = serde_json::json!(dir.join("decoy.json"));
        assert!(parse(&c, &bad).is_err());
        let mut foreign = c.clone();
        foreign["install_hostname"] = serde_json::json!("other");
        assert!(parse(&foreign, &h).unwrap().is_none());
    }
}
