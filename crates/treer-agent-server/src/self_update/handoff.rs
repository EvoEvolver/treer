//! Authenticate the exact connected lock-transfer peer, not a PID file or a
//! second probe connection. The one-shot ownership protocol binds the prepared
//! job and durable ACK; launching/detaching the owned activator and coordinating
//! activation/recovery remain separate work.
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

mod launcher_frame;
mod ownership;
pub mod parent;
#[cfg(test)]
mod source_boundary_tests;
mod worker;
pub use launcher_frame::LauncherFrame;
pub use ownership::{AcceptedOwnership, OwnedActivation};
pub use worker::WorkerBootstrap;

/// Only this module can construct proof of the endpoint preflight. Keep the
/// complete checked receipt so another job/revision cannot consume the proof.
pub(super) struct Preparation {
    receipt: Receipt,
}

impl Preparation {
    pub(super) fn receipt(&self) -> &Receipt {
        &self.receipt
    }
}

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
        journal.prepare(Preparation { receipt })
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
        // The child must detach before connecting. Check after acceptance so
        // polling an endpoint while the child is still starting is harmless,
        // but before peer authentication can yield a lock-transfer capability.
        child
            .verify_private_session()
            .map_err(|_| anyhow::anyhow!("update_handoff_session_unavailable"))?;
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
    launcher_read_started: bool,
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
        let result = Self {
            stream,
            image,
            launcher_read_started: false,
        };
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

