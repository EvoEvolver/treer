//! Copy checked old images, then publish backup-only evidence. Never replace
//! an installed image, mark rollback/update success or resume an old job.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

const STARTUP_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StartupDigest {
    pub(super) sha256: String,
    pub(super) size: u64,
}

pub(super) fn startup_digest(bytes: &[u8], workspace: &str, server: &str) -> Result<StartupDigest> {
    #[derive(Deserialize)]
    struct Store {
        version: u32,
        entries: Vec<crate::startup_store::StoredAgentStartup>,
        #[serde(default)]
        denied_agent_ids: Vec<String>,
    }
    if bytes.is_empty() || bytes.len() as u64 > STARTUP_LIMIT {
        bail!("windows_update_startup_size_refused");
    }
    let store: Store = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("windows_update_startup_format_refused"))?;
    if !matches!(store.version, 1 | 2)
        || store.entries.len() > 4096
        || store.denied_agent_ids.len() > 4096
        || (store.version == 1 && !store.denied_agent_ids.is_empty())
    {
        bail!("windows_update_startup_format_refused");
    }
    let valid_id = |id: &str| {
        id.strip_prefix("ag_").is_some_and(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
    };
    let mut denied = std::collections::BTreeSet::new();
    for id in store.denied_agent_ids {
        if !valid_id(&id) || !denied.insert(id) {
            bail!("windows_update_startup_format_refused");
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    for entry in store.entries {
        if entry.workspace_id != workspace
            || entry.spec.server_id != server
            || entry.spec.generation == 0
            || !valid_id(&entry.spec.agent_id)
            || denied.contains(&entry.spec.agent_id)
            || !ids.insert(entry.spec.agent_id)
        {
            bail!("windows_update_startup_binding_refused");
        }
    }
    Ok(StartupDigest {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        size: bytes.len() as u64,
    })
}

pub(super) fn copy_checked(
    source: &mut dyn Read,
    target: &mut dyn Write,
    expected: &str,
    limit: u64,
) -> Result<()> {
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0; 16 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        if size > limit {
            bail!("windows_update_backup_size_refused");
        }
        target.write_all(&buffer[..read])?;
        hash.update(&buffer[..read]);
    }
    if size == 0 || format!("{:x}", hash.finalize()) != expected {
        bail!("windows_update_backup_digest_refused");
    }
    Ok(())
}

#[cfg(windows)]
mod native {
    use super::super::ReservedInputs;
    use super::*;
    use std::io::{Seek, SeekFrom};
    use treer_platform::windows_update_backup::{BackupDirectory, SealedBackup};

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct Evidence {
        schema_version: u32,
        phase: Phase,
        binding: crate::windows_update_intent::Binding,
        directory_volume: u64,
        directory_file_id: u64,
        controller_file_id: u64,
        cli_file_id: u64,
        controller_size: u64,
        cli_size: u64,
    }

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum Phase {
        ImagesBackedUp,
    }

    /// Retains installed/target pins and installation lease with checked backup
    /// files. There is no public constructor, execution, resume or success API.
    pub struct BackedUpInputs {
        pub(super) reserved: ReservedInputs,
        pub(super) backup: SealedBackup,
    }

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct StartupEvidence {
        schema_version: u32,
        phase: StartupPhase,
        binding: crate::windows_update_intent::Binding,
        backup_directory: (u64, u64),
        state_directory: (u64, u64),
        source_file_id: Option<u64>,
        backup_file_id: Option<u64>,
        snapshot: Option<StartupDigest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller_state: Option<crate::controller::ControllerStateEvidence>,
    }

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum StartupPhase {
        StartupBackedUp,
    }

    /// Full image and startup backups only. No restore, mutation, resume or
    /// health authority; startup is one snapshot, not a concurrent-write CAS.
    pub struct StartupBackedUpInputs {
        pub(super) images: BackedUpInputs,
        pub(super) startup: Option<std::fs::File>,
        evidence: StartupEvidence,
    }

    impl BackedUpInputs {
        pub fn backup_configured_startup(self) -> Result<StartupBackedUpInputs> {
            let state = self
                .reserved
                .0
                .state
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("windows_update_runtime_authority_required"))?;
            state.matches(self.binding())?;
            let snapshot = state
                .directory()
                .startup_snapshot(&format!("{}-agent-startup.json", self.binding().server_id))
                .map_err(|_| anyhow::anyhow!("windows_update_startup_source_refused"))?;
            let state_id = state.directory().identity()?;
            let authority = state.evidence().clone();
            self.backup_startup_snapshot(state_id, snapshot, Some(authority))
        }

        /// Caller-supplied state directories are isolated test observations,
        /// never a production admission entry.
        #[cfg(test)]
        pub fn backup_startup(self, state: BackupDirectory) -> Result<StartupBackedUpInputs> {
            let state_id = state.identity()?;
            let name = format!("{}-agent-startup.json", self.binding().server_id);
            let snapshot = state
                .startup_snapshot(&name)
                .map_err(|_| anyhow::anyhow!("windows_update_startup_source_refused"))?;
            self.backup_startup_snapshot(state_id, snapshot, None)
        }

        fn backup_startup_snapshot(
            self,
            state_id: (u64, u64),
            snapshot: Option<(u64, Vec<u8>)>,
            controller_state: Option<crate::controller::ControllerStateEvidence>,
        ) -> Result<StartupBackedUpInputs> {
            self.inspect_backups()?;
            if state_id.0 != self.binding().installation_volume
                || state_id == self.backup.identity()?
                || state_id == self.reserved.0._installed.identity()?
                || state_id == self.reserved.0._target.identity()?
            {
                bail!("windows_update_startup_directory_refused");
            }
            let digest = snapshot
                .as_ref()
                .map(|(_, bytes)| {
                    startup_digest(
                        bytes,
                        &self.binding().workspace_id,
                        &self.binding().server_id,
                    )
                })
                .transpose()?;
            let startup = self
                .backup
                .create_startup(snapshot.as_ref().map(|(_, b)| b.as_slice()))?;
            let backup_file_id = startup
                .as_ref()
                .map(|f| treer_platform::transfer_file_identity(f).map(|(_, id, _, _)| id))
                .transpose()?;
            let evidence = StartupEvidence {
                schema_version: 1,
                phase: StartupPhase::StartupBackedUp,
                binding: self.binding().clone(),
                backup_directory: self.backup.identity()?,
                state_directory: state_id,
                source_file_id: snapshot.as_ref().map(|(id, _)| *id),
                backup_file_id,
                snapshot: digest,
                controller_state,
            };
            let backed = StartupBackedUpInputs {
                images: self,
                startup,
                evidence,
            };
            backed.verify_startup()?;
            backed
                .images
                .backup
                .publish_startup_evidence(&serde_json::to_vec(&backed.evidence)?)?;
            backed.inspect_startup()?;
            Ok(backed)
        }
    }

    impl StartupBackedUpInputs {
        pub fn binding(&self) -> &crate::windows_update_intent::Binding {
            self.images.binding()
        }

        fn verify_startup(&self) -> Result<()> {
            match (
                &self.images.reserved.0.state,
                &self.evidence.controller_state,
            ) {
                (Some(state), Some(expected)) => {
                    state.matches(self.binding())?;
                    if state.evidence() != expected
                        || state.directory().identity()? != self.evidence.state_directory
                    {
                        bail!("windows_update_runtime_evidence_refused");
                    }
                }
                (None, None) => {}
                _ => bail!("windows_update_runtime_evidence_refused"),
            }
            match (self.startup.as_ref(), self.evidence.snapshot.as_ref()) {
                (Some(mut file), Some(expected)) => {
                    file.seek(SeekFrom::Start(0))?;
                    let mut bytes = Vec::new();
                    file.take(STARTUP_LIMIT + 1).read_to_end(&mut bytes)?;
                    let (volume, id, _, _) =
                        treer_platform::transfer_file_identity(self.startup.as_ref().unwrap())?;
                    if startup_digest(
                        &bytes,
                        &self.binding().workspace_id,
                        &self.binding().server_id,
                    )? != *expected
                        || volume != self.evidence.backup_directory.0
                        || id == 0
                        || Some(id) != self.evidence.backup_file_id
                    {
                        bail!("windows_update_startup_readback_refused");
                    }
                }
                (None, None)
                    if self.evidence.source_file_id.is_none()
                        && self.evidence.backup_file_id.is_none()
                        && self.images.backup.startup_absent()? => {}
                _ => bail!("windows_update_startup_readback_refused"),
            }
            Ok(())
        }

        pub fn inspect_startup(&self) -> Result<()> {
            self.images.inspect_backups()?;
            self.verify_startup()?;
            let evidence: StartupEvidence =
                serde_json::from_slice(&self.images.backup.read_startup_evidence()?)
                    .map_err(|_| anyhow::anyhow!("windows_update_startup_evidence_refused"))?;
            if evidence != self.evidence {
                bail!("windows_update_startup_evidence_conflict");
            }
            Ok(())
        }
    }

    impl ReservedInputs {
        /// Consume a single reservation. Partial files and unknown publication
        /// remain evidence on error, never cleaned up or automatically retried.
        pub fn backup(self, directory: BackupDirectory) -> Result<BackedUpInputs> {
            let identity = directory.identity()?;
            if identity.0 != self.binding().installation_volume
                || identity == self.0._installed.identity()?
                || identity == self.0._target.identity()?
                || self
                    .0
                    .state
                    .as_ref()
                    .map(|s| s.directory().identity())
                    .transpose()?
                    == Some(identity)
            {
                bail!("windows_update_backup_directory_refused");
            }
            self.0.inspect_installation_claim()?;
            let mut writing = directory.create()?;
            let (mut old_controller, mut old_cli) = self.0._installed.files();
            old_controller.seek(SeekFrom::Start(0))?;
            old_cli.seek(SeekFrom::Start(0))?;
            let (controller, cli) = writing.files();
            copy_checked(
                &mut old_controller,
                controller,
                &self.binding().old_controller_sha256,
                128 * 1024 * 1024,
            )?;
            copy_checked(
                &mut old_cli,
                cli,
                &self.binding().old_cli_sha256,
                128 * 1024 * 1024,
            )?;
            let backed = BackedUpInputs {
                reserved: self,
                backup: writing.seal()?,
            };
            backed.verify_images()?;
            let bytes = serde_json::to_vec(&backed.expected_evidence()?)?;
            backed.backup.publish_evidence(&bytes)?;
            backed.inspect_evidence()?;
            Ok(backed)
        }
    }

    impl BackedUpInputs {
        pub fn binding(&self) -> &crate::windows_update_intent::Binding {
            self.reserved.binding()
        }

        fn expected_evidence(&self) -> Result<Evidence> {
            let (volume, directory_id) = self.backup.identity()?;
            let (controller, cli) = self.backup.files();
            let (cv, controller_id, _, _) = treer_platform::transfer_file_identity(controller)?;
            let (lv, cli_id, _, _) = treer_platform::transfer_file_identity(cli)?;
            if volume != cv
                || volume != lv
                || directory_id == 0
                || controller_id == 0
                || cli_id == 0
            {
                bail!("windows_update_backup_identity_refused");
            }
            Ok(Evidence {
                schema_version: 1,
                phase: Phase::ImagesBackedUp,
                binding: self.binding().clone(),
                directory_volume: volume,
                directory_file_id: directory_id,
                controller_file_id: controller_id,
                cli_file_id: cli_id,
                controller_size: controller.metadata()?.len(),
                cli_size: cli.metadata()?.len(),
            })
        }

        fn verify_images(&self) -> Result<()> {
            let (mut controller, mut cli) = self.backup.files();
            controller.seek(SeekFrom::Start(0))?;
            cli.seek(SeekFrom::Start(0))?;
            if super::super::digest(&mut controller, None)? != self.binding().old_controller_sha256
                || super::super::digest(&mut cli, None)? != self.binding().old_cli_sha256
            {
                bail!("windows_update_backup_readback_refused");
            }
            Ok(())
        }

        fn inspect_evidence(&self) -> Result<()> {
            let actual: Evidence = serde_json::from_slice(&self.backup.read_evidence()?)?;
            if actual != self.expected_evidence()? {
                bail!("windows_update_backup_evidence_conflict");
            }
            Ok(())
        }

        /// Check held bytes and the strict, exact evidence. Successful inspection
        /// grants no replacement, rollback, retirement or health authority.
        pub fn inspect_backups(&self) -> Result<()> {
            self.verify_images()?;
            self.inspect_evidence()
        }
    }
}

