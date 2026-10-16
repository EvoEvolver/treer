//! One-shot job-bound handoff. No mutation of installed images or Controller
//! restart occurs here. A lost ACK is uncertain, never permission to resend.
use std::{
    io::{Read, Write},
    os::fd::AsFd,
    time::{Duration, Instant},
};

use anyhow::{bail, Result};
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use super::*;
use crate::self_update::artifacts::Binary;

const LIMIT: usize = 96 * 1024;
const TIMEOUT: Duration = Duration::from_secs(3);
const RECORD: &str = "ownership.json";

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    schema_version: u32,
    prepared: Receipt,
    pid: u32,
    creation: String,
}

/// Only returned after the ACK matches the durable record and live owned child.
/// The caller may now drop its original locks. It must not submit a second job
/// on error: even a failed ACK read may follow a durable child-side takeover.
pub struct AcceptedOwnership {
    prepared: Receipt,
}

/// Keeps the transferred open file descriptions alive for the entire activation.
/// This value is not a claim that a Controller restart has happened or succeeded.
pub struct OwnedActivation {
    locks: SelectionLocks,
    prepared: Receipt,
}

impl AcceptedOwnership {
    pub fn prepared(&self) -> &Receipt {
        &self.prepared
    }
}

impl OwnedActivation {
    /// Test-only ownership substitute for isolated image orchestration tests;
    /// production construction still requires the authenticated durable handoff.
    #[cfg(test)]
    pub(in crate::self_update) fn fixture(locks: SelectionLocks, prepared: Receipt) -> Self {
        Self { locks, prepared }
    }

    pub fn prepared(&self) -> &Receipt {
        &self.prepared
    }

    pub fn locks(&self) -> &SelectionLocks {
        &self.locks
    }
}

fn checked(journal: &Journal, selected: &Selected) -> Result<Receipt> {
    let receipt = journal.read()?;
    if receipt.phase != Phase::Prepared
        || receipt.workspace_id != selected.workspace()
        || receipt.server_id != selected.server_id()
    {
        bail!("update_handoff_job_mismatch");
    }
    Ok(receipt)
}

fn unchanged(journal: &Journal, expected: &Receipt) -> Result<()> {
    if journal.read()? != *expected {
        bail!("update_handoff_job_changed");
    }
    Ok(())
}

fn bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| anyhow::anyhow!("invalid_update_handoff_frame"))?;
    if bytes.is_empty() || bytes.len() > LIMIT {
        bail!("invalid_update_handoff_frame");
    }
    Ok(bytes)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!("invalid_update_handoff_frame"))
}

// A trickling peer cannot obtain a fresh timeout per byte.
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| anyhow::anyhow!("update_handoff_timed_out"))
}

fn send<T: Serialize>(mut stream: &UnixStream, value: &T, deadline: Instant) -> Result<()> {
    let bytes = bytes(value)?;
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(&bytes);
    let mut pending = frame.as_slice();
    let result = (|| -> std::io::Result<()> {
        while !pending.is_empty() {
            stream.set_write_timeout(Some(remaining(deadline).map_err(std::io::Error::other)?))?;
            let count = stream.write(pending)?;
            if count == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            pending = &pending[count..];
        }
        Ok(())
    })();
    result.map_err(|_| anyhow::anyhow!("update_handoff_io_failed"))
}

fn receive<T: DeserializeOwned>(stream: &UnixStream, deadline: Instant) -> Result<T> {
    fn exact(mut stream: &UnixStream, mut buffer: &mut [u8], deadline: Instant) -> Result<()> {
        while !buffer.is_empty() {
            // Darwin rejects SO_RCVTIMEO with EINVAL once the peer has closed,
            // even with a complete final confirmation still queued. Poll the
            // dedicated, single-reader stream instead of changing its timeout.
            let timeout = PollTimeout::try_from(remaining(deadline)?.as_millis().max(1))
                .map_err(|_| anyhow::anyhow!("update_handoff_deadline_unavailable"))?;
            let mut descriptor = [PollFd::new(stream.as_fd(), PollFlags::POLLIN)];
            if poll(&mut descriptor, timeout)
                .map_err(|_| anyhow::anyhow!("update_handoff_poll_failed"))?
                == 0
            {
                bail!("update_handoff_timed_out");
            }
            let events = descriptor[0].revents().unwrap_or(PollFlags::POLLNVAL);
            if events.contains(PollFlags::POLLNVAL)
                || !events.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
            {
                bail!("update_handoff_read_failed");
            }
            let count = stream.read(buffer).map_err(|error| {
                anyhow::anyhow!(match error.kind() {
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock =>
                        "update_handoff_timed_out",
                    std::io::ErrorKind::ConnectionReset => "update_handoff_connection_reset",
                    std::io::ErrorKind::Interrupted => "update_handoff_interrupted",
                    _ => "update_handoff_read_failed",
                })
            })?;
            if count == 0 {
                bail!("update_handoff_eof");
            }
            buffer = &mut buffer[count..];
        }
        Ok(())
    }
    let mut size = [0; 4];
    exact(stream, &mut size, deadline)?;
    let size = u32::from_be_bytes(size) as usize;
    if size == 0 || size > LIMIT {
        bail!("invalid_update_handoff_frame");
    }
    let mut body = vec![0; size];
    // Never buffer/prefetch the following SCM_RIGHTS marker on this stream.
    exact(stream, &mut body, deadline)?;
    decode(&body)
}