fn revalidate_launcher_image(
    image: &ProcessImage,
    arbitration: &mut super::launcher_arbitration::Arbitration,
) -> Result<()> {
    if image.verify_current().is_err() {
        arbitration.invalidate();
        bail!("update_handoff_peer_changed");
    }
    Ok(())
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

    fn pending() -> super::super::launcher_arbitration::Arbitration {
        super::super::launcher_arbitration::Arbitration::new(
            super::super::launcher_arbitration::Binding::new(
                uuid::Uuid::new_v4(),
                &"a".repeat(64),
                &"b".repeat(64),
            )
            .unwrap(),
        )
    }

    #[test]
    fn launcher_read_failures_invalidate_and_cannot_be_retried_as_empty_eof() {
        use super::super::launcher_arbitration::Decision;
        use std::net::Shutdown;

        for mode in ["invalid", "timeout", "cancelled"] {
            let (stream, mut writer) = UnixStream::pair().unwrap();
            // Same-PID socket fixtures exercise verification and arbitration;
            // exact child authentication is covered by the real-process test.
            let mut peer = AuthenticatedPeer {
                stream,
                image: ProcessImage::capture(std::process::id()).unwrap(),
                launcher_read_started: false,
            };
            let (cancel, mut cancel_writer) = UnixStream::pair().unwrap();
            let expected = match mode {
                "invalid" => {
                    writer.write_all(&[b'x'; 152]).unwrap();
                    writer.shutdown(Shutdown::Write).unwrap();
                    "update_launcher_permit_frame_invalid"
                }
                "timeout" => "update_launcher_permit_read_timeout",
                _ => {
                    cancel_writer.write_all(b"C").unwrap();
                    "update_launcher_permit_read_cancelled"
                }
            };
            let mut arbitration = pending();
            assert_eq!(
                peer.read_launcher_frame(&mut arbitration, &cancel)
                    .err()
                    .unwrap()
                    .to_string(),
                expected
            );
            assert_eq!(arbitration.decision(), Decision::Uncertain);
            if mode != "invalid" {
                writer.shutdown(Shutdown::Write).unwrap();
            }
            assert_eq!(
                peer.read_launcher_frame(&mut arbitration, &cancel)
                    .err()
                    .unwrap()
                    .to_string(),
                "update_recovery_required"
            );
            assert_eq!(arbitration.decision(), Decision::Uncertain);
        }
    }

    #[test]
    fn launcher_empty_eof_is_only_raw_data_and_the_connection_is_single_use() {
        use super::super::launcher_arbitration::{transport::Kind, Decision};
        let (stream, writer) = UnixStream::pair().unwrap();
        let mut peer = AuthenticatedPeer {
            stream,
            image: ProcessImage::capture(std::process::id()).unwrap(),
            launcher_read_started: false,
        };
        let (cancel, _cancel_writer) = UnixStream::pair().unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        let mut arbitration = pending();
        let observation = peer.read_launcher_frame(&mut arbitration, &cancel).unwrap();
        assert_eq!(observation.kind(), Kind::EmptyEof);
        drop(observation);
        assert_eq!(arbitration.decision(), Decision::Pending);
        assert!(peer.read_launcher_frame(&mut arbitration, &cancel).is_err());
        assert_eq!(arbitration.decision(), Decision::Uncertain);
    }

    #[test]
    fn authenticated_launcher_read_rejects_foreign_attempt_intent_and_identity() {
        use super::super::launcher_arbitration::{transport::Kind, Arbitration, Binding, Decision};
        use std::net::Shutdown;

        let image = ProcessImage::capture(std::process::id()).unwrap();
        let digest = digest_file(image.executable()).unwrap();
        let expected =
            Binding::new(uuid::Uuid::from_u128(7), &"a".repeat(64), &"b".repeat(64)).unwrap();
        for (attempt, intent, identity, matches) in [
            (7, b'a', b'b', true),
            (6, b'a', b'b', false), // Previous attempt with identical digests.
            (8, b'a', b'b', false), // Another attempt is not current either.
            (7, b'c', b'b', false),
            (7, b'a', b'd', false),
        ] {
            let (stream, mut writer) = UnixStream::pair().unwrap();
            // Authenticate the actual connected same-PID peer, including image
            // inode and digest. These fixtures test binding equality, not a
            // supervisor's still-unimplemented durable registration protocol.
            let mut peer =
                AuthenticatedPeer::authenticate(stream, &image, image.executable(), &digest)
                    .unwrap();
            let (cancel, _cancel_writer) = UnixStream::pair().unwrap();
            let mut frame = Vec::from(&b"TRLP\x01\0\0\0"[..]);
            frame.extend_from_slice(uuid::Uuid::from_u128(attempt).as_bytes());
            frame.extend_from_slice(&[intent; 64]);
            frame.extend_from_slice(&[identity; 64]);
            writer.write_all(&frame).unwrap();
            writer.shutdown(Shutdown::Write).unwrap();
            let mut arbitration = Arbitration::new(expected.clone());
            let result = peer.read_launcher_frame(&mut arbitration, &cancel);
            if matches {
                let observation = result.unwrap();
                assert_eq!(observation.kind(), Kind::Permit);
                assert!(observation.binding() == Some(&expected));
                // Reading a matching frame does not accept it, ACK it, or
                // establish durable authority to execute anything.
                assert_eq!(arbitration.decision(), Decision::Pending);
                arbitration.accept(observation).unwrap();
                assert_eq!(arbitration.decision(), Decision::Accepted);
            } else {
                assert_eq!(
                    result.err().unwrap().to_string(),
                    "update_launcher_attempt_mismatch"
                );
                assert_eq!(arbitration.decision(), Decision::Uncertain);
            }
            assert_eq!(
                peer.read_launcher_frame(&mut arbitration, &cancel)
                    .err()
                    .unwrap()
                    .to_string(),
                "update_recovery_required"
            );
            assert_eq!(
                arbitration.decision(),
                if matches {
                    Decision::Accepted
                } else {
                    Decision::Uncertain
                }
            );
        }
    }

    #[test]
    fn authenticated_empty_or_partial_frames_cannot_be_accepted_as_permits() {
        use super::super::launcher_arbitration::Decision;
        let image = ProcessImage::capture(std::process::id()).unwrap();
        let digest = digest_file(image.executable()).unwrap();
        for payload in [&b""[..], &b"TRLP"[..]] {
            let (stream, mut writer) = UnixStream::pair().unwrap();
            let mut peer =
                AuthenticatedPeer::authenticate(stream, &image, image.executable(), &digest)
                    .unwrap();
            writer.write_all(payload).unwrap();
            writer.shutdown(std::net::Shutdown::Write).unwrap();
            let (cancel, _cancel_writer) = UnixStream::pair().unwrap();
            let mut arbitration = pending();
            let frame = peer.read_launcher_frame(&mut arbitration, &cancel).unwrap();
            assert!(frame.binding().is_none());
            assert_eq!(
                arbitration.accept(frame).unwrap_err().to_string(),
                "update_recovery_required"
            );
            assert_eq!(arbitration.decision(), Decision::Uncertain);
            assert!(peer.read_launcher_frame(&mut arbitration, &cancel).is_err());
            assert_eq!(arbitration.decision(), Decision::Uncertain);
        }
    }

    #[test]
    fn accepting_a_frame_rechecks_binding_and_never_reopens_a_terminal_gate() {
        use super::super::launcher_arbitration::{Arbitration, Binding, Decision};
        let binding = |attempt| {
            Binding::new(
                uuid::Uuid::from_u128(attempt),
                &"a".repeat(64),
                &"b".repeat(64),
            )
            .unwrap()
        };
        let image = ProcessImage::capture(std::process::id()).unwrap();
        let digest = digest_file(image.executable()).unwrap();
        let make_peer = || {
            let (stream, mut writer) = UnixStream::pair().unwrap();
            let peer = AuthenticatedPeer::authenticate(stream, &image, image.executable(), &digest)
                .unwrap();
            let mut bytes = Vec::from(&b"TRLP\x01\0\0\0"[..]);
            bytes.extend_from_slice(uuid::Uuid::from_u128(7).as_bytes());
            bytes.extend_from_slice(&[b'a'; 64]);
            bytes.extend_from_slice(&[b'b'; 64]);
            writer.write_all(&bytes).unwrap();
            writer.shutdown(std::net::Shutdown::Write).unwrap();
            // Keep the half-closed writer descriptor alive: Darwin may stop
            // reporting socketpair peer credentials after its full close.
            (peer, writer)
        };
        let (cancel, _cancel_writer) = UnixStream::pair().unwrap();
        for changed_binding in [false, true] {
            let (mut peer, _writer) = make_peer();
            let mut arbitration = Arbitration::new(binding(7));
            let frame = peer.read_launcher_frame(&mut arbitration, &cancel).unwrap();
            let expected_error = if changed_binding {
                arbitration = Arbitration::new(binding(8));
                "update_launcher_attempt_mismatch"
            } else {
                arbitration.invalidate();
                "update_recovery_required"
            };
            assert_eq!(
                arbitration.accept(frame).unwrap_err().to_string(),
                expected_error
            );
            assert_eq!(arbitration.decision(), Decision::Uncertain);
        }

        let (mut first_peer, _first_writer) = make_peer();
        let (mut second_peer, _second_writer) = make_peer();
        let mut arbitration = Arbitration::new(binding(7));
        let first = first_peer
            .read_launcher_frame(&mut arbitration, &cancel)
            .unwrap();
        let second = second_peer
            .read_launcher_frame(&mut arbitration, &cancel)
            .unwrap();
        arbitration.accept(first).unwrap();
        assert_eq!(arbitration.decision(), Decision::Accepted);
        assert_eq!(
            arbitration.accept(second).unwrap_err().to_string(),
            "update_recovery_required"
        );
        assert_eq!(arbitration.decision(), Decision::Accepted);
    }

    #[test]
    #[ignore = "owned subprocess helper, actually invoked by the outer peer test"]
    fn peer_child() {
        let path = std::env::var_os("TREER_TEST_HANDOFF_SOCKET").unwrap();
        let mut socket = UnixStream::connect(path).unwrap();
        socket.write_all(b"R").unwrap();
        if std::env::var_os("TREER_TEST_LAUNCHER_FRAME").is_some() {
            let mut frame = Vec::from(&b"TRLP\x01\0\0\0"[..]);
            frame.extend_from_slice(uuid::Uuid::from_u128(7).as_bytes());
            frame.extend_from_slice(&[b'a'; 64]);
            frame.extend_from_slice(&[b'b'; 64]);
            socket.write_all(&frame).unwrap();
            socket.shutdown(std::net::Shutdown::Write).unwrap();
            let _ = std::io::stdin().read(&mut [0]);
            return;
        }
        let _ = std::io::stdin().read(&mut [0]);
        socket.write_all(b"E").unwrap();
    }

    #[test]
    fn peer_exit_after_successful_frame_read_cannot_be_accepted() {
        use super::super::launcher_arbitration::{Arbitration, Binding, Decision};
        for killed in [false, true] {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "ta-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..16]
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
                    .env("TREER_TEST_LAUNCHER_FRAME", "1")
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
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut ready = [0];
            stream.read_exact(&mut ready).unwrap();
            assert_eq!(ready, [b'R']);
            let image = ProcessImage::capture(child.0.id()).unwrap();
            let digest = digest_file(image.executable()).unwrap();
            let mut peer =
                AuthenticatedPeer::authenticate(stream, &image, image.executable(), &digest)
                    .unwrap();
            let expected =
                Binding::new(uuid::Uuid::from_u128(7), &"a".repeat(64), &"b".repeat(64)).unwrap();
            let mut arbitration = Arbitration::new(expected);
            let (cancel, _cancel_writer) = UnixStream::pair().unwrap();
            let frame = peer.read_launcher_frame(&mut arbitration, &cancel).unwrap();
            assert_eq!(arbitration.decision(), Decision::Pending);
            assert!(child.0.try_wait().unwrap().is_none());
            if killed {
                child.0.kill().unwrap();
            } else {
                drop(child.0.stdin.take());
            }
            let status = child.0.wait().unwrap();
            assert_eq!(status.success(), !killed);
            assert!(image.verify_current().is_err());
            // The frame really passed the read boundary while the peer was
            // alive. Only the acceptance-time identity check can reject it.
            assert_eq!(
                arbitration.accept(frame).unwrap_err().to_string(),
                "update_handoff_peer_changed"
            );
            assert_eq!(arbitration.decision(), Decision::Uncertain);
            assert!(peer.read_launcher_frame(&mut arbitration, &cancel).is_err());
            assert_eq!(arbitration.decision(), Decision::Uncertain);
        }
    }

    #[test]
    #[ignore = "owned same-PID exec helper, invoked twice by the outer exec test"]
    fn peer_exec_child() {
        use nix::fcntl::{fcntl, FcntlArg, FdFlag};
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let path = std::env::var_os("TREER_TEST_HANDOFF_SOCKET").unwrap();
        let mut socket = UnixStream::connect(&path).unwrap();
        require_cloexec(&socket).unwrap();
        if std::env::var_os("TREER_TEST_EXEC_TARGET").is_some() {
            let inherited: i32 = std::env::var("TREER_TEST_EXEC_FD")
                .unwrap()
                .parse()
                .unwrap();
            // Only this owned test child deliberately inherits its own socket.
            // Restore CLOEXEC immediately; safe raw-fd send/shutdown avoid
            // manufacturing Rust ownership of an inherited descriptor.
            fcntl(inherited, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).unwrap();
            let mut frame = Vec::from(&b"TRLP\x01\0\0\0"[..]);
            frame.extend_from_slice(&[1; 16]);
            frame.extend_from_slice(&[b'a'; 64]);
            frame.extend_from_slice(&[b'b'; 64]);
            while !frame.is_empty() {
                let sent =
                    nix::sys::socket::send(inherited, &frame, nix::sys::socket::MsgFlags::empty())
                        .unwrap();
                assert!(sent > 0);
                frame.drain(..sent);
            }
            nix::sys::socket::shutdown(inherited, nix::sys::socket::Shutdown::Write).unwrap();
            socket.write_all(b"X").unwrap();
            let _ = std::io::stdin().read(&mut [0]);
            return;
        }
        socket.write_all(b"R").unwrap();
        std::io::stdin().read_exact(&mut [0]).unwrap();
        let image = std::env::var_os("TREER_TEST_EXEC_IMAGE").unwrap();
        fcntl(socket.as_raw_fd(), FcntlArg::F_SETFD(FdFlag::empty())).unwrap();
        let error = Command::new(image)
            .args([
                "--exact",
                "self_update::handoff::tests::peer_exec_child",
                "--ignored",
            ])
            .env_clear()
            .env("TREER_TEST_HANDOFF_SOCKET", path)
            .env("TREER_TEST_EXEC_TARGET", "1")
            .env("TREER_TEST_EXEC_FD", socket.as_raw_fd().to_string())
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .exec();
        panic!("owned fixture exec failed: {error}");
    }

    #[test]
    fn same_pid_exec_after_read_entry_invalidates_the_observation() {
        use nix::fcntl::{fcntl, FcntlArg, OFlag};
        use std::os::fd::AsRawFd;

        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "te-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let fixture = Fixture(root);
        let alternate = fixture.0.join("image");
        std::fs::copy(std::env::current_exe().unwrap(), &alternate).unwrap();
        let alternate_file = File::open(&alternate).unwrap();
        let path = fixture.0.join("s");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "self_update::handoff::tests::peer_exec_child",
                    "--ignored",
                ])
                .env_clear()
                .env("TREER_TEST_HANDOFF_SOCKET", &path)
                .env("TREER_TEST_EXEC_IMAGE", &alternate)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let accept = || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut ready = [0];
                        stream.read_exact(&mut ready).unwrap();
                        return (stream, ready[0]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "owned exec peer did not connect");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
        };
        let (stream, ready) = accept();
        assert_eq!(ready, b'R');
        let original = ProcessImage::capture(child.0.id()).unwrap();
        let digest = digest_file(original.executable()).unwrap();
        let mut authenticated =
            AuthenticatedPeer::authenticate(stream, &original, original.executable(), &digest)
                .unwrap();
        // A test-only duplicate shares O_NONBLOCK with the reader. Only raw
        // read changes that flag, after the authenticated entry's pre-check.
        // This is a kernel-visible barrier, not a sleep-based exec race or a
        // production callback capable of bypassing verification.
        let observer = authenticated.stream.try_clone().unwrap();
        let flags =
            || OFlag::from_bits_truncate(fcntl(observer.as_raw_fd(), FcntlArg::F_GETFL).unwrap());
        assert!(!flags().contains(OFlag::O_NONBLOCK));
        let (cancel, _cancel_writer) = UnixStream::pair().unwrap();
        let reader = std::thread::spawn(move || {
            let mut arbitration = pending();
            let error = authenticated
                .read_launcher_frame(&mut arbitration, &cancel)
                .err()
                .map(|error| error.to_string());
            (error, arbitration.decision())
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !flags().contains(OFlag::O_NONBLOCK) {
            assert!(
                Instant::now() < deadline,
                "reader did not pass authentication"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        child.0.stdin.as_mut().unwrap().write_all(b"E").unwrap();
        let (_target_connection, ready) = accept();
        assert_eq!(ready, b'X');
        assert!(child.0.try_wait().unwrap().is_none());
        let changed = ProcessImage::capture(child.0.id()).unwrap();
        assert_eq!(changed.pid(), original.pid());
        assert!(changed.matches_file(&alternate_file).unwrap());
        assert!(original.verify_current().is_err());
        let (error, decision) = reader.join().unwrap();
        assert_eq!(error.as_deref(), Some("update_handoff_peer_changed"));
        assert_eq!(
            decision,
            super::super::launcher_arbitration::Decision::Uncertain
        );
        drop(child.0.stdin.take());
        assert!(child.0.wait().unwrap().success());
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
        let mut authenticated =
            AuthenticatedPeer::authenticate(stream, &image, image.executable(), &digest).unwrap();
        authenticated.verify().unwrap();
        drop(child.0.stdin.take());
        assert!(child.0.wait().unwrap().success());
        assert!(authenticated.verify().is_err());
        // Identity revalidation fails before the reader can consume bytes.
        // A clone is a test-only view of the same accepted socket; production
        // exposes no stream accessor or duplicate.
        let mut retained = authenticated.stream.try_clone().unwrap();
        retained.set_nonblocking(true).unwrap();
        let (cancel, _cancel_writer) = UnixStream::pair().unwrap();
        let mut arbitration = pending();
        assert!(authenticated
            .read_launcher_frame(&mut arbitration, &cancel)
            .is_err());
        assert_eq!(
            arbitration.decision(),
            super::super::launcher_arbitration::Decision::Uncertain
        );
        assert!(authenticated.launcher_read_started);
        let mut unread = [0];
        assert_eq!(retained.read(&mut unread).unwrap(), 1);
        assert_eq!(unread, [b'E']);
        // Exercise the post-read guard separately with real stale OS identity:
        // even a successful raw EOF drain cannot make a dead peer current.
        let raw =
            super::super::launcher_arbitration::transport::read(&mut retained, &cancel).unwrap();
        assert_eq!(
            raw.kind(),
            super::super::launcher_arbitration::transport::Kind::EmptyEof
        );
        let mut post_read = pending();
        assert_eq!(
            revalidate_launcher_image(&image, &mut post_read)
                .unwrap_err()
                .to_string(),
            "update_handoff_peer_changed"
        );
        assert_eq!(
            post_read.decision(),
            super::super::launcher_arbitration::Decision::Uncertain
        );
    }
}
