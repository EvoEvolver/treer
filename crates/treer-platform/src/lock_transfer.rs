//! Bounded SCM_RIGHTS transport for self-update lock ownership handoff.
//!
//! Caller authenticates the private socket peer/job before sending or adopting
//! descriptors. Receiving descriptors alone is NOT proof of the right locks.
//! Core must match each descriptor to its checked lock inode and retain it for
//! the whole transaction. No flock unlock/relock and no inheritable parent FDs.
use std::{
    fs::File,
    io::{self, IoSlice, IoSliceMut},
    mem::MaybeUninit,
    os::{fd::AsFd, unix::net::UnixStream},
    time::Duration,
};

use rustix::net::{
    recvmsg, sendmsg, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags,
};

const MAX_LOCKS: usize = 3; // two installation parents + one enrollment
const TIMEOUT: Duration = Duration::from_secs(3);

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid lock ownership transfer",
    )
}

/// Match the received reference against a checked stable lock file, then ensure
/// this same open description holds LOCK_EX. It cannot steal a competing lock.
pub fn adopt(file: File, expected: &File) -> io::Result<File> {
    use std::os::unix::fs::MetadataExt;
    let actual = file.metadata()?;
    let expected = expected.metadata()?;
    if !actual.is_file() || (actual.dev(), actual.ino()) != (expected.dev(), expected.ino()) {
        return Err(invalid());
    }
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)?;
    Ok(file)
}

/// This creates another reference to the SAME open file descriptions, preserving
/// flock ownership while parent/child overlap. Never retry an ambiguous send.
pub fn send(stream: &UnixStream, locks: &[&File]) -> io::Result<()> {
    if locks.is_empty() || locks.len() > MAX_LOCKS {
        return Err(invalid());
    }
    for file in locks {
        if !rustix::io::fcntl_getfd(file)?.contains(rustix::io::FdFlags::CLOEXEC) {
            return Err(invalid());
        }
    }
    stream.set_write_timeout(Some(TIMEOUT))?;
    let fds: Vec<_> = locks.iter().map(|file| file.as_fd()).collect();
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(invalid());
    }
    let header = [b'T', 1, locks.len() as u8];
    let count = sendmsg(
        stream,
        &[IoSlice::new(&header)],
        &mut control,
        SendFlags::empty(),
    )?;
    if count != header.len() {
        return Err(invalid());
    }
    Ok(())
}