#[cfg(windows)]
pub use native::BackedUpInputs;
#[cfg(windows)]
pub use native::StartupBackedUpInputs;

#[cfg(windows)]
#[path = "windows_update_publication.rs"]
mod publication;
#[cfg(windows)]
pub use publication::{FilePairPhase, PublishedInputs};

#[cfg(test)]
mod tests {
    use super::*;

    fn startup() -> serde_json::Value {
        serde_json::json!({"version":2,"denied_agent_ids":["ag_deleted"],"entries":[{
            "agent_id":"ag_kept","server_id":"srv_fixture","workspace_id":"ws_fixture",
            "generation":1,"kind":"shell","name":"fixture","cwd":"private-cwd",
            "command":"fixture","args":["private-argument"],"enabled":false,
            "workload_credential":"private-credential","last_host_epoch":"fixture"
        }]})
    }

    #[test]
    fn windows_update_startup_format_binds_complete_v1_v2_bytes_and_denial_only_state() {
        let mut value = startup();
        let check = |v: &serde_json::Value| {
            startup_digest(&serde_json::to_vec(v).unwrap(), "ws_fixture", "srv_fixture")
        };
        let mixed = check(&value).unwrap();
        value["entries"] = serde_json::json!([]);
        let denial_only = check(&value).unwrap();
        assert_ne!(mixed, denial_only);
        value["denied_agent_ids"] = serde_json::json!(["ag_deleted", "ag_later"]);
        assert_ne!(denial_only, check(&value).unwrap());
        value["version"] = 1.into();
        value.as_object_mut().unwrap().remove("denied_agent_ids");
        check(&value).unwrap();
        let safe = serde_json::to_string(&mixed).unwrap();
        assert!(!safe.contains("private"));
        assert!(!safe.contains("ag_deleted"));
    }

