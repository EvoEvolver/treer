//! Activation invariants. These checks are deliberately stricter than a live
//! PID/HTTP-200 check. A caller must collect the OS identity and image digests
//! from checked handles, not trust a Controller's self-reported image hash.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use treer_host_protocol::{HostBuildInfo, HostOutputReplay, HostResponse, OutputCursor};

use super::{hex, identifier, Journal, Phase, Receipt};

const HEALTH_LIMIT: usize = 64 * 1024;
const INVENTORY_LIMIT: usize = 4096;
const BASELINE_LIMIT: u64 = 8 * 1024 * 1024;

/// Private transaction observation; contains neither a path nor credentials.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostIdentity {
    pub pid: u32,
    pub creation_identity: String,
    pub executable_sha256: String,
}

impl HostIdentity {
    fn validate(&self) -> Result<()> {
        if self.pid == 0
            || !label(&self.creation_identity, 128)
            || !hex(&self.executable_sha256, 64)
        {
            bail!("invalid_update_host_identity");
        }
        Ok(())
    }
}

fn label(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

/// Only these non-secret metadata fields are retained. No Debug or generic JSON
/// error is returned: Host metadata also contains workload credentials.
#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Metadata {
    agent_id: String,
    run_generation: u64, // Required: do not silently adopt a legacy default zero.
    workspace_id: String,
    server_id: String,
    kind: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Witness {
    revision: u64,
    sha256: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Workload {
    pid: Option<u32>,
    running: bool,
    started_at: DateTime<Utc>,
    exited_at: Option<DateTime<Utc>>,
    exit_code: Option<i32>,
    metadata: Metadata,
    stream_epoch: String,
    next_revision: u64,
    witness: Option<Witness>,
}

/// Kept privately by the transaction, never serialized into its public receipt.
#[derive(PartialEq, Eq)]
pub struct HostSnapshot {
    identity: HostIdentity,
    epoch: String,
    build: HostBuildInfo,
    workloads: BTreeMap<String, Workload>,
}

// Private format only. Use a list rather than a deserialized map so duplicate
// process IDs cannot silently overwrite part of the pre-update inventory.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredBaseline {
    schema_version: u32,
    binding: Binding,
    previous_controller_epoch: String,
    previous_controller_source_sha: String,
    identity: HostIdentity,
    epoch: String,
    build_version: String,
    build_git_commit: String,
    workloads: Vec<(String, Workload)>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Binding {
    job_id: uuid::Uuid,
    workspace_id: String,
    server_id: String,
    release_sha: String,
    artifact_source_sha: String,
    controller_sha256: String,
    cli_sha256: String,
    original_controller_sha256: String,
}

impl From<&Receipt> for Binding {
    fn from(receipt: &Receipt) -> Self {
        Self {
            job_id: receipt.job_id,
            workspace_id: receipt.workspace_id.clone(),
            server_id: receipt.server_id.clone(),
            release_sha: receipt.release_sha.clone(),
            artifact_source_sha: receipt.artifact_source_sha.clone(),
            controller_sha256: receipt.controller_sha256.clone(),
            cli_sha256: receipt.cli_sha256.clone(),
            original_controller_sha256: receipt
                .publications
                .original_digest(super::artifacts::Binary::Controller)
                .to_owned(),
        }
    }
}

fn replays(items: &[HostOutputReplay]) -> Result<BTreeMap<&str, &HostOutputReplay>> {
    let mut result = BTreeMap::new();
    for replay in items {
        if !label(&replay.process_id, 256)
            || !label(&replay.stream_epoch, 128)
            || replay.gap
            || replay.first_available_revision > replay.next_revision
            || result.insert(replay.process_id.as_str(), replay).is_some()
        {
            bail!("update_output_continuity_unproven");
        }
        let mut previous = None;
        for chunk in &replay.chunks {
            if chunk.process_id != replay.process_id
                || chunk.stream_epoch != replay.stream_epoch
                || chunk.revision < replay.first_available_revision
                || chunk.revision >= replay.next_revision
                || previous
                    .is_some_and(|revision: u64| revision.checked_add(1) != Some(chunk.revision))
            {
                bail!("update_output_continuity_unproven");
            }
            previous = Some(chunk.revision);
        }
        if previous.is_some_and(|revision| revision.checked_add(1) != Some(replay.next_revision)) {
            bail!("update_output_continuity_unproven");
        }
    }
    Ok(result)
}

impl HostSnapshot {
    fn validate_baseline(&self, workspace: &str, server: &str) -> Result<()> {
        self.identity.validate()?;
        if !identifier(workspace, "ws_")
            || !identifier(server, "srv_")
            || !label(&self.epoch, 128)
            || !label(&self.build.version, 128)
            || !hex(&self.build.git_commit, 40)
            || self.workloads.len() > INVENTORY_LIMIT
        {
            bail!("invalid_update_baseline");
        }
        for (id, workload) in &self.workloads {
            if !identifier(id, "ag_")
                || workload.metadata.agent_id != *id
                || workload.metadata.workspace_id != workspace
                || workload.metadata.server_id != server
                || workload.metadata.run_generation == 0
                || !label(&workload.metadata.kind, 64)
                || !label(&workload.stream_epoch, 128)
                || workload.pid == Some(0)
                || (workload.running && workload.pid.is_none())
                || workload.witness.as_ref().is_some_and(|witness| {
                    !hex(&witness.sha256, 64)
                        || witness.revision.checked_add(1) != Some(workload.next_revision)
                })
            {
                bail!("invalid_update_baseline");
            }
        }
        Ok(())
    }

    /// Persist the ORIGINAL observation, never an after-update replacement.
    /// Caller supplies the independently observed old Controller epoch/build. This
    /// private file contains only metadata and output hashes, no raw replay,
    /// cwd/argv/environment or workload credentials. No overwrite/retry API.
    pub(super) fn persist(
        &self,
        journal: &Journal,
        revision: u64,
        previous_controller_epoch: &str,
        previous_controller_source_sha: &str,
    ) -> Result<()> {
        let receipt = journal.read()?;
        if receipt.revision != revision
            || receipt.phase != Phase::Preparing
            || !label(previous_controller_epoch, 128)
            || !hex(previous_controller_source_sha, 40)
        {
            bail!("update_baseline_phase_conflict");
        }
        self.validate_baseline(&receipt.workspace_id, &receipt.server_id)?;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "binding": Binding::from(&receipt),
            "previous_controller_epoch": previous_controller_epoch,
            "previous_controller_source_sha": previous_controller_source_sha,
            "identity": self.identity,
            "epoch": self.epoch,
            "build_version": self.build.version,
            "build_git_commit": self.build.git_commit,
            "workloads": self.workloads.iter().collect::<Vec<_>>(),
        }))?;
        if bytes.len() as u64 > BASELINE_LIMIT {
            bail!("update_baseline_too_large");
        }
        let mut file = journal
            .directory
            .create_file("host-baseline.json")
            .map_err(|_| anyhow::anyhow!("update_baseline_exists_or_unsafe"))?;
        file.write_all(&bytes)
            .map_err(|_| anyhow::anyhow!("update_baseline_write_failed"))?;
        file.sync_all()
            .map_err(|_| anyhow::anyhow!("update_baseline_write_failed"))?;
        drop(file);
        journal
            .directory
            .sync()
            .map_err(|_| anyhow::anyhow!("update_baseline_write_failed"))?;
        Ok(())
    }

    pub(super) fn load(journal: &Journal) -> Result<(Self, String, String)> {
        let receipt = journal.read()?;
        let file = journal
            .directory
            .read_file("host-baseline.json")
            .map_err(|_| anyhow::anyhow!("update_baseline_missing_or_unsafe"))?;
        let mut bytes = Vec::new();
        file.take(BASELINE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("update_baseline_read_failed"))?;
        if bytes.len() as u64 > BASELINE_LIMIT {
            bail!("update_baseline_too_large");
        }
        let stored: StoredBaseline = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid_update_baseline"))?;
        if stored.schema_version != 2
            || stored.binding != Binding::from(&receipt)
            || !label(&stored.previous_controller_epoch, 128)
            || !hex(&stored.previous_controller_source_sha, 40)
            || stored.workloads.len() > INVENTORY_LIMIT
        {
            bail!("update_baseline_binding_mismatch");
        }
        let mut workloads = BTreeMap::new();
        for (id, workload) in stored.workloads {
            if workloads.insert(id, workload).is_some() {
                bail!("invalid_update_baseline");
            }
        }
        let snapshot = Self {
            identity: stored.identity,
            epoch: stored.epoch,
            build: HostBuildInfo {
                version: stored.build_version,
                git_commit: stored.build_git_commit,
            },
            workloads,
        };
        snapshot.validate_baseline(&receipt.workspace_id, &receipt.server_id)?;
        Ok((
            snapshot,
            stored.previous_controller_epoch,
            stored.previous_controller_source_sha,
        ))
    }

    /// Capture a full initial sync (no cursors). The Host creates process-list
    /// and replay observations separately, so output counters may advance
    /// between them; never require an artificial simultaneous snapshot.
    pub fn capture(
        response: &HostResponse,
        identity: HostIdentity,
        workspace: &str,
        server: &str,
    ) -> Result<Self> {
        identity.validate()?;
        if !identifier(workspace, "ws_") || !identifier(server, "srv_") {
            bail!("invalid_update_enrollment");
        }
        let HostResponse::Synced {
            host_epoch,
            host_build,
            processes,
            replay,
        } = response
        else {
            bail!("update_host_sync_required");
        };
        if !label(host_epoch, 128)
            || !label(&host_build.version, 128)
            || !hex(&host_build.git_commit, 40)
            || processes.len() > INVENTORY_LIMIT
            || replay.len() != processes.len()
        {
            bail!("invalid_update_host_inventory");
        }
        let output = replays(replay)?;
        let mut workloads = BTreeMap::new();
        for process in processes {
            if !label(&process.process_id, 256)
                || process.metadata.len() > HEALTH_LIMIT
                || process.first_revision > process.next_revision
                || process.pid == Some(0)
                || (process.running && process.pid.is_none())
            {
                bail!("invalid_update_host_inventory");
            }
            let metadata: Metadata = serde_json::from_str(&process.metadata)
                .map_err(|_| anyhow::anyhow!("update_host_inventory_unsupported"))?;
            if metadata.agent_id != process.process_id
                || metadata.workspace_id != workspace
                || metadata.server_id != server
                || !label(&metadata.kind, 64)
            {
                bail!("update_host_enrollment_mismatch");
            }
            let replay = output
                .get(process.process_id.as_str())
                .ok_or_else(|| anyhow::anyhow!("update_output_continuity_unproven"))?;
            if replay.stream_epoch != process.stream_epoch
                || replay.next_revision < process.next_revision
                || replay
                    .chunks
                    .first()
                    .is_some_and(|chunk| chunk.revision != replay.first_available_revision)
                || (replay.chunks.is_empty()
                    && replay.first_available_revision != replay.next_revision)
            {
                bail!("update_output_continuity_unproven");
            }
            let witness = replay.chunks.last().map(|chunk| Witness {
                revision: chunk.revision,
                sha256: format!("{:x}", Sha256::digest(&chunk.data)),
            });
            let workload = Workload {
                pid: process.pid,
                running: process.running,
                started_at: process.started_at,
                exited_at: process.exited_at,
                exit_code: process.exit_code,
                metadata,
                stream_epoch: process.stream_epoch.clone(),
                next_revision: replay.next_revision,
                witness,
            };
            if workloads
                .insert(process.process_id.clone(), workload)
                .is_some()
            {
                bail!("invalid_update_host_inventory");
            }
        }
        Ok(Self {
            identity,
            epoch: host_epoch.clone(),
            build: host_build.clone(),
            workloads,
        })
    }

    /// Ask for the last observed chunk again, so matching stream/counters alone
    /// cannot claim that retained output bytes survived. A genuine buffer rollover
    /// can make continuity unprovable: fail explicitly instead of claiming success.
    pub fn cursors(&self) -> BTreeMap<String, OutputCursor> {
        self.workloads
            .iter()
            .map(|(id, workload)| {
                (
                    id.clone(),
                    OutputCursor {
                        stream_epoch: workload.stream_epoch.clone(),
                        revision: workload
                            .witness
                            .as_ref()
                            .map_or(workload.next_revision.saturating_sub(1), |witness| {
                                witness.revision.saturating_sub(1)
                            }),
                    },
                )
            })
            .collect()
    }

    pub fn verify(&self, response: &HostResponse, identity: &HostIdentity) -> Result<()> {
        identity.validate()?;
        let HostResponse::Synced {
            host_epoch,
            host_build,
            processes,
            replay,
        } = response
        else {
            bail!("update_host_sync_required");
        };
        if identity != &self.identity || host_epoch != &self.epoch || host_build != &self.build {
            bail!("update_host_changed");
        }
        let ids: BTreeSet<_> = processes
            .iter()
            .map(|process| process.process_id.as_str())
            .collect();
        if processes.len() != self.workloads.len()
            || ids.len() != processes.len()
            || replay.len() != processes.len()
        {
            bail!("update_workload_changed");
        }
        let output = replays(replay)?;
        for process in processes {
            let previous = self
                .workloads
                .get(&process.process_id)
                .ok_or_else(|| anyhow::anyhow!("update_workload_changed"))?;
            if process.metadata.len() > HEALTH_LIMIT
                || process.first_revision > process.next_revision
            {
                bail!("invalid_update_host_inventory");
            }
            let metadata: Metadata = serde_json::from_str(&process.metadata)
                .map_err(|_| anyhow::anyhow!("update_host_inventory_unsupported"))?;
            if process.pid != previous.pid
                || process.running != previous.running
                || process.started_at != previous.started_at
                || process.exited_at != previous.exited_at
                || process.exit_code != previous.exit_code
                || metadata != previous.metadata
                || process.stream_epoch != previous.stream_epoch
            {
                bail!("update_workload_changed");
            }
            let replay = output
                .get(process.process_id.as_str())
                .ok_or_else(|| anyhow::anyhow!("update_output_continuity_unproven"))?;
            if replay.stream_epoch != previous.stream_epoch
                || replay.next_revision < previous.next_revision
                || replay.next_revision < process.next_revision
            {
                bail!("update_output_continuity_unproven");
            }
            if let Some(witness) = &previous.witness {
                let Some(chunk) = replay.chunks.first() else {
                    bail!("update_output_continuity_unproven");
                };
                if chunk.revision != witness.revision
                    || format!("{:x}", Sha256::digest(&chunk.data)) != witness.sha256
                {
                    bail!("update_output_continuity_unproven");
                }
            } else if replay
                .chunks
                .first()
                .is_some_and(|chunk| chunk.revision != previous.next_revision)
                || (replay.chunks.is_empty() && replay.next_revision != previous.next_revision)
            {
                bail!("update_output_continuity_unproven");
            }
        }
        Ok(())
    }
}

