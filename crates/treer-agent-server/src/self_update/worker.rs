//! Pre-runtime resource resolution for the future private worker entry.
//! No ordinary Controller fallback, new locks, process spawn or path argv API.
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use treer_platform::private_directory::PrivateDirectory;

use super::{
    activator::RunningActivator,
    handoff::{OwnedActivation, WorkerBootstrap},
    job_id,
    selection::{Inventory, Selected},
    Journal, Phase, Receipt,
};

mod core;
pub(in crate::self_update) mod launcher;
pub use core::{CoreProgress, CoreRun};

/// This only locates existing resources; it is not ownership or authority to
/// mutate installed files. Fields intentionally private and not Debug/Serialize.
pub struct WorkerResources {
    selected: Selected,
    journal: Journal,
    prepared: Receipt,
    job_directory: PathBuf,
}

/// Only obtainable after synchronous running-image/session/parent/peer proof
/// and the durable, one-shot transfer of the parent's SAME lock descriptions.
#[must_use = "retain the owned worker and handle its progress before exiting"]
pub struct OwnedWorker {
    selected: Selected,
    journal: Journal,
    owned: OwnedActivation,
    core_started: bool,
    progress: CoreProgress,
}

impl WorkerResources {
    /// Only a canonical job UUID is supplied by argv. Root comes from the local
    /// service state resolver, and enrollment comes from the bound receipt.
    /// No filesystem changes or ambient hostname executable are used here.
    pub fn open(selector: &str) -> Result<Self> {
        // Reject invalid selectors before resolving any real local paths.
        job_id(selector)?;
        let root = crate::service::self_update_jobs_directory()?;
        Self::open_at(&root, selector, Inventory::local)
    }

    fn open_at(
        root: &Path,
        selector: &str,
        inventory: impl FnOnce() -> Result<Inventory>,
    ) -> Result<Self> {
        let id = job_id(selector)?;
        let jobs = PrivateDirectory::open(root)
            .map_err(|_| anyhow::anyhow!("update_worker_job_unavailable"))?;
        let journal = Journal::open(&jobs, selector)
            .map_err(|_| anyhow::anyhow!("update_worker_job_unavailable"))?;
        let prepared = journal.read()?;
        if prepared.phase != Phase::Prepared {
            bail!("update_worker_phase_conflict");
        }
        // No new shared-binary consent is taken here and no locks acquired.
        // Parent preparation already requires consent; execution still requires
        // its authenticated full-receipt handoff and exact installation locks.
        let selected = inventory()?.select(Some(&prepared.workspace_id), true)?;
        if selected.server_id() != prepared.server_id || journal.read()? != prepared {
            bail!("update_worker_binding_mismatch");
        }
        Ok(Self {
            selected,
            journal,
            prepared,
            job_directory: root.join(id.to_string()),
        })
    }

    /// Call BEFORE runtime threads/forks. Any failure is terminal for this
    /// startup: no reconnect, ownership-record adoption or normal run fallback.
    pub fn enter(self) -> Result<OwnedWorker> {
        let result = (|| -> Result<OwnedWorker> {
            if self.journal.read()? != self.prepared {
                bail!("update_worker_binding_mismatch");
            }
            let running = RunningActivator::enter(&self.selected, &self.journal)?;
            #[cfg(test)]
            tests::bootstrap_checkpoint("running");
            let bootstrap = WorkerBootstrap::connect(
                &running,
                &self.journal,
                &self.selected,
                &self.job_directory,
            )?;
            #[cfg(test)]
            tests::bootstrap_checkpoint("authenticated");
            let owned = bootstrap.take_ownership(&self.journal, &self.selected)?;
            Ok(OwnedWorker {
                selected: self.selected,
                journal: self.journal,
                owned,
                core_started: false,
                progress: CoreProgress::RecoveryRequired,
            })
        })();
        // Never expose OS errors, paths or secret configuration projections.
        result.map_err(|_| anyhow::anyhow!("update_worker_bootstrap_failed"))
    }
}

impl OwnedWorker {
    pub(super) async fn verify_web_admission(&self) -> Result<()> {
        super::web_admission::verify(&self.journal, &self.selected, self.owned.locks()).await
    }

