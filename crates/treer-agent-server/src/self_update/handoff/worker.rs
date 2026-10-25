//! Synchronous worker bootstrap before a runtime or subprocess can start.
//! No caller-provided parent PID/image, raw socket accessor or reconnect path.
use super::*;
use crate::self_update::{activator::RunningActivator, artifacts::Binary};

/// Constructible only after running-image, parent-record and exact connected
/// peer checks. Not Debug/Serialize: neither process paths nor a raw stream
/// should escape into diagnostics or an asynchronous worker task.
pub struct WorkerBootstrap {
    peer: AuthenticatedPeer,
    prepared: Receipt,
}

impl WorkerBootstrap {
    /// Invoke synchronously after RunningActivator::enter, before runtime
    /// threads/forks (Darwin CLOEXEC requirements). The caller must terminate
    /// startup on error, not retry or fall through to ordinary Controller run.
    /// No bytes are sent here, even before the peer has authenticated us.
    pub fn connect(
        running: &RunningActivator,
        journal: &Journal,
        selected: &Selected,
        job_directory: &Path,
    ) -> Result<Self> {
        running.verify(selected, journal)?;
        let prepared = journal.read()?;
        let parent = parent::load(journal, selected)?;
        let stream = journal
            .directory
            .connect_handoff(job_directory)
            .map_err(|_| anyhow::anyhow!("update_handoff_endpoint_unavailable"))?;
        let peer = AuthenticatedPeer::authenticate(
            stream,
            &parent,
            parent.executable(),
            prepared.publications.original_digest(Binary::Controller),
        )?;
        if journal.read()? != prepared {
            bail!("update_handoff_job_changed");
        }
        Ok(Self { peer, prepared })
    }

    /// Consumes the only authenticated stream. Any error closes it, including
    /// uncertain ACK errors; never reconnect/resend/adopt an existing record.
    /// Only the returned value permits starting the asynchronous activation
    /// engine while retaining the transferred installation/enrollment locks.
    pub fn take_ownership(self, journal: &Journal, selected: &Selected) -> Result<OwnedActivation> {
        if journal.read()? != self.prepared {
            bail!("update_handoff_job_changed");
        }
        self.peer.take_ownership(journal, selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{selection::tests::Fixture, tests::receipt};
    use std::{io::Read, time::Duration};
    use treer_platform::private_directory::PrivateDirectory;

    #[test]
    fn changed_receipt_closes_bootstrap_without_sending_or_recording_ownership() {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        let selected = f.inventory().select(None, false).unwrap();
        let jobs = PrivateDirectory::open(&f.path)
            .unwrap()
            .create_child("jobs")
            .unwrap();
        let journal = Journal::create(&jobs, &receipt()).unwrap();
        let prepared = journal.prepare_fixture().unwrap();
        let mut stale = prepared.clone();
        stale.updated_at += chrono::Duration::seconds(1);
        let (stream, mut other) = UnixStream::pair().unwrap();
        other
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        // Fabricate an already-authenticated stream only in this local test:
        // the early receipt guard must close it without attempting any frame.
        let bootstrap = WorkerBootstrap {
            peer: AuthenticatedPeer {
                stream,
                image: ProcessImage::capture(std::process::id()).unwrap(),
                launcher_read_started: false,
            },
            prepared: stale,
        };
        assert_eq!(
            bootstrap
                .take_ownership(&journal, &selected)
                .err()
                .unwrap()
                .to_string(),
            "update_handoff_job_changed"
        );
        assert_eq!(other.read(&mut [0]).unwrap(), 0);
        assert_eq!(journal.read().unwrap(), prepared);
        assert!(!f
            .path
            .join("jobs")
            .join(prepared.job_id.to_string())
            .join("ownership.json")
            .exists());
    }
}
