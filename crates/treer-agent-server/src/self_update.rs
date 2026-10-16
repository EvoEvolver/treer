//! Unix self-update transaction primitives. Not yet exposed as a CLI command.
//! The activation engine must hold installation/enrollment OS locks around all
//! journal transitions. Public receipts intentionally cannot contain paths,
//! credentials, arguments or arbitrary child-process diagnostics.
use std::io::{Read, Write};

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use treer_platform::private_directory::PrivateDirectory;
use uuid::Uuid;

const RECEIPT_LIMIT: u64 = 64 * 1024;

pub mod artifacts;
pub mod images;
pub mod publication;
pub mod safety;
pub mod selection;

use publication::{FileStep, Publications};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Prepared,
    Activating,
    CoreHealthy,
    LauncherInstalling,
    RollingBack,
    Complete,
    Failed,
    RolledBack,
    RollbackFailed,
    Partial,
}

impl Phase {
    /// A terminal execution result need not be safe for retention cleanup.
    pub fn reclaimable(self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::RolledBack)
    }

    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Complete | Self::Failed | Self::RolledBack | Self::RollbackFailed | Self::Partial
        )
    }

    fn permits(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Preparing, Self::Prepared | Self::Failed)
                | (Self::Prepared, Self::Activating | Self::Failed)
                | (Self::Activating, Self::CoreHealthy | Self::RollingBack)
                | (
                    Self::CoreHealthy,
                    Self::LauncherInstalling | Self::RollingBack | Self::Partial
                )
                | (Self::LauncherInstalling, Self::Complete | Self::Partial)
                | (Self::RollingBack, Self::RolledBack | Self::RollbackFailed)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// Authenticated metadata; freshness remains unavailable until P4.
    Signed {
        key_id: String,
        channel: String,
        accepted_sequence: Option<u64>,
        expires_at: Option<DateTime<Utc>>,
    },
    /// Explicit transition acknowledgment, never a publisher signature.
    Pin {
        sums_sha256: String,
        allow_pinned: bool,
        allow_artifact_origin: bool,
        allow_same: bool,
        allow_downgrade: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema_version: u32,
    pub job_id: Uuid,
    pub revision: u64,
    pub workspace_id: String,
    pub server_id: String,
    pub release_sha: String,
    pub artifact_source_sha: String,
    pub controller_sha256: String,
    pub cli_sha256: String,
    pub publications: Publications,
    pub source: Source,
    pub phase: Phase,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn identifier(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 128
            && rest.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
    })
}

impl Receipt {
    pub fn reconcile_file(
        &self,
        binary: artifacts::Binary,
        actual_sha256: Option<&str>,
    ) -> publication::Reconcile {
        if self.validate().is_err() {
            return publication::Reconcile::Conflict;
        }
        let target = match binary {
            artifacts::Binary::Controller => &self.controller_sha256,
            artifacts::Binary::Cli => &self.cli_sha256,
        };
        self.publications.reconcile(binary, target, actual_sha256)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.job_id.is_nil()
            || !identifier(&self.workspace_id, "ws_")
            || !identifier(&self.server_id, "srv_")
            || !hex(&self.release_sha, 40)
            || !hex(&self.artifact_source_sha, 40)
            || !hex(&self.controller_sha256, 64)
            || !hex(&self.cli_sha256, 64)
            || self.updated_at < self.created_at
            || (self.phase == Phase::Preparing) != (self.revision == 0)
        {
            bail!("invalid_update_receipt");
        }
        self.publications.validate(self)?;
        match &self.source {
            Source::Signed {
                key_id,
                channel,
                accepted_sequence,
                expires_at,
            } => {
                if !hex(key_id, 16)
                    || channel.is_empty()
                    || channel.len() > 64
                    || !channel.starts_with(|c: char| c.is_ascii_lowercase())
                    || !channel
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                    || accepted_sequence.is_some() != expires_at.is_some()
                {
                    bail!("invalid_update_receipt");
                }
            }
            Source::Pin {
                sums_sha256,
                allow_pinned,
                allow_same,
                allow_downgrade,
                ..
            } => {
                if !hex(sums_sha256, 64) || !allow_pinned || (!allow_same && !allow_downgrade) {
                    bail!("invalid_update_receipt");
                }
            }
        }
        Ok(())
    }
}

/// Only canonical hyphenated UUIDs are selectors, never filesystem paths.
pub fn job_id(value: &str) -> Result<Uuid> {
    let parsed = Uuid::parse_str(value).map_err(|_| anyhow::anyhow!("invalid_update_job_id"))?;
    if parsed.is_nil() || parsed.to_string() != value {
        bail!("invalid_update_job_id");
    }
    Ok(parsed)
}

