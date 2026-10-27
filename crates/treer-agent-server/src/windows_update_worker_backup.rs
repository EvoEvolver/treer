//! Backup-only mutation after an explicit command on the original Holding
//! connection. No installed-image/startup write, release, restart or replay.
use crate::windows_update_intent::Binding;
use anyhow::{anyhow, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};
use treer_platform::{
    windows_update_backup::{BackupDirectory, SealedBackup},
    windows_update_inputs::PinnedPair,
};
use treer_protocol::{
    controller_update_worker::WorkerBindingFrame, controller_update_worker_inputs::Snapshot,
};

fn refused() -> anyhow::Error {
    anyhow!("windows_update_worker_backup_unconfirmed")
}

/// Retained receiver-side backup handles. Original received images, state and
/// inherited installation lock stay in their SAME owner alongside this stage.
pub(super) struct WorkerBackup {
    backup: SealedBackup,
    startup: Option<File>,
    evidence: Vec<u8>,
}

pub(super) fn prepare(
    installed: &PinnedPair,
    target: &PinnedPair,
    state: &BackupDirectory,
    binding: &Binding,
    snapshot: &Option<Snapshot>,
    context: WorkerBindingFrame,
) -> Result<WorkerBackup> {
    // Fixed child of the original independently pinned operation target. Never
    // accept a destination path, repair/adopt an existing directory or retry.
    let path = target.directory_path().join("worker-backup");
    std::fs::create_dir(&path)?;
    treer_platform::secure_directory(&path)?;
    let directory = BackupDirectory::open(&path)?;
    let directory_id = directory.identity()?;
    if directory_id.0 != binding.installation_volume {
        return Err(refused());
    }
    let raw = state.startup_snapshot(&format!("{}-agent-startup.json", binding.server_id))?;
    super::check_snapshot(
        snapshot,
        raw.as_ref().map(|(id, bytes)| (*id, bytes.clone())),
        binding,
    )?;
    let mut writing = directory.create()?;
    let (mut old_c, mut old_cli) = installed.files();
    old_c.seek(SeekFrom::Start(0))?;
    old_cli.seek(SeekFrom::Start(0))?;
    let (controller, cli) = writing.files();
    super::super::backup::copy_checked(
        &mut old_c,
        controller,
        &binding.old_controller_sha256,
        128 * 1024 * 1024,
    )?;
    super::super::backup::copy_checked(
        &mut old_cli,
        cli,
        &binding.old_cli_sha256,
        128 * 1024 * 1024,
    )?;
    let backup = writing.seal()?;
    let startup = backup.create_startup(raw.as_ref().map(|(_, b)| b.as_slice()))?;
    let (c, l) = backup.files();
    // Safe opaque evidence binds effects and physical handles, not credentials
    // or a generic caller-provided "success" setter.
    let evidence = serde_json::to_vec(&serde_json::json!({
        "schema_version":1, "phase":"worker_backup_prepared", "binding":binding,
        "operation":context.operation, "challenge":context.challenge,
        "offer_sha256":context.context_sha256, "backup_directory":directory_id,
        "state_directory":state.identity()?, "snapshot":snapshot,
        "controller":treer_platform::transfer_file_identity(c)?,
        "cli":treer_platform::transfer_file_identity(l)?,
        "startup":startup.as_ref().map(treer_platform::transfer_file_identity).transpose()?,
    }))?;
    let prepared = WorkerBackup {
        backup,
        startup,
        evidence,
    };
    prepared.verify_bytes(binding, snapshot)?;
    prepared.backup.publish_evidence(&prepared.evidence)?;
    prepared.inspect(binding, snapshot)?;
    Ok(prepared)
}

