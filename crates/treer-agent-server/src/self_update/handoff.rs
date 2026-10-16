//! Authenticate the exact connected lock-transfer peer, not a PID file or a
//! second probe connection. This is only the process-proof layer: the activator
//! coordinator must still create a private endpoint, bind the job, and persist
//! ownership ACK before the sender releases its locks or reports acceptance.
use std::{cell::Cell, fs::File, os::unix::net::UnixStream, path::Path};

use anyhow::{bail, Result};
use treer_platform::private_directory::PrivateListener;
use treer_platform::process_image::ProcessImage;

use super::{
    hex,
    images::digest_file,
    selection::{Selected, SelectionLocks},
    Journal, Phase, Receipt,
};

/// Exclusive private endpoint bound to one already-prepared journal revision.
/// This does not launch a child, transfer locks or persist an ownership ACK.
pub struct Endpoint<'a> {
    listener: PrivateListener,
    journal: &'a Journal,
    receipt: Receipt,
    failed: Cell<bool>,
}

impl<'a> Endpoint<'a> {
    /// Preparation coordinator's phase transition: reject an unusable private
    /// endpoint before recording Prepared. This does not reserve the socket.
    pub fn mark_prepared(journal: &Journal, job_directory: &Path) -> Result<Receipt> {
        let receipt = journal.read()?;
        if receipt.phase != Phase::Preparing {
            bail!("update_handoff_phase_conflict");
        }
        journal
            .directory
            .preflight_handoff(job_directory)
            .map_err(|_| anyhow::anyhow!("update_handoff_endpoint_unavailable"))?;
        journal.advance(receipt.revision, Phase::Prepared)
    }

    pub fn bind(journal: &'a Journal, job_directory: &Path) -> Result<Self> {
        let receipt = journal.read()?;
        if receipt.phase != Phase::Prepared {
            bail!("update_handoff_phase_conflict");
        }
        let listener = journal
            .directory
            .bind_handoff(job_directory)
            .map_err(|_| anyhow::anyhow!("update_handoff_endpoint_unavailable"))?;
        let result = Self {
            listener,
            journal,
            receipt,
            failed: Cell::new(false),
        };
        result.verify()?;
        Ok(result)
    }

    fn verify(&self) -> Result<()> {
        if self.journal.read()? != self.receipt {
            bail!("update_handoff_job_changed");
        }
        Ok(())
    }

    /// No waiting/retry hidden here. The future owned-child coordinator must
    /// enforce its deadline and abort before acceptance when the child dies.
    /// Any error permanently aborts this endpoint; an unauthenticated connection
    /// fails loud rather than being silently ignored until another peer arrives.
    pub fn accept(
        &self,
        child: &ProcessImage,
        verified_image: &File,
        digest: &str,
    ) -> Result<Option<AuthenticatedPeer>> {
        if self.failed.get() {
            bail!("update_handoff_aborted");
        }
        let result = self.try_accept(child, verified_image, digest);
        if result.is_err() {
            self.failed.set(true);
        }
        result
    }

    fn try_accept(
        &self,
        child: &ProcessImage,
        verified_image: &File,
        digest: &str,
    ) -> Result<Option<AuthenticatedPeer>> {
        self.verify()?;
        let stream = match self.listener.accept() {
            Ok(stream) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(_) => bail!("update_handoff_endpoint_unavailable"),
        };
        let peer = AuthenticatedPeer::authenticate(stream, child, verified_image, digest)?;
        self.verify()?;
        Ok(Some(peer))
    }
}

fn require_cloexec(stream: &UnixStream) -> Result<()> {
    treer_platform::lock_transfer::require_cloexec_socket(stream)
        .map_err(|_| anyhow::anyhow!("update_handoff_inheritable_socket"))
}

fn peer(stream: &UnixStream) -> Result<(u32, u32)> {
    #[cfg(target_os = "linux")]
    let (pid, uid) = {
        let credentials =
            nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
                .map_err(|_| anyhow::anyhow!("update_handoff_peer_unavailable"))?;
        (credentials.pid(), credentials.uid())
    };
    #[cfg(target_os = "macos")]
    let (pid, uid) = {
        let pid = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid)
            .map_err(|_| anyhow::anyhow!("update_handoff_peer_unavailable"))?;
        let (uid, _) = nix::unistd::getpeereid(stream)
            .map_err(|_| anyhow::anyhow!("update_handoff_peer_unavailable"))?;
        (pid, uid.as_raw())
    };
    let pid = u32::try_from(pid)
        .ok()
        .filter(|pid| *pid != 0)
        .ok_or_else(|| anyhow::anyhow!("update_handoff_peer_unavailable"))?;
    Ok((pid, uid))
}

/// No raw-stream accessor: all lock transfer through this wrapper rechecks the
/// peer's OS creation identity/image. No Debug/Serialize (no process paths).
pub struct AuthenticatedPeer {
    stream: UnixStream,
    image: ProcessImage,
}