/// Invoke before the activator starts threads/spawns subprocesses on macOS:
/// unlike Linux's MSG_CMSG_CLOEXEC, Darwin needs fcntl after receipt. All accepted
/// FDs are close-on-exec before returning. Errors drop every received descriptor.
pub fn receive(stream: &UnixStream, expected: usize) -> io::Result<Vec<File>> {
    if expected == 0 || expected > MAX_LOCKS {
        return Err(invalid());
    }
    stream.set_read_timeout(Some(TIMEOUT))?;
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(4))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let mut header = [0; 3];
    #[cfg(target_os = "linux")]
    let flags = RecvFlags::CMSG_CLOEXEC;
    #[cfg(target_os = "macos")]
    let flags = RecvFlags::empty();
    let message = recvmsg(
        stream,
        &mut [IoSliceMut::new(&mut header)],
        &mut control,
        flags,
    )?;
    let mut files = Vec::new();
    for item in control.drain() {
        match item {
            RecvAncillaryMessage::ScmRights(fds) => {
                for fd in fds {
                    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
                    files.push(File::from(fd));
                }
            }
            #[allow(unreachable_patterns)]
            _ => return Err(invalid()),
        }
    }
    if message.bytes != header.len()
        || message
            .flags
            .intersects(ReturnFlags::CTRUNC | ReturnFlags::TRUNC)
        || header != [b'T', 1, expected as u8]
        || files.len() != expected
    {
        return Err(invalid());
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_directory::PrivateDirectory;
    use std::{io::Write, os::unix::fs::DirBuilderExt};

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> (Self, PrivateDirectory) {
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "tlf-{}-{:x}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            let directory = PrivateDirectory::open(&path).unwrap();
            (Self(path), directory)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn assert_locked(file: &File) {
        assert_eq!(
            rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive),
            Err(rustix::io::Errno::WOULDBLOCK)
        );
    }

    fn isolated(helper: &str) {
        // CLOEXEC closes on exec, not fork. Unrelated concurrent test spawns can
        // briefly inherit lock references between fork/exec and falsify an
        // immediate-release assertion. Run the proof with no unrelated spawns.
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", helper, "--ignored"])
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "isolated lock fixture failed: {helper}");
    }

    #[test]
    fn transfer_keeps_the_original_lock_until_the_last_owner_drops() {
        isolated("lock_transfer::tests::transfer_child");
    }

    #[test]
    #[ignore = "isolated release proof, actually invoked by outer test"]
    fn transfer_child() {
        let (_fixture, directory) = Fixture::new();
        let original = directory.lock("held").unwrap();
        let contender = directory.read_file("held").unwrap();
        let (sender, receiver) = UnixStream::pair().unwrap();
        send(&sender, &[&original]).unwrap();
        let transferred = receive(&receiver, 1).unwrap();
        assert_eq!(
            rustix::io::fcntl_getfd(&transferred[0]).unwrap(),
            rustix::io::FdFlags::CLOEXEC
        );
        assert_locked(&contender);
        drop(original);
        assert_locked(&contender);
        drop(transferred);
        rustix::fs::flock(
            &contender,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .unwrap();
    }

    #[test]
    fn inheritable_sender_is_rejected_in_an_isolated_child() {
        // Never clear CLOEXEC in the concurrently spawning parent test process.
        isolated("lock_transfer::tests::inheritable_sender_child");
    }

    #[test]
    #[ignore = "isolated no-concurrent-spawn helper, actually invoked by outer test"]
    fn inheritable_sender_child() {
        let (_fixture, directory) = Fixture::new();
        let original = directory.lock("held").unwrap();
        let (sender, mut receiver) = UnixStream::pair().unwrap();
        rustix::io::fcntl_setfd(&original, rustix::io::FdFlags::empty()).unwrap();
        assert!(send(&sender, &[&original]).is_err());
        rustix::io::fcntl_setfd(&original, rustix::io::FdFlags::CLOEXEC).unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut byte = [0];
        assert_eq!(
            std::io::Read::read(&mut receiver, &mut byte)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn malformed_or_wrong_count_transfer_does_not_keep_a_hidden_lock_owner() {
        isolated("lock_transfer::tests::malformed_child");
    }

    #[test]
    #[ignore = "isolated release proof, actually invoked by outer test"]
    fn malformed_child() {
        for malformed in [false, true] {
            let (_fixture, directory) = Fixture::new();
            let original = directory.lock("held").unwrap();
            let contender = directory.read_file("held").unwrap();
            let (mut sender, receiver) = UnixStream::pair().unwrap();
            if malformed {
                sender.write_all(b"bad").unwrap();
            } else {
                send(&sender, &[&original]).unwrap();
            }
            assert!(receive(&receiver, 2).is_err());
            drop(original);
            rustix::fs::flock(
                &contender,
                rustix::fs::FlockOperation::NonBlockingLockExclusive,
            )
            .unwrap();
        }
    }

    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "owned subprocess fixture, executed by the parent handoff test"]
    fn handoff_child() {
        use std::io::Read;
        let path = std::env::var_os("TREER_TEST_LOCK_SOCKET").unwrap();
        let mut socket = UnixStream::connect(path).unwrap();
        let locks = receive(&socket, 3).unwrap();
        assert_eq!(locks.len(), 3);
        socket.write_all(b"R").unwrap();
        let _ = std::io::stdin().read(&mut [0_u8; 1]);
        drop(locks);
    }

    #[test]
    fn child_retains_all_locks_after_parent_drops_and_releases_them_on_exit() {
        isolated("lock_transfer::tests::retention_child");
    }

    #[test]
    #[ignore = "isolated release proof, actually invoked by outer test"]
    fn retention_child() {
        use std::{
            io::Read,
            process::{Command, Stdio},
        };
        let (fixture, directory) = Fixture::new();
        let locks: Vec<_> = ["install-a", "install-b", "enrollment"]
            .iter()
            .map(|name| directory.lock(name).unwrap())
            .collect();
        let contenders: Vec<_> = ["install-a", "install-b", "enrollment"]
            .iter()
            .map(|name| directory.read_file(name).unwrap())
            .collect();
        let path = fixture.0.join("s");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut child = Child(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "lock_transfer::tests::handoff_child",
                    "--ignored",
                ])
                .env_clear()
                .env("TREER_TEST_LOCK_SOCKET", &path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        child.0.try_wait().unwrap().is_none(),
                        "owned child exited before handoff"
                    );
                    assert!(
                        std::time::Instant::now() < deadline,
                        "owned child did not connect"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("owned socket accept failed: {error}"),
            }
        };
        // Darwin accept inherits the nonblocking listener flag. This dedicated
        // std socket handshake uses bounded blocking I/O, not an async reactor.
        socket.set_nonblocking(false).unwrap();
        send(&socket, &locks.iter().collect::<Vec<_>>()).unwrap();
        socket.set_read_timeout(Some(TIMEOUT)).unwrap();
        let mut ready = [0];
        socket.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"R");
        drop(locks);
        for file in &contenders {
            assert_locked(file);
        }
        drop(child.0.stdin.take());
        assert!(child.0.wait().unwrap().success());
        for file in &contenders {
            rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        }
    }
}