impl WorkerBackup {
    /// Real fixed installed-directory candidates from already authenticated held
    /// inputs and the independent old backup. No rename or image-pin release.
    pub(super) fn stage_candidates(
        &self,
        installed: &PinnedPair,
        target: &PinnedPair,
        lock: &treer_platform::windows_update_inputs::InheritedInstallationLock,
        binding: &Binding,
        snapshot: &Option<Snapshot>,
    ) -> Result<treer_platform::windows_update_pair::WorkerReadyPair> {
        self.inspect(binding, snapshot)?;
        let mut staging = installed.stage_worker_pair(lock)?;
        let (mut next_c, mut next_l) = target.files();
        let (mut old_c, mut old_l) = self.backup.files();
        let [c, l, rc, rl] = staging.files();
        for (source, destination, expected) in [
            (&mut next_c, c, &binding.controller_sha256),
            (&mut next_l, l, &binding.cli_sha256),
            (&mut old_c, rc, &binding.old_controller_sha256),
            (&mut old_l, rl, &binding.old_cli_sha256),
        ] {
            source.seek(SeekFrom::Start(0))?;
            super::super::backup::copy_checked(source, destination, expected, 128 * 1024 * 1024)?;
        }
        let ready = staging.seal()?;
        Self::inspect_candidates(&ready, binding)?;
        Ok(ready)
    }

    pub(super) fn inspect_candidates(
        ready: &treer_platform::windows_update_pair::WorkerReadyPair,
        binding: &Binding,
    ) -> Result<()> {
        for (mut file, expected) in ready.files()?.into_iter().zip([
            &binding.controller_sha256,
            &binding.cli_sha256,
            &binding.old_controller_sha256,
            &binding.old_cli_sha256,
        ]) {
            file.seek(SeekFrom::Start(0))?;
            if super::super::digest(&mut file, None)? != *expected {
                return Err(refused());
            }
        }
        Ok(())
    }
    pub(super) fn preserves_fences(&self, next: Option<&[u8]>) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct Fences {
            #[serde(default)]
            denied_agent_ids: std::collections::BTreeSet<String>,
        }
        let fences = |raw: Option<&[u8]>| -> Result<std::collections::BTreeSet<String>> {
            Ok(match raw {
                Some(b) => serde_json::from_slice::<Fences>(b)?.denied_agent_ids,
                None => Default::default(),
            })
        };
        let mut original = Vec::new();
        if let Some(mut file) = self.startup.as_ref() {
            file.seek(SeekFrom::Start(0))?;
            file.take(16 * 1024 * 1024 + 1).read_to_end(&mut original)?;
        }
        let old = fences(self.startup.as_ref().map(|_| original.as_slice()))?;
        if !old.is_subset(&fences(next)?) {
            return Err(refused());
        }
        Ok(())
    }
    fn verify_bytes(&self, binding: &Binding, snapshot: &Option<Snapshot>) -> Result<()> {
        let (mut c, mut l) = self.backup.files();
        c.seek(SeekFrom::Start(0))?;
        l.seek(SeekFrom::Start(0))?;
        if super::super::digest(&mut c, None)? != binding.old_controller_sha256
            || super::super::digest(&mut l, None)? != binding.old_cli_sha256
        {
            return Err(refused());
        }
        match (self.startup.as_ref(), snapshot.as_ref()) {
            (Some(mut file), Some(expected)) => {
                file.seek(SeekFrom::Start(0))?;
                let mut raw = Vec::new();
                file.take(16 * 1024 * 1024 + 1).read_to_end(&mut raw)?;
                let digest = super::super::backup::startup_digest(
                    &raw,
                    &binding.workspace_id,
                    &binding.server_id,
                )?;
                if digest.sha256 != expected.sha256 || digest.size != expected.size {
                    return Err(refused());
                }
            }
            (None, None) if self.backup.startup_absent()? => {}
            _ => return Err(refused()),
        }
        Ok(())
    }
    pub(super) fn inspect(&self, binding: &Binding, snapshot: &Option<Snapshot>) -> Result<()> {
        self.verify_bytes(binding, snapshot)?;
        if self.backup.read_evidence()? != self.evidence {
            return Err(refused());
        }
        Ok(())
    }
}
