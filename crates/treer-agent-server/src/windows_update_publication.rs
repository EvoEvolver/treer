//! File effects after signed admission and both backups; never worker/health
//! success. All error paths retain the installation claim and partial names.
use super::super::{AdmittedInputs, ReservedInputs};
use super::{copy_checked, BackedUpInputs, StartupBackedUpInputs};
use crate::windows_update_intent::Binding;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Seek, SeekFrom},
};
use treer_platform::{
    windows_update_backup::SealedBackup,
    windows_update_inputs::PinnedPair,
    windows_update_pair::{PairFiles, PairState},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilePairPhase {
    FilesPublished,
    FilesRestored,
    PublicationRefused,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Evidence {
    schema_version: u32,
    phase: FilePairPhase,
    binding: Binding,
    file_ids: [(u64, u64); 2],
}

/// Holds target, backup, startup and current-image pins plus the installation
/// lease. No running Controller/Host ownership or safe claim-retirement claim.
pub struct PublishedInputs {
    binding: Binding,
    pair: PairFiles,
    _target: PinnedPair,
    backup: SealedBackup,
    _startup: Option<File>,
    _state: Option<crate::controller::ControllerUpdateState>,
    restoration_record: bool,
}

fn verify_file(mut file: &File, expected: &str) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    if super::super::digest(&mut file, None)? != expected {
        bail!("windows_update_pair_digest_refused");
    }
    Ok(())
}

impl StartupBackedUpInputs {
    pub fn publish_pair(self) -> Result<PublishedInputs> {
        self.inspect_startup()?;
        let Self {
            images, startup, ..
        } = self;
        let BackedUpInputs { reserved, backup } = images;
        backup.require_pair_evidence_absent(false)?;
        backup.require_pair_evidence_absent(true)?;
        let ReservedInputs(AdmittedInputs {
            binding,
            _installed,
            _target,
            state,
            worker_attempted,
        }) = reserved;
        if worker_attempted {
            bail!("windows_update_worker_already_attempted");
        }
        let mut staging = _installed.stage_pair()?;
        let (mut target_c, mut target_l) = _target.files();
        let (mut old_c, mut old_l) = backup.files();
        let [c, l, rc, rl] = staging.files();
        for (source, target, expected) in [
            (&mut target_c, c, &binding.controller_sha256),
            (&mut target_l, l, &binding.cli_sha256),
            (&mut old_c, rc, &binding.old_controller_sha256),
            (&mut old_l, rl, &binding.old_cli_sha256),
        ] {
            source.seek(SeekFrom::Start(0))?;
            copy_checked(source, target, expected, 128 * 1024 * 1024)?;
        }
        let ready = staging.seal()?;
        for (file, expected) in ready.files().into_iter().zip([
            &binding.controller_sha256,
            &binding.cli_sha256,
            &binding.old_controller_sha256,
            &binding.old_cli_sha256,
        ]) {
            verify_file(file, expected)?;
        }
        let published = PublishedInputs {
            binding,
            pair: ready.publish()?,
            _target,
            backup,
            _startup: startup,
            _state: state,
            restoration_record: false,
        };
        published.verify_current()?;
        published
            .backup
            .publish_pair_evidence(&serde_json::to_vec(&published.evidence()?)?, false)?;
        published.inspect_pair()?;
        Ok(published)
    }
}

impl PublishedInputs {
    pub fn phase(&self) -> FilePairPhase {
        match self.pair.state() {
            PairState::Published => FilePairPhase::FilesPublished,
            PairState::Restored => FilePairPhase::FilesRestored,
            PairState::Refused => FilePairPhase::PublicationRefused,
        }
    }
    pub fn binding(&self) -> &Binding {
        &self.binding
    }

    fn verify_current(&self) -> Result<()> {
        self.pair.identities()?;
        let (controller, cli) = self.pair.files();
        let hashes = if self.pair.state() == PairState::Published {
            [&self.binding.controller_sha256, &self.binding.cli_sha256]
        } else {
            [
                &self.binding.old_controller_sha256,
                &self.binding.old_cli_sha256,
            ]
        };
        verify_file(controller, hashes[0])?;
        verify_file(cli, hashes[1])
    }

    fn evidence(&self) -> Result<Evidence> {
        Ok(Evidence {
            schema_version: 1,
            phase: self.phase(),
            binding: self.binding.clone(),
            file_ids: self.pair.identities()?,
        })
    }

    pub fn inspect_pair(&self) -> Result<()> {
        self.verify_current()?;
        let evidence: Evidence =
            serde_json::from_slice(&self.backup.read_pair_evidence(self.restoration_record)?)
                .map_err(|_| anyhow::anyhow!("windows_update_pair_evidence_refused"))?;
        if evidence != self.evidence()? {
            bail!("windows_update_pair_evidence_conflict");
        }
        Ok(())
    }

    /// Explicit one-shot file restoration. Existing publication evidence stays
    /// historical; only verified restored bytes permit a new restoration record.
    /// Not a successful runtime rollback or permission to replay any job.
    pub fn restore_files(self) -> Result<Self> {
        self.inspect_pair()?;
        self.backup.require_pair_evidence_absent(true)?;
        let Self {
            binding,
            pair,
            _target,
            backup,
            _startup,
            _state,
            ..
        } = self;
        let restored = Self {
            binding,
            pair: pair.restore_files()?,
            _target,
            backup,
            _startup,
            _state,
            restoration_record: true,
        };
        restored.verify_current()?;
        restored
            .backup
            .publish_pair_evidence(&serde_json::to_vec(&restored.evidence()?)?, true)?;
        restored.inspect_pair()?;
        Ok(restored)
    }
}