fn load(journal: &Journal) -> Result<Ownership> {
    let mut bytes = Vec::new();
    journal
        .directory
        .read_file(RECORD)
        .and_then(|file| file.take((LIMIT + 1) as u64).read_to_end(&mut bytes))
        .map_err(|_| anyhow::anyhow!("update_handoff_ownership_unavailable"))?;
    if bytes.len() > LIMIT {
        bail!("invalid_update_handoff_frame");
    }
    decode(&bytes)
}

fn persist(journal: &Journal, ownership: &Ownership) -> Result<()> {
    let bytes = bytes(ownership)?;
    // Exclusive, no replace and no cleanup on error: a partial record is an
    // uncertain takeover requiring explicit recovery, not an automatic retry.
    let result = (|| -> std::io::Result<()> {
        let mut file = journal.directory.create_file(RECORD)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        journal.directory.sync()
    })();
    result.map_err(|_| anyhow::anyhow!("update_handoff_ownership_unavailable"))
}

impl AuthenticatedPeer {
    /// Parent side. Borrow (do not consume) the parent's locks so all failures
    /// leave them held by the caller. Consumes/closes the one-shot channel.
    pub fn offer_ownership(
        self,
        journal: &Journal,
        selected: &Selected,
        locks: &SelectionLocks,
    ) -> Result<AcceptedOwnership> {
        self.verify()?;
        locks.directory(selected, Binary::Controller)?;
        locks.directory(selected, Binary::Cli)?;
        let prepared = checked(journal, selected)?;
        match journal.directory.read_file(RECORD) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => bail!("update_handoff_ownership_exists"),
        }
        let ready: Receipt = receive(&self.stream, Instant::now() + TIMEOUT)?;
        if ready != prepared {
            bail!("update_handoff_job_mismatch");
        }
        send(&self.stream, &prepared, Instant::now() + TIMEOUT)?;
        // This method checks the peer before the complete exchange and again
        // before confirmation; avoid redundant process scans between frames.
        locks.send(&self.stream)?;
        let ack: Ownership = receive(&self.stream, Instant::now() + TIMEOUT)?;
        let expected = Ownership {
            schema_version: 1,
            prepared: prepared.clone(),
            pid: self.image.pid(),
            creation: self.image.creation_identity().to_owned(),
        };
        if ack != expected || load(journal)? != expected {
            bail!("update_handoff_ownership_mismatch");
        }
        unchanged(journal, &prepared)?;
        self.verify()?;
        // The child cannot advance Prepared until the parent has checked the
        // durable ACK. Otherwise its first journal transition races this read.
        send(&self.stream, &expected, Instant::now() + TIMEOUT)?;
        // Close before callers can fork/exec. CLOEXEC alone is not sufficient.
        drop(self);
        Ok(AcceptedOwnership { prepared })
    }

    /// Child side, before threads or subprocesses on Darwin. Does not activate
    /// a Proxy connection, advance the journal, or start another Controller.
    pub fn take_ownership(self, journal: &Journal, selected: &Selected) -> Result<OwnedActivation> {
        self.verify()?;
        let prepared = checked(journal, selected)?;
        send(&self.stream, &prepared, Instant::now() + TIMEOUT)?;
        let offered: Receipt = receive(&self.stream, Instant::now() + TIMEOUT)?;
        if offered != prepared {
            bail!("update_handoff_job_mismatch");
        }
        let locks = SelectionLocks::receive(selected, &self.stream)?;
        unchanged(journal, &prepared)?;
        let own = ProcessImage::capture(std::process::id())
            .map_err(|_| anyhow::anyhow!("update_handoff_peer_unavailable"))?;
        let ownership = Ownership {
            schema_version: 1,
            prepared: prepared.clone(),
            pid: own.pid(),
            creation: own.creation_identity().to_owned(),
        };
        persist(journal, &ownership)?;
        unchanged(journal, &prepared)?;
        self.verify()?;
        send(&self.stream, &ownership, Instant::now() + TIMEOUT)?;
        let accepted: Ownership = receive(&self.stream, Instant::now() + TIMEOUT)
            .map_err(|error| anyhow::anyhow!("update_handoff_confirmation_failed: {error}"))?;
        if accepted != ownership {
            bail!("update_handoff_ownership_mismatch");
        }
        // Do not require parent liveness after ACK: it may legitimately exit
        // immediately on receipt. Keep the received locks alive in the result.
        drop(self);
        Ok(OwnedActivation { locks, prepared })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{selection::tests::Fixture, tests::receipt};

    fn fixture() -> (Fixture, Journal, Selected, SelectionLocks) {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        let selected = f.inventory().select(None, false).unwrap();
        let locks = selected.lock().unwrap();
        let mut receipt = receipt();
        receipt.workspace_id = "ws_a".into();
        receipt.server_id = "srv_a".into();
        let root = treer_platform::private_directory::PrivateDirectory::open(&f.path).unwrap();
        let journal = Journal::create(&root, &receipt).unwrap();
        journal.prepare_fixture().unwrap();
        (f, journal, selected, locks)
    }

    // Authentication itself has separate real-process coverage. These fault
    // fixtures directly assemble same-PID authenticated endpoints so failures
    // exercise the ownership protocol, not executable hashing performance.
    fn connection() -> (AuthenticatedPeer, UnixStream) {
        let (stream, other) = UnixStream::pair().unwrap();
        let image = ProcessImage::capture(std::process::id()).unwrap();
        (AuthenticatedPeer { stream, image }, other)
    }

    #[test]
    fn forged_ack_missing_or_different_record_and_changed_revision_never_accept() {
        for fault in ["pid", "missing-record", "different-record", "revision"] {
            let (_fixture, journal, selected, held) = fixture();
            let prepared = journal.read().unwrap();
            let (parent, child) = connection();
            std::thread::scope(|scope| {
                let child = scope.spawn(|| {
                    send(&child, &prepared, Instant::now() + TIMEOUT).unwrap();
                    assert_eq!(
                        receive::<Receipt>(&child, Instant::now() + TIMEOUT).unwrap(),
                        prepared
                    );
                    let adopted = SelectionLocks::receive(&selected, &child).unwrap();
                    let own = ProcessImage::capture(std::process::id()).unwrap();
                    let mut record = Ownership {
                        schema_version: 1,
                        prepared: prepared.clone(),
                        pid: own.pid(),
                        creation: own.creation_identity().to_owned(),
                    };
                    if fault == "different-record" {
                        record.creation.push('x');
                    }
                    if fault != "missing-record" {
                        persist(&journal, &record).unwrap();
                    }
                    if fault == "different-record" {
                        record.creation.pop();
                    }
                    if fault == "pid" {
                        record.pid = 0;
                    }
                    if fault == "revision" {
                        journal.advance(prepared.revision, Phase::Failed).unwrap();
                    }
                    send(&child, &record, Instant::now() + TIMEOUT).unwrap();
                    assert!(receive::<Ownership>(&child, Instant::now() + TIMEOUT).is_err());
                    drop(adopted);
                });
                assert!(
                    parent.offer_ownership(&journal, &selected, &held).is_err(),
                    "{fault}"
                );
                child.join().unwrap();
            });
            assert!(selected.lock().is_err());
        }
    }

    #[test]
    fn lost_parent_confirmation_keeps_uncertain_record_and_original_locks() {
        let (_fixture, journal, selected, held) = fixture();
        let prepared = journal.read().unwrap();
        let (child, parent) = connection();
        std::thread::scope(|scope| {
            let child = scope.spawn(|| child.take_ownership(&journal, &selected));
            assert_eq!(
                receive::<Receipt>(&parent, Instant::now() + TIMEOUT).unwrap(),
                prepared
            );
            send(&parent, &prepared, Instant::now() + TIMEOUT).unwrap();
            held.send(&parent).unwrap();
            let ack: Ownership = receive(&parent, Instant::now() + TIMEOUT).unwrap();
            assert!(ack == load(&journal).unwrap());
            drop(parent); // Lose the parent connection before final confirmation.
            assert!(child.join().unwrap().is_err());
        });
        let stored = bytes(&load(&journal).unwrap()).unwrap();
        assert!(persist(&journal, &load(&journal).unwrap()).is_err());
        assert_eq!(stored, bytes(&load(&journal).unwrap()).unwrap());
        assert_eq!(journal.read().unwrap(), prepared);
        assert!(selected.lock().is_err());
    }

    #[test]
    fn malformed_oversized_truncated_and_stalled_frames_fail_closed() {
        for frame in [
            0u32.to_be_bytes().to_vec(),
            ((LIMIT + 1) as u32).to_be_bytes().to_vec(),
            [2u32.to_be_bytes().as_slice(), b"{"].concat(),
            [2u32.to_be_bytes().as_slice(), b"xx"].concat(),
        ] {
            let (mut sender, receiver) = UnixStream::pair().unwrap();
            sender.write_all(&frame).unwrap();
            drop(sender);
            assert!(receive::<Receipt>(&receiver, Instant::now() + TIMEOUT).is_err());
        }
        let (_sender, receiver) = UnixStream::pair().unwrap();
        let started = Instant::now();
        assert!(receive::<Receipt>(&receiver, started + Duration::from_millis(50)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn complete_confirmation_remains_readable_after_peer_closes() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let expected = receipt();
        send(&sender, &expected, Instant::now() + TIMEOUT).unwrap();
        drop(sender);
        assert_eq!(
            receive::<Receipt>(&receiver, Instant::now() + TIMEOUT).unwrap(),
            expected
        );
    }
}