impl AuthenticatedPeer {
    /// `expected` must be captured from the coordinator's owned child/parent;
    /// `verified_image` is the checked job-local image held by the coordinator,
    /// whose hash was authenticated before executing it. No path/PATH lookup.
    pub fn authenticate(
        stream: UnixStream,
        expected: &ProcessImage,
        verified_image: &File,
        expected_digest: &str,
    ) -> Result<Self> {
        require_cloexec(&stream)?;
        if !hex(expected_digest, 64) {
            bail!("invalid_update_handoff_image");
        }
        let (pid, uid) = peer(&stream)?;
        if uid != nix::unistd::geteuid().as_raw() || pid != expected.pid() {
            bail!("update_handoff_peer_mismatch");
        }
        expected
            .verify_current()
            .map_err(|_| anyhow::anyhow!("update_handoff_peer_changed"))?;
        let image = ProcessImage::capture(pid)
            .map_err(|_| anyhow::anyhow!("update_handoff_peer_unavailable"))?;
        if image.creation_identity() != expected.creation_identity()
            || !image
                .matches_file(verified_image)
                .map_err(|_| anyhow::anyhow!("update_handoff_image_unavailable"))?
            || digest_file(verified_image)
                .map_err(|_| anyhow::anyhow!("update_handoff_image_unavailable"))?
                != expected_digest
            || digest_file(image.executable())
                .map_err(|_| anyhow::anyhow!("update_handoff_image_unavailable"))?
                != expected_digest
        {
            bail!("update_handoff_peer_mismatch");
        }
        let result = Self { stream, image };
        result.verify()?;
        Ok(result)
    }

    fn verify(&self) -> Result<()> {
        require_cloexec(&self.stream)?;
        let (pid, uid) = peer(&self.stream)?;
        if pid != self.image.pid() || uid != nix::unistd::geteuid().as_raw() {
            bail!("update_handoff_peer_mismatch");
        }
        self.image
            .verify_current()
            .map_err(|_| anyhow::anyhow!("update_handoff_peer_changed"))
    }

    /// This is NOT an ownership ACK. The parent retains locks until a durable
    /// job-bound ACK has been checked by the future coordinator.
    pub fn send_locks(&self, locks: &SelectionLocks) -> Result<()> {
        self.verify()?;
        locks.send(&self.stream)?;
        self.verify()
    }

    /// Before starting threads/subprocesses on Darwin (SCM_RIGHTS CLOEXEC).
    pub fn receive_locks(&self, selected: &Selected) -> Result<SelectionLocks> {
        self.verify()?;
        let locks = SelectionLocks::receive(selected, &self.stream)?;
        self.verify()?;
        Ok(locks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::{fs::DirBuilderExt, net::UnixListener},
        path::PathBuf,
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "owned subprocess helper, actually invoked by the outer peer test"]
    fn peer_child() {
        let path = std::env::var_os("TREER_TEST_HANDOFF_SOCKET").unwrap();
        let mut socket = UnixStream::connect(path).unwrap();
        socket.write_all(b"R").unwrap();
        let _ = std::io::stdin().read(&mut [0]);
    }

    #[test]
    fn exact_connected_child_rejects_other_pid_image_digest_and_exited_peer() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "th-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let fixture = Fixture(root);
        let socket = fixture.0.join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "self_update::handoff::tests::peer_child",
                    "--ignored",
                ])
                .env_clear()
                .env("TREER_TEST_HANDOFF_SOCKET", &socket)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(child.0.try_wait().unwrap().is_none());
                    assert!(Instant::now() < deadline, "owned child did not connect");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => panic!("private fixture accept failed"),
            }
        };
        // Darwin inherits listener O_NONBLOCK on accepted sockets.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut ready = [0];
        stream.read_exact(&mut ready).unwrap();
        assert_eq!(ready, [b'R']);
        let image = ProcessImage::capture(child.0.id()).unwrap();
        let parent = ProcessImage::capture(std::process::id()).unwrap();
        let digest = digest_file(image.executable()).unwrap();
        let bad_digest = "0".repeat(64);
        // Same executable and same UID are not enough: PID must be exact.
        assert!(AuthenticatedPeer::authenticate(
            stream.try_clone().unwrap(),
            &parent,
            image.executable(),
            &digest,
        )
        .is_err());
        assert!(AuthenticatedPeer::authenticate(
            stream.try_clone().unwrap(),
            &image,
            image.executable(),
            &bad_digest,
        )
        .is_err());
        let copy = fixture.0.join("other-image");
        std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        let same_bytes_different_inode = File::open(copy).unwrap();
        assert_eq!(digest_file(&same_bytes_different_inode).unwrap(), digest);
        assert!(AuthenticatedPeer::authenticate(
            stream.try_clone().unwrap(),
            &image,
            &same_bytes_different_inode,
            &digest,
        )
        .is_err());
        let authenticated =
            AuthenticatedPeer::authenticate(stream, &image, image.executable(), &digest).unwrap();
        authenticated.verify().unwrap();
        drop(child.0.stdin.take());
        assert!(child.0.wait().unwrap().success());
        assert!(authenticated.verify().is_err());
    }
}
