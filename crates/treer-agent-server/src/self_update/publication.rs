//! Per-image durable rename progress. Only fixed enum labels and image digests
//! enter the public receipt; destinations/backups belong to checked private
//! transaction metadata, never an arbitrary recovery path from CLI input.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::{artifacts::Binary, hex, Phase, Receipt};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStep {
    Original,
    Unchanged,
    InstallIntent,
    Installed,
    RestoreIntent,
    Restored,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    original_sha256: String,
    step: FileStep,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publications {
    controller: Publication,
    cli: Publication,
}

/// A reconciliation decision is NOT permission to rename. The engine first
/// checks the retained destination/backup handles and hashes under both locks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconcile {
    NoMutation,
    InstallVerified,
    RecordInstalled,
    RestoreVerified,
    RecordRestored,
    Conflict,
}

impl Publications {
    pub fn new(
        controller_old: String,
        cli_old: String,
        controller_new: &str,
        cli_new: &str,
    ) -> Self {
        fn image(old: String, new: &str) -> Publication {
            let step = if old == new {
                FileStep::Unchanged
            } else {
                FileStep::Original
            };
            Publication {
                original_sha256: old,
                step,
            }
        }
        Self {
            controller: image(controller_old, controller_new),
            cli: image(cli_old, cli_new),
        }
    }

    pub fn step(&self, binary: Binary) -> FileStep {
        self.image(binary).step
    }

    pub(super) fn original_digest(&self, binary: Binary) -> &str {
        &self.image(binary).original_sha256
    }

    fn image(&self, binary: Binary) -> &Publication {
        match binary {
            Binary::Controller => &self.controller,
            Binary::Cli => &self.cli,
        }
    }

    pub(super) fn validate(&self, receipt: &Receipt) -> Result<()> {
        for (image, next) in [
            (&self.controller, &receipt.controller_sha256),
            (&self.cli, &receipt.cli_sha256),
        ] {
            if !hex(&image.original_sha256, 64)
                || (image.original_sha256 == *next) != (image.step == FileStep::Unchanged)
            {
                bail!("invalid_update_publication");
            }
            let valid = match receipt.phase {
                Phase::Preparing | Phase::Prepared | Phase::Failed => {
                    matches!(image.step, FileStep::Original | FileStep::Unchanged)
                }
                Phase::Activating => matches!(
                    image.step,
                    FileStep::Original
                        | FileStep::Unchanged
                        | FileStep::InstallIntent
                        | FileStep::Installed
                ),
                Phase::CoreHealthy
                | Phase::LauncherInstalling
                | Phase::Complete
                | Phase::Partial => matches!(image.step, FileStep::Installed | FileStep::Unchanged),
                Phase::RollingBack | Phase::RollbackFailed => true,
                Phase::RolledBack => matches!(
                    image.step,
                    FileStep::Original | FileStep::Unchanged | FileStep::Restored
                ),
            };
            if !valid {
                bail!("invalid_update_publication_phase");
            }
        }
        Ok(())
    }

    pub(super) fn transition(
        &mut self,
        binary: Binary,
        phase: Phase,
        next: FileStep,
    ) -> Result<()> {
        let image = match binary {
            Binary::Controller => &mut self.controller,
            Binary::Cli => &mut self.cli,
        };
        let permitted = match phase {
            Phase::Activating => matches!(
                (image.step, next),
                (FileStep::Original, FileStep::InstallIntent)
                    | (FileStep::InstallIntent, FileStep::Installed)
            ),
            Phase::RollingBack => matches!(
                (image.step, next),
                (
                    FileStep::InstallIntent | FileStep::Installed,
                    FileStep::RestoreIntent
                ) | (FileStep::RestoreIntent, FileStep::Restored)
            ),
            _ => false,
        };
        if !permitted {
            bail!("update_file_step_conflict");
        }
        image.step = next;
        Ok(())
    }

    /// Unix replacement is atomic, so a missing or unrecognized destination
    /// is never explained away as a transient rename gap. Same-image updates
    /// skip replacement entirely, avoiding ambiguous old/new digest equality.
    pub(super) fn reconcile(
        &self,
        binary: Binary,
        target_sha256: &str,
        actual_sha256: Option<&str>,
    ) -> Reconcile {
        let image = self.image(binary);
        if !hex(target_sha256, 64) || actual_sha256.is_none_or(|hash| !hex(hash, 64)) {
            return Reconcile::Conflict;
        }
        let old = actual_sha256 == Some(image.original_sha256.as_str());
        let new = actual_sha256 == Some(target_sha256);
        match (image.step, old, new) {
            (FileStep::Original | FileStep::Restored, true, false)
            | (FileStep::Installed, false, true)
            | (FileStep::Unchanged, true, true) => Reconcile::NoMutation,
            (FileStep::InstallIntent, true, false) => Reconcile::InstallVerified,
            (FileStep::InstallIntent, false, true) => Reconcile::RecordInstalled,
            (FileStep::RestoreIntent, false, true) => Reconcile::RestoreVerified,
            (FileStep::RestoreIntent, true, false) => Reconcile::RecordRestored,
            _ => Reconcile::Conflict,
        }
    }
}