/// The installed image digest is independently observed by the activator.
/// `artifact_source_sha` is distinct from the wrapper release SHA for reused
/// native artifacts. HTTP health alone cannot satisfy this verification.
pub struct ControllerExpectation {
    pub workspace: String,
    pub server: String,
    pub previous_epoch: String,
    pub artifact_source_sha: String,
    pub executable_sha256: String,
}

#[derive(Deserialize)]
struct Health {
    service: String,
    workspace_id: String,
    server_id: String,
    controller_epoch: String,
    controller_build: HostBuildInfo,
    proxy_connected: bool,
}

/// Initial HTTP claims are bound to an independently checked old process image
/// by Monitor. Its build string is observed, not authenticated release metadata.
pub(super) fn initial_controller_epoch(
    document: &[u8],
    workspace: &str,
    server: &str,
) -> Result<(String, String)> {
    if document.len() > HEALTH_LIMIT {
        bail!("update_health_too_large");
    }
    let health: Health = serde_json::from_slice(document)
        .map_err(|_| anyhow::anyhow!("update_health_unsupported"))?;
    if health.service != "treer-agent-server"
        || health.workspace_id != workspace
        || health.server_id != server
        || !label(&health.controller_epoch, 128)
        || !label(&health.controller_build.version, 128)
        || !hex(&health.controller_build.git_commit, 40)
        || !health.proxy_connected
    {
        bail!("update_controller_health_mismatch");
    }
    Ok((health.controller_epoch, health.controller_build.git_commit))
}