/// Access is relative to a checked, retained private jobs directory handle.
pub struct Journal {
    directory: PrivateDirectory,
    id: Uuid,
}

impl Journal {
    pub fn create(jobs: &PrivateDirectory, receipt: &Receipt) -> Result<Self> {
        receipt.validate()?;
        if receipt.revision != 0 || receipt.phase != Phase::Preparing {
            bail!("invalid_initial_update_receipt");
        }
        let journal = Self {
            directory: jobs.create_child(&receipt.job_id.to_string())?,
            id: receipt.job_id,
        };
        journal.write(receipt)?;
        Ok(journal)
    }

    pub fn open(jobs: &PrivateDirectory, selector: &str) -> Result<Self> {
        let id = job_id(selector)?;
        let journal = Self {
            directory: jobs.open_child(&id.to_string())?,
            id,
        };
        journal.read()?;
        Ok(journal)
    }

    pub fn read(&self) -> Result<Receipt> {
        let file = self.directory.read_file("receipt.json")?;
        let mut bytes = Vec::new();
        file.take(RECEIPT_LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > RECEIPT_LIMIT {
            bail!("update_receipt_too_large");
        }
        let receipt: Receipt = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid_update_receipt"))?;
        receipt.validate()?;
        if receipt.job_id != self.id {
            bail!("update_job_mismatch");
        }
        Ok(receipt)
    }

    fn write(&self, receipt: &Receipt) -> Result<()> {
        receipt.validate()?;
        if receipt.job_id != self.id {
            bail!("update_job_mismatch");
        }
        let bytes = serde_json::to_vec(receipt)?;
        if bytes.len() as u64 > RECEIPT_LIMIT {
            bail!("update_receipt_too_large");
        }
        let temporary = format!("receipt-{}.tmp", Uuid::new_v4());
        let mut file = self.directory.create_file(&temporary)?;
        let result = (|| -> Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            self.directory.replace_file(&temporary, "receipt.json")?;
            Ok(())
        })();
        if result.is_err() {
            // This exact random name belongs to this write attempt. A failed
            // post-rename directory sync may mean it is already absent; never
            // delete the published receipt while reporting uncertain durability.
            let _ = self.directory.remove_file(&temporary);
        }
        result
    }