    #[test]
    fn windows_update_startup_format_refuses_invalid_scope_fences_and_bounds_without_echo() {
        for case in 0..9 {
            let mut v = startup();
            match case {
                0 => v["version"] = 1.into(),
                1 => v["denied_agent_ids"] = serde_json::json!(["ag_deleted", "ag_deleted"]),
                2 => v["denied_agent_ids"] = serde_json::json!(["ag_kept"]),
                3 => v["entries"][0]["workspace_id"] = "ws_wrong".into(),
                4 => v["entries"][0]["server_id"] = "srv_wrong".into(),
                5 => v["entries"][0]["generation"] = 0.into(),
                6 => v["denied_agent_ids"] = serde_json::json!(["private-invalid"]),
                7 => v["entries"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("command")
                    .map(|_| ())
                    .unwrap(),
                _ => {
                    v["denied_agent_ids"] =
                        serde_json::json!((0..4097).map(|i| format!("ag_{i}")).collect::<Vec<_>>())
                }
            }
            let error = startup_digest(
                &serde_json::to_vec(&v).unwrap(),
                "ws_fixture",
                "srv_fixture",
            )
            .unwrap_err()
            .to_string();
            assert!(!error.contains("private"));
        }
        assert!(startup_digest(
            &vec![b' '; STARTUP_LIMIT as usize + 1],
            "ws_fixture",
            "srv_fixture"
        )
        .is_err());
        assert!(startup_digest(b"", "ws_fixture", "srv_fixture").is_err());
    }

    #[test]
    fn windows_update_backup_copy_checks_full_bytes_bounds_and_digest() {
        let bytes = b"original image";
        let hash = format!("{:x}", Sha256::digest(bytes));
        let mut target = Vec::new();
        copy_checked(&mut &bytes[..], &mut target, &hash, 32).unwrap();
        assert_eq!(target, bytes);
        assert!(copy_checked(&mut &bytes[..], &mut Vec::new(), &hash, 1).is_err());
        assert!(copy_checked(&mut &bytes[..], &mut Vec::new(), &"0".repeat(64), 32).is_err());
        assert!(copy_checked(&mut &b""[..], &mut Vec::new(), &hash, 32).is_err());
    }

    #[test]
    fn windows_update_backup_copy_propagates_read_and_partial_write_failure() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
        }
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::WriteZero.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(copy_checked(&mut Broken, &mut Vec::new(), &"0".repeat(64), 32).is_err());
        let hash = format!("{:x}", Sha256::digest(b"abc"));
        assert!(copy_checked(&mut &b"abc"[..], &mut Broken, &hash, 32).is_err());
        struct Partial(Vec<u8>);
        impl Write for Partial {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if !self.0.is_empty() {
                    return Err(std::io::ErrorKind::WriteZero.into());
                }
                self.0.extend_from_slice(&bytes[..2]);
                Ok(2)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut partial = Partial(Vec::new());
        assert!(copy_checked(&mut &b"abc"[..], &mut partial, &hash, 32).is_err());
        assert_eq!(partial.0, b"ab");
    }
}