impl ControllerExpectation {
    pub fn verify(&self, document: &[u8], installed_image_sha256: &str) -> Result<String> {
        if !identifier(&self.workspace, "ws_")
            || !identifier(&self.server, "srv_")
            || !label(&self.previous_epoch, 128)
            || !hex(&self.artifact_source_sha, 40)
            || !hex(&self.executable_sha256, 64)
        {
            bail!("invalid_update_health_expectation");
        }
        if document.len() > HEALTH_LIMIT {
            bail!("update_health_too_large");
        }
        let health: Health = serde_json::from_slice(document)
            .map_err(|_| anyhow::anyhow!("update_health_unsupported"))?;
        if health.service != "treer-agent-server"
            || health.workspace_id != self.workspace
            || health.server_id != self.server
            || !label(&health.controller_epoch, 128)
            || health.controller_epoch == self.previous_epoch
            || !health.proxy_connected
            || health.controller_build.git_commit != self.artifact_source_sha
            || installed_image_sha256 != self.executable_sha256
        {
            bail!("update_controller_health_mismatch");
        }
        Ok(health.controller_epoch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use treer_host_protocol::{HostOutputChunk, HostProcessInfo};

    #[test]
    fn initial_epoch_rejects_wrong_scope_or_unusable_old_controller_claims() {
        let base = json!({"service":"treer-agent-server", "workspace_id":"ws_fixture",
            "server_id":"srv_fixture", "controller_epoch":"observed-old",
            "controller_build":{"version":"0.1.14", "git_commit":"a".repeat(40)},
            "proxy_connected":true});
        let check = |value: &serde_json::Value| {
            initial_controller_epoch(
                &serde_json::to_vec(value).unwrap(),
                "ws_fixture",
                "srv_fixture",
            )
        };
        assert_eq!(
            check(&base).unwrap(),
            ("observed-old".into(), "a".repeat(40))
        );
        for (field, value) in [
            ("service", json!("other")),
            ("workspace_id", json!("ws_other")),
            ("server_id", json!("srv_other")),
            ("controller_epoch", json!("")),
            ("controller_epoch", json!("bad\nvalue")),
            ("controller_epoch", json!("x".repeat(129))),
            ("proxy_connected", json!(false)),
            (
                "controller_build",
                json!({"version":"", "git_commit":"a".repeat(40)}),
            ),
            (
                "controller_build",
                json!({"version":"0.1.14", "git_commit":"bad"}),
            ),
        ] {
            let mut value_under_test = base.clone();
            value_under_test[field] = value;
            assert!(check(&value_under_test).is_err(), "{field}");
        }
    }

    struct PrivateFixture(std::path::PathBuf);
    impl Drop for PrivateFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn journal() -> (PrivateFixture, Journal) {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tu-baseline-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let jobs = treer_platform::private_directory::PrivateDirectory::open(&root).unwrap();
        let now = Utc::now();
        let receipt = Receipt {
            schema_version: 1,
            job_id: uuid::Uuid::new_v4(),
            revision: 0,
            workspace_id: "ws_fixture".into(),
            server_id: "srv_fixture".into(),
            release_sha: "a".repeat(40),
            artifact_source_sha: "b".repeat(40),
            controller_sha256: "c".repeat(64),
            cli_sha256: "d".repeat(64),
            publications: super::super::publication::Publications::new(
                "e".repeat(64),
                "f".repeat(64),
                &"c".repeat(64),
                &"d".repeat(64),
            ),
            source: super::super::Source::Pin {
                sums_sha256: "e".repeat(64),
                allow_pinned: true,
                allow_artifact_origin: false,
                allow_same: true,
                allow_downgrade: false,
            },
            phase: Phase::Preparing,
            created_at: now,
            updated_at: now,
        };
        (
            PrivateFixture(root),
            Journal::create(&jobs, &receipt).unwrap(),
        )
    }

    #[test]
    fn original_baseline_round_trips_privately_without_credentials_paths_or_output() {
        use std::os::unix::fs::PermissionsExt;
        let (_fixture, journal) = journal();
        let response = sync();
        capture(&response)
            .persist(&journal, 0, "old-controller", &"a".repeat(40))
            .unwrap();
        let file = journal.directory.read_file("host-baseline.json").unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        let mut bytes = String::new();
        file.take(BASELINE_LIMIT)
            .read_to_string(&mut bytes)
            .unwrap();
        for forbidden in [
            "synthetic-secret",
            "synthetic-private-path",
            "synthetic-output",
            "workload_credential",
            "cwd",
        ] {
            assert!(!bytes.contains(forbidden));
        }
        journal.prepare_fixture().unwrap();
        journal.advance(1, Phase::Activating).unwrap();
        let (saved, epoch, source_sha) = HostSnapshot::load(&journal).unwrap();
        assert_eq!(epoch, "old-controller");
        assert_eq!(source_sha, "a".repeat(40));
        saved.verify(&response, &identity()).unwrap();
        let mut after = response.clone();
        inventory(&mut after).0[0].pid = Some(9999);
        assert!(saved.verify(&after, &identity()).is_err());
        assert!(capture(&response)
            .persist(&journal, 2, "new-controller", &"b".repeat(40))
            .is_err());
    }

    #[test]
    fn stored_baseline_rejects_duplicates_corruption_cross_job_and_invalid_witnesses() {
        let (fixture, journal) = journal();
        capture(&sync())
            .persist(&journal, 0, "old-controller", &"a".repeat(40))
            .unwrap();
        let path = fixture
            .0
            .join(journal.id.to_string())
            .join("host-baseline.json");
        let original: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for case in [
            "job",
            "workspace",
            "source",
            "host",
            "unknown",
            "duplicate",
            "generation",
            "scope",
            "pid",
            "witness",
            "stream",
            "epoch",
            "old-source",
            "missing-old-source",
            "old-digest",
            "old-schema",
        ] {
            let mut value = original.clone();
            match case {
                "job" => value["binding"]["job_id"] = uuid::Uuid::new_v4().to_string().into(),
                "workspace" => value["binding"]["workspace_id"] = "ws_other".into(),
                "source" => value["binding"]["controller_sha256"] = "0".repeat(64).into(),
                "host" => value["identity"]["pid"] = 0.into(),
                "unknown" => value["unexpected"] = true.into(),
                "duplicate" => {
                    let item = value["workloads"][0].clone();
                    value["workloads"].as_array_mut().unwrap().push(item);
                }
                "generation" => value["workloads"][0][1]["metadata"]["run_generation"] = 0.into(),
                "scope" => value["workloads"][0][1]["metadata"]["server_id"] = "srv_other".into(),
                "pid" => value["workloads"][0][1]["pid"] = serde_json::Value::Null,
                "witness" => value["workloads"][0][1]["witness"]["revision"] = u64::MAX.into(),
                "stream" => value["workloads"][0][1]["stream_epoch"] = "".into(),
                "epoch" => value["previous_controller_epoch"] = "\nsynthetic-secret".into(),
                "old-source" => {
                    value["previous_controller_source_sha"] = "bad\nsynthetic-secret".into()
                }
                "missing-old-source" => {
                    value
                        .as_object_mut()
                        .unwrap()
                        .remove("previous_controller_source_sha");
                }
                "old-digest" => {
                    value["binding"]["original_controller_sha256"] = "0".repeat(64).into()
                }
                "old-schema" => value["schema_version"] = 1.into(),
                _ => unreachable!(),
            }
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            let error = HostSnapshot::load(&journal).err().unwrap().to_string();
            assert!(!error.contains("synthetic-secret"));
        }
        std::fs::write(&path, b"{").unwrap();
        assert!(HostSnapshot::load(&journal).is_err());
    }

    #[test]
    fn invalid_old_build_does_not_create_a_baseline() {
        let (_fixture, journal) = journal();
        for source_sha in [
            "",
            "bad",
            &"a".repeat(39),
            &"a".repeat(41),
            "bad\nsynthetic-secret",
        ] {
            assert_eq!(
                capture(&sync())
                    .persist(&journal, 0, "old-controller", source_sha)
                    .unwrap_err()
                    .to_string(),
                "update_baseline_phase_conflict"
            );
            assert!(journal.directory.read_file("host-baseline.json").is_err());
        }
    }

    #[test]
    fn missing_partial_oversize_and_aliased_baselines_are_not_recaptured_or_overwritten() {
        use std::os::unix::fs::symlink;
        let (fixture, journal) = journal();
        assert!(HostSnapshot::load(&journal).is_err());
        let path = fixture
            .0
            .join(journal.id.to_string())
            .join("host-baseline.json");
        let file = journal.directory.create_file("host-baseline.json").unwrap();
        assert!(capture(&sync())
            .persist(&journal, 0, "old-controller", &"a".repeat(40))
            .is_err());
        assert_eq!(file.metadata().unwrap().len(), 0);
        file.set_len(BASELINE_LIMIT + 1).unwrap();
        assert_eq!(
            HostSnapshot::load(&journal).err().unwrap().to_string(),
            "update_baseline_too_large"
        );
        drop(file);
        std::fs::remove_file(&path).unwrap();
        symlink("receipt.json", &path).unwrap();
        assert!(HostSnapshot::load(&journal).is_err());
        assert!(capture(&sync())
            .persist(&journal, 0, "old-controller", &"a".repeat(40))
            .is_err());
    }

    fn identity() -> HostIdentity {
        HostIdentity {
            pid: 123,
            creation_identity: "os-start-identity".into(),
            executable_sha256: "a".repeat(64),
        }
    }

    fn sync() -> HostResponse {
        let now = DateTime::from_timestamp_millis(1_000).unwrap();
        let processes = [true, false].into_iter().enumerate().map(|(index, running)| {
            let id = format!("ag_fixture{index}");
            HostProcessInfo {
                process_id: id.clone(), pid: Some(4000 + index as u32), cwd: ".".into(), running,
                started_at: now, last_output_at: now, exited_at: (!running).then_some(now),
                exit_code: (!running).then_some(0), stream_epoch: format!("stream-{index}"),
                first_revision: 1, next_revision: 2, bracketed_paste: false,
                metadata: json!({"agent_id":id,"run_generation":3,"workspace_id":"ws_fixture",
                    "server_id":"srv_fixture","kind":if index == 0 {"command"} else {"app"},
                    "workload_credential":"synthetic-secret-not-retained", "cwd":"synthetic-private-path"}).to_string(),
            }
        }).collect::<Vec<_>>();
        let replay = processes
            .iter()
            .map(|process| HostOutputReplay {
                process_id: process.process_id.clone(),
                stream_epoch: process.stream_epoch.clone(),
                first_available_revision: 1,
                next_revision: 2,
                gap: false,
                chunks: vec![HostOutputChunk {
                    process_id: process.process_id.clone(),
                    stream_epoch: process.stream_epoch.clone(),
                    revision: 1,
                    data: b"synthetic-output".to_vec(),
                    bracketed_paste: false,
                    emitted_at: now,
                }],
            })
            .collect();
        HostResponse::Synced {
            host_epoch: "host-epoch".into(),
            host_build: HostBuildInfo {
                version: "0.1.14".into(),
                git_commit: "b".repeat(40),
            },
            processes,
            replay,
        }
    }

    fn inventory(
        response: &mut HostResponse,
    ) -> (&mut Vec<HostProcessInfo>, &mut Vec<HostOutputReplay>) {
        let HostResponse::Synced {
            processes, replay, ..
        } = response
        else {
            panic!("fixture sync required")
        };
        (processes, replay)
    }

    fn capture(response: &HostResponse) -> HostSnapshot {
        HostSnapshot::capture(response, identity(), "ws_fixture", "srv_fixture").unwrap()
    }

    #[test]
    fn keeps_live_and_stopped_identity_while_allowing_new_output() {
        let before = sync();
        let baseline = capture(&before);
        assert_eq!(baseline.cursors()["ag_fixture0"].revision, 0);
        let mut after = before.clone();
        let (processes, replay) = inventory(&mut after);
        let mut chunk = replay[0].chunks[0].clone();
        chunk.revision = 2;
        chunk.data = b"new output".to_vec();
        replay[0].chunks.push(chunk);
        replay[0].next_revision = 3;
        // list() preceded read(): the counters need not be simultaneous.
        assert_eq!(processes[0].next_revision, 2);
        baseline.verify(&after, &identity()).unwrap();
    }

    #[test]
    fn refuses_changed_host_os_identity_image_epoch_or_build() {
        let before = sync();
        let baseline = capture(&before);
        let mut changed = identity();
        changed.pid += 1;
        assert_eq!(
            baseline.verify(&before, &changed).unwrap_err().to_string(),
            "update_host_changed"
        );
        changed = identity();
        changed.creation_identity = "reused-pid-new-start".into();
        assert!(baseline.verify(&before, &changed).is_err());
        changed = identity();
        changed.executable_sha256 = "c".repeat(64);
        assert!(baseline.verify(&before, &changed).is_err());
        for field in ["epoch", "build"] {
            let mut after = before.clone();
            let HostResponse::Synced {
                host_epoch,
                host_build,
                ..
            } = &mut after
            else {
                unreachable!()
            };
            if field == "epoch" {
                *host_epoch = "new-host".into();
            } else {
                host_build.git_commit = "c".repeat(40);
            }
            assert_eq!(
                baseline
                    .verify(&after, &identity())
                    .unwrap_err()
                    .to_string(),
                "update_host_changed"
            );
        }
    }

    #[test]
    fn rejects_restarted_missing_resurrected_and_cross_enrollment_workloads() {
        let before = sync();
        let baseline = capture(&before);
        for case in [
            "pid",
            "generation",
            "running",
            "resurrected",
            "stream",
            "creation",
            "missing",
            "duplicate",
            "workspace",
        ] {
            let mut after = before.clone();
            let (processes, _) = inventory(&mut after);
            match case {
                "pid" => processes[0].pid = Some(5000),
                "generation" | "workspace" => {
                    let mut value: serde_json::Value =
                        serde_json::from_str(&processes[0].metadata).unwrap();
                    if case == "generation" {
                        value["run_generation"] = 4.into();
                    } else {
                        value["workspace_id"] = "ws_other".into();
                    }
                    processes[0].metadata = value.to_string();
                }
                "running" => processes[0].running = false,
                "resurrected" => processes[1].running = true,
                "stream" => processes[0].stream_epoch = "new-stream".into(),
                "creation" => processes[0].started_at += chrono::Duration::milliseconds(1),
                "missing" => {
                    processes.pop();
                }
                "duplicate" => {
                    processes[1] = processes[0].clone();
                }
                _ => unreachable!(),
            }
            assert_eq!(
                baseline
                    .verify(&after, &identity())
                    .unwrap_err()
                    .to_string(),
                "update_workload_changed",
                "{case}"
            );
        }
    }

    #[test]
    fn output_retention_must_prove_the_same_bytes_and_contiguous_revisions() {
        let before = sync();
        let baseline = capture(&before);
        for case in [
            "data",
            "empty",
            "gap",
            "wrong-stream",
            "duplicate",
            "tail-gap",
        ] {
            let mut after = before.clone();
            let (_, replay) = inventory(&mut after);
            match case {
                "data" => replay[0].chunks[0].data = b"forged synthetic bytes".to_vec(),
                "empty" => replay[0].chunks.clear(),
                "gap" => replay[0].gap = true,
                "wrong-stream" => replay[0].chunks[0].stream_epoch = "wrong".into(),
                "duplicate" => {
                    let chunk = replay[0].chunks[0].clone();
                    replay[0].chunks.push(chunk);
                }
                "tail-gap" => replay[0].next_revision = 3,
                _ => unreachable!(),
            }
            assert_eq!(
                baseline
                    .verify(&after, &identity())
                    .unwrap_err()
                    .to_string(),
                "update_output_continuity_unproven",
                "{case}"
            );
        }
    }

    #[test]
    fn legacy_or_wrong_enrollment_metadata_fails_without_echoing_private_fields() {
        for case in ["legacy", "wrong-workspace", "malformed"] {
            let mut before = sync();
            let (processes, _) = inventory(&mut before);
            let mut value: serde_json::Value =
                serde_json::from_str(&processes[0].metadata).unwrap();
            match case {
                "legacy" => {
                    value.as_object_mut().unwrap().remove("run_generation");
                }
                "wrong-workspace" => value["workspace_id"] = "ws_other".into(),
                _ => {}
            }
            processes[0].metadata = if case == "malformed" {
                "synthetic-secret-not-json".into()
            } else {
                value.to_string()
            };
            let error = HostSnapshot::capture(&before, identity(), "ws_fixture", "srv_fixture")
                .err()
                .unwrap()
                .to_string();
            assert!(matches!(
                error.as_str(),
                "update_host_inventory_unsupported" | "update_host_enrollment_mismatch"
            ));
        }
    }

    #[test]
    fn empty_output_requires_monotonic_bounds_and_a_matching_cursor() {
        let mut before = sync();
        let (processes, replay) = inventory(&mut before);
        for process in processes {
            process.next_revision = 1;
        }
        for item in replay {
            item.next_revision = 1;
            item.chunks.clear();
        }
        let baseline = capture(&before);
        assert_eq!(baseline.cursors()["ag_fixture0"].revision, 0);
        baseline.verify(&before, &identity()).unwrap();
        let (_, replay) = inventory(&mut before);
        replay[0].next_revision = 2;
        assert!(baseline.verify(&before, &identity()).is_err());
    }

    #[test]
    fn health_requires_new_epoch_proxy_source_commit_and_independent_image_digest() {
        let expectation = ControllerExpectation {
            workspace: "ws_fixture".into(),
            server: "srv_fixture".into(),
            previous_epoch: "previous".into(),
            artifact_source_sha: "a".repeat(40),
            executable_sha256: "b".repeat(64),
        };
        let health = json!({"service":"treer-agent-server","workspace_id":"ws_fixture","server_id":"srv_fixture",
            "controller_epoch":"new", "controller_build":{"version":"0.1.14","git_commit":"a".repeat(40)},"proxy_connected":true});
        assert_eq!(
            expectation
                .verify(&serde_json::to_vec(&health).unwrap(), &"b".repeat(64))
                .unwrap(),
            "new"
        );
        for (key, value) in [
            ("workspace_id", json!("ws_other")),
            ("server_id", json!("srv_other")),
            ("controller_epoch", json!("previous")),
            ("proxy_connected", json!(false)),
            (
                "controller_build",
                json!({"version":"0.1.14","git_commit":"c".repeat(40)}),
            ),
        ] {
            let mut changed = health.clone();
            changed[key] = value;
            assert_eq!(
                expectation
                    .verify(&serde_json::to_vec(&changed).unwrap(), &"b".repeat(64))
                    .unwrap_err()
                    .to_string(),
                "update_controller_health_mismatch"
            );
        }
        assert!(expectation
            .verify(&serde_json::to_vec(&health).unwrap(), &"c".repeat(64))
            .is_err());
        assert_eq!(
            expectation
                .verify(b"synthetic-secret-malformed", &"b".repeat(64))
                .unwrap_err()
                .to_string(),
            "update_health_unsupported"
        );
        assert_eq!(
            expectation
                .verify(&vec![b'x'; HEALTH_LIMIT + 1], &"b".repeat(64))
                .unwrap_err()
                .to_string(),
            "update_health_too_large"
        );
    }
}