    /// The caller must hold installation then enrollment advisory locks. This
    /// expected revision check detects stale state; it is not a replacement lock.
    pub fn advance(&self, expected_revision: u64, phase: Phase) -> Result<Receipt> {
        let mut receipt = self.read()?;
        if receipt.revision != expected_revision || !receipt.phase.permits(phase) {
            bail!("update_revision_or_phase_conflict");
        }
        receipt.revision = receipt
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("update_revision_exhausted"))?;
        receipt.phase = phase;
        receipt.updated_at = Utc::now().max(receipt.updated_at);
        self.write(&receipt)?;
        Ok(receipt)
    }

    /// Persist an individual replacement intent/completion BEFORE/AFTER its OS
    /// rename respectively. The same installation/enrollment locks are required.
    /// Recovery reconciles actual image digests, never infers a completed rename
    /// from an intent or launches a process while publication is ambiguous.
    pub fn record_file(
        &self,
        expected_revision: u64,
        binary: artifacts::Binary,
        step: FileStep,
    ) -> Result<Receipt> {
        let mut receipt = self.read()?;
        if receipt.revision != expected_revision {
            bail!("update_revision_or_phase_conflict");
        }
        receipt
            .publications
            .transition(binary, receipt.phase, step)?;
        receipt.revision = receipt
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("update_revision_exhausted"))?;
        receipt.updated_at = Utc::now().max(receipt.updated_at);
        self.write(&receipt)?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> (Self, PrivateDirectory) {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("treer-update-journal-{}", Uuid::new_v4()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            let root = PrivateDirectory::open(&path).unwrap();
            (Self(path), root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn receipt() -> Receipt {
        Receipt {
            schema_version: 1,
            job_id: Uuid::new_v4(),
            revision: 0,
            workspace_id: "ws_fixture".into(),
            server_id: "srv_fixture".into(),
            release_sha: "a".repeat(40),
            artifact_source_sha: "b".repeat(40),
            controller_sha256: "c".repeat(64),
            cli_sha256: "d".repeat(64),
            publications: Publications::new(
                "f".repeat(64),
                "0".repeat(64),
                &"c".repeat(64),
                &"d".repeat(64),
            ),
            source: Source::Pin {
                sums_sha256: "e".repeat(64),
                allow_pinned: true,
                allow_artifact_origin: false,
                allow_same: true,
                allow_downgrade: false,
            },
            phase: Phase::Preparing,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn persistent_phases_reject_stale_revision_and_terminal_replay() {
        let (_f, root) = Fixture::new();
        let original = receipt();
        let journal = Journal::create(&root, &original).unwrap();
        assert!(Journal::create(&root, &original).is_err());
        assert!(journal.advance(0, Phase::Complete).is_err());
        journal.advance(0, Phase::Prepared).unwrap();
        assert!(journal.advance(0, Phase::Activating).is_err());
        drop(journal);
        let recovered = Journal::open(&root, &original.job_id.to_string()).unwrap();
        for (revision, phase) in [Phase::Activating, Phase::RollingBack, Phase::RolledBack]
            .into_iter()
            .enumerate()
        {
            recovered.advance(revision as u64 + 1, phase).unwrap();
        }
        assert!(recovered.read().unwrap().phase.terminal());
        assert!(recovered.advance(4, Phase::Activating).is_err());
    }

    #[test]
    fn selectors_and_receipt_fields_cannot_smuggle_paths_or_unknown_payloads() {
        let id = Uuid::new_v4();
        for selector in [
            "..".to_owned(),
            format!("../{id}"),
            id.simple().to_string(),
            "AAAAAAAA-AAAA-4AAA-AAAA-AAAAAAAAAAAA".to_owned(),
            Uuid::nil().to_string(),
        ] {
            assert!(job_id(&selector).is_err());
        }
        assert_eq!(job_id(&id.to_string()).unwrap(), id);
        let mut bad = receipt();
        bad.server_id = "srv_secret\nvalue".into();
        assert!(bad.validate().is_err());
        let mut json = serde_json::to_value(receipt()).unwrap();
        json["credential"] = "synthetic-never-return".into();
        assert!(serde_json::from_value::<Receipt>(json).is_err());
    }

    #[test]
    fn unsigned_mode_requires_ack_and_null_signed_sequence_is_not_zero() {
        let mut r = receipt();
        if let Source::Pin { allow_pinned, .. } = &mut r.source {
            *allow_pinned = false;
        }
        assert!(r.validate().is_err());
        r.source = Source::Signed {
            key_id: "a".repeat(16),
            channel: "canary".into(),
            accepted_sequence: None,
            expires_at: None,
        };
        r.validate().unwrap();
        let encoded = serde_json::to_value(&r).unwrap();
        assert!(encoded["source"]["accepted_sequence"].is_null());
        if let Source::Signed {
            accepted_sequence, ..
        } = &mut r.source
        {
            *accepted_sequence = Some(0);
        }
        assert!(r.validate().is_err());
    }

    #[test]
    fn unresolved_terminal_results_are_never_retention_cleanup_candidates() {
        for phase in [Phase::Partial, Phase::RollbackFailed] {
            assert!(phase.terminal());
            assert!(!phase.reclaimable());
        }
        assert!(!Phase::RollingBack.reclaimable());
        assert!(Phase::Complete.reclaimable());
    }

    #[test]
    fn corrupt_oversize_or_wrong_job_receipts_fail_closed() {
        let (f, root) = Fixture::new();
        let initial = receipt();
        let journal = Journal::create(&root, &initial).unwrap();
        let path = f.0.join(initial.job_id.to_string()).join("receipt.json");
        std::fs::write(&path, vec![b'x'; RECEIPT_LIMIT as usize + 1]).unwrap();
        assert_eq!(
            journal.read().unwrap_err().to_string(),
            "update_receipt_too_large"
        );
        std::fs::write(&path, b"not json synthetic private value").unwrap();
        assert_eq!(
            journal.read().unwrap_err().to_string(),
            "invalid_update_receipt"
        );
        let mut wrong = initial;
        wrong.job_id = Uuid::new_v4();
        std::fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert_eq!(
            journal.read().unwrap_err().to_string(),
            "update_job_mismatch"
        );
    }

    #[test]
    fn durable_per_file_intents_reconcile_crashes_before_and_after_atomic_renames() {
        use artifacts::Binary;
        use publication::{FileStep, Reconcile};
        use sha2::{Digest, Sha256};
        let hash = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
        let (fixture, root) = Fixture::new();
        let mut initial = receipt();
        initial.controller_sha256 = hash(b"new-controller");
        initial.cli_sha256 = hash(b"new-cli");
        initial.publications = Publications::new(
            hash(b"old-controller"),
            hash(b"old-cli"),
            &initial.controller_sha256,
            &initial.cli_sha256,
        );
        for (name, bytes) in [
            ("installed", b"old-controller".as_slice()),
            ("stage", b"new-controller"),
            ("backup", b"old-controller"),
        ] {
            let mut file = root.create_file(name).unwrap();
            file.write_all(bytes).unwrap();
            file.sync_all().unwrap();
        }
        let journal = Journal::create(&root, &initial).unwrap();
        journal.advance(0, Phase::Prepared).unwrap();
        journal.advance(1, Phase::Activating).unwrap();
        let observed = || hash(&std::fs::read(fixture.0.join("installed")).unwrap());
        assert_eq!(
            journal
                .read()
                .unwrap()
                .reconcile_file(Binary::Controller, Some(&observed())),
            Reconcile::NoMutation
        );
        assert!(journal
            .record_file(2, Binary::Controller, FileStep::Installed)
            .is_err());
        journal
            .record_file(2, Binary::Controller, FileStep::InstallIntent)
            .unwrap();
        drop(journal);
        let journal = Journal::open(&root, &initial.job_id.to_string()).unwrap();
        assert_eq!(
            journal
                .read()
                .unwrap()
                .reconcile_file(Binary::Controller, Some(&observed())),
            Reconcile::InstallVerified
        );
        // Simulate a crash after the actual Unix atomic rename, before the
        // completion receipt. The fixture is private, never an installed binary.
        std::fs::rename(fixture.0.join("stage"), fixture.0.join("installed")).unwrap();
        drop(journal);
        let journal = Journal::open(&root, &initial.job_id.to_string()).unwrap();
        assert_eq!(
            journal
                .read()
                .unwrap()
                .reconcile_file(Binary::Controller, Some(&observed())),
            Reconcile::RecordInstalled
        );
        journal
            .record_file(3, Binary::Controller, FileStep::Installed)
            .unwrap();
        assert!(journal
            .record_file(3, Binary::Controller, FileStep::Installed)
            .is_err());
        assert!(
            journal.advance(4, Phase::CoreHealthy).is_err(),
            "CLI was not installed"
        );
        assert_eq!(
            journal.read().unwrap().revision,
            4,
            "failed validation must preserve the old receipt"
        );
        journal.advance(4, Phase::RollingBack).unwrap();
        journal
            .record_file(5, Binary::Controller, FileStep::RestoreIntent)
            .unwrap();
        assert_eq!(
            journal
                .read()
                .unwrap()
                .reconcile_file(Binary::Controller, Some(&observed())),
            Reconcile::RestoreVerified
        );
        std::fs::rename(fixture.0.join("backup"), fixture.0.join("installed")).unwrap();
        drop(journal);
        let journal = Journal::open(&root, &initial.job_id.to_string()).unwrap();
        assert_eq!(
            journal
                .read()
                .unwrap()
                .reconcile_file(Binary::Controller, Some(&observed())),
            Reconcile::RecordRestored
        );
        journal
            .record_file(6, Binary::Controller, FileStep::Restored)
            .unwrap();
        let restored = journal.advance(7, Phase::RolledBack).unwrap();
        assert_eq!(
            restored.reconcile_file(Binary::Controller, Some(&observed())),
            Reconcile::NoMutation
        );
        assert!(journal
            .record_file(8, Binary::Controller, FileStep::InstallIntent)
            .is_err());
        assert!(journal.advance(8, Phase::Activating).is_err());
    }

    #[test]
    fn missing_unknown_and_same_image_publications_cannot_guess_a_rename_result() {
        use artifacts::Binary;
        use publication::{FileStep, Reconcile};
        let mut r = receipt();
        assert_eq!(
            r.reconcile_file(Binary::Controller, None),
            Reconcile::Conflict
        );
        assert_eq!(
            r.reconcile_file(Binary::Controller, Some(&"e".repeat(64))),
            Reconcile::Conflict
        );
        assert_eq!(
            r.reconcile_file(Binary::Controller, Some(&r.controller_sha256)),
            Reconcile::Conflict
        );
        r.publications = Publications::new(
            r.controller_sha256.clone(),
            r.cli_sha256.clone(),
            &r.controller_sha256,
            &r.cli_sha256,
        );
        r.validate().unwrap();
        assert_eq!(r.publications.step(Binary::Controller), FileStep::Unchanged);
        assert_eq!(
            r.reconcile_file(Binary::Controller, Some(&r.controller_sha256)),
            Reconcile::NoMutation
        );
        let (_fixture, root) = Fixture::new();
        let journal = Journal::create(&root, &r).unwrap();
        journal.advance(0, Phase::Prepared).unwrap();
        journal.advance(1, Phase::Activating).unwrap();
        assert!(journal
            .record_file(2, Binary::Controller, FileStep::InstallIntent)
            .is_err());
        journal.advance(2, Phase::CoreHealthy).unwrap();
        journal.advance(3, Phase::LauncherInstalling).unwrap();
        journal.advance(4, Phase::Complete).unwrap();
        assert!(journal
            .record_file(5, Binary::Controller, FileStep::RestoreIntent)
            .is_err());
    }
}