    /// Recovery is the default after ownership until a complete stage proves
    /// otherwise. Keep this owner alive; status alone cannot release its locks.
    pub fn progress(&self) -> CoreProgress {
        self.progress
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{selection::tests::Fixture, tests::receipt};
    use std::cell::Cell;

    fn fixture(workspace: &str, server: &str) -> (Fixture, Journal) {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        f.enrollment("srv_b", "ws_b", &f.bin.join("treer-agent-server"));
        let jobs = PrivateDirectory::open(&f.path)
            .unwrap()
            .create_child("jobs")
            .unwrap();
        let mut receipt = receipt();
        receipt.workspace_id = workspace.into();
        receipt.server_id = server.into();
        let journal = Journal::create(&jobs, &receipt).unwrap();
        (f, journal)
    }

    #[test]
    fn receipt_selects_exact_workspace_without_acquiring_new_locks_or_mutating_files() {
        let (f, journal) = fixture("ws_b", "srv_b");
        let before = journal.prepare_fixture().unwrap();
        let resources =
            WorkerResources::open_at(&f.path.join("jobs"), &before.job_id.to_string(), || {
                Ok(f.inventory())
            })
            .unwrap();
        assert_eq!(resources.selected.workspace(), "ws_b");
        assert_eq!(resources.selected.server_id(), "srv_b");
        assert_eq!(resources.selected.shared_server_ids(), &["srv_a"]);
        assert_eq!(journal.read().unwrap(), before);
        assert_eq!(
            std::fs::read(f.bin.join("treer-agent-server")).unwrap(),
            b"controller"
        );
        // Resolution is read-only and must not compete with the submitting
        // parent's still-held locks before descriptor transfer.
        let locks = resources.selected.lock().unwrap();
        assert_eq!(
            resources.enter().err().unwrap().to_string(),
            "update_worker_bootstrap_failed"
        );
        assert_eq!(journal.read().unwrap(), before);
        assert!(journal.directory.read_file("ownership.json").is_err());
        assert!(!f
            .path
            .join("jobs")
            .join(before.job_id.to_string())
            .join("handoff.sock")
            .exists());
        drop(locks);
    }

    #[test]
    fn shared_resource_discovery_cannot_bypass_parent_locks_or_grant_ownership() {
        let (f, journal) = fixture("ws_b", "srv_b");
        let before = journal.prepare_fixture().unwrap();
        let inventory = f.inventory();
        assert!(inventory.select(Some("ws_b"), false).is_err());
        let parent = inventory.select(Some("ws_b"), true).unwrap();
        let held = parent.lock().unwrap();
        let resources =
            WorkerResources::open_at(&f.path.join("jobs"), &before.job_id.to_string(), || {
                Ok(f.inventory())
            })
            .unwrap();
        // Discovery succeeds while the submitting parent holds the locks;
        // neither another lock acquisition nor worker ownership follows from it.
        assert!(resources.selected.lock().is_err());
        assert_eq!(
            resources.enter().err().unwrap().to_string(),
            "update_worker_bootstrap_failed"
        );
        assert!(parent.lock().is_err());
        assert!(journal.directory.read_file("ownership.json").is_err());
        assert_eq!(journal.read().unwrap(), before);
        assert_eq!(
            std::fs::read(f.bin.join("treer-agent-server")).unwrap(),
            b"controller"
        );
        drop(held);
        assert!(parent.lock().is_ok());
    }

    #[test]
    fn invalid_missing_or_unprepared_job_is_rejected_before_inventory() {
        let (f, journal) = fixture("ws_a", "srv_a");
        let visited = Cell::new(false);
        for selector in [
            "../private",
            "/tmp/receipt.json",
            "bad",
            &uuid::Uuid::new_v4().to_string(),
            &journal.id.to_string(),
        ] {
            assert!(
                WorkerResources::open_at(&f.path.join("jobs"), selector, || {
                    visited.set(true);
                    Ok(f.inventory())
                })
                .is_err()
            );
            assert!(!visited.get());
        }
        assert_eq!(journal.read().unwrap().phase, Phase::Preparing);
    }

    #[test]
    fn mismatched_server_and_aliased_jobs_root_are_rejected() {
        let (f, journal) = fixture("ws_a", "srv_b");
        let prepared = journal.prepare_fixture().unwrap();
        assert_eq!(
            WorkerResources::open_at(&f.path.join("jobs"), &prepared.job_id.to_string(), || Ok(
                f.inventory()
            ))
            .err()
            .unwrap()
            .to_string(),
            "update_worker_binding_mismatch"
        );
        std::os::unix::fs::symlink(f.path.join("jobs"), f.path.join("alias")).unwrap();
        assert_eq!(
            WorkerResources::open_at(
                &f.path.join("alias"),
                &prepared.job_id.to_string(),
                || panic!("must not enumerate for an alias")
            )
            .err()
            .unwrap()
            .to_string(),
            "update_worker_job_unavailable"
        );
        assert_eq!(journal.read().unwrap(), prepared);
    }
    include!("worker/death_tests.rs");
}
