//! Inherit an already-owned OS lock across fork into a trusted supervisor.
//! Descriptor mechanics only: no lock acquisition, child identity, execution
//! permit, negative liveness evidence, process containment or recovery authority.
use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, OwnedFd, RawFd},
    os::unix::process::CommandExt,
    process::{Child, Command},
};

use rustix::io::{fcntl_dupfd_cloexec, fcntl_getfd, fcntl_setfd, FdFlags};

/// A single-use fork inheritance of the caller's held lock.
/// State and job directories must be on a local filesystem with native flock
/// semantics; network filesystems are unsupported.
///
/// Duplicate the caller's held lock using CLOEXEC atomically. The duplicate
/// shares its open-file description (and flock ownership), not a reopened file.
/// It remains CLOEXEC in the parent and unrelated concurrent execs. Only the
/// intended child's pre_exec clears that flag, after fork inherited the lock.
///
/// The descriptor number is a locator for the trusted supervisor's private
/// bootstrap, NOT ownership evidence. Spawn consumes both this value and the
/// command, releasing the parent's duplicate after success or failure.
/// Callers must hold original ownership, authenticate
/// the supervisor and restore CLOEXEC in it before any installer child exec.
/// This helper does not authorize the command.
///
/// ```compile_fail,E0382
/// use std::{fs::File, io, process::Command};
/// use treer_platform::birth_lock::BirthLockInheritance;
/// fn cannot_replay(held: &File) -> io::Result<()> {
///     let inheritance = BirthLockInheritance::new(held)?;
///     inheritance.spawn(Command::new("/trusted/supervisor"))?;
///     inheritance.spawn(Command::new("/trusted/supervisor"))?; // consumed
///     Ok(())
/// }
/// ```
pub struct BirthLockInheritance {
    inherited: OwnedFd,
}

impl BirthLockInheritance {
    pub fn new(held_lock: &File) -> io::Result<Self> {
        Ok(Self {
            inherited: fcntl_dupfd_cloexec(held_lock, 3)?,
        })
    }

    pub fn descriptor(&self) -> RawFd {
        self.inherited.as_raw_fd()
    }

    pub fn spawn(self, mut command: Command) -> io::Result<Child> {
        self.configure(&mut command);
        command.spawn()
    }

    /// Preserve the same single-use configuration through Tokio's std adapter.
    pub fn spawn_tokio(
        self,
        mut command: tokio::process::Command,
    ) -> io::Result<tokio::process::Child> {
        self.configure(command.as_std_mut());
        command.spawn()
    }

    fn configure(self, command: &mut Command) {
        let inherited = self.inherited;
        // SAFETY: the post-fork callback only calls async-signal-safe fcntl on
        // an owned, already-duplicated descriptor, without allocation/locking.
        // Command owns the captured fd; both consuming spawn adapters drop the
        // command before returning, preventing reuse of this configuration.
        unsafe {
            command.pre_exec(move || {
                let mut flags = fcntl_getfd(&inherited)?;
                flags.remove(FdFlags::CLOEXEC);
                fcntl_setfd(&inherited, flags)?;
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_directory::PrivateDirectory;
    use std::{
        io::{BufRead, Write},
        os::fd::BorrowedFd,
        os::unix::fs::{DirBuilderExt, MetadataExt},
        path::PathBuf,
        process::{Child, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    fn assert_lock_eventually_available(directory: &PrivateDirectory) {
        // Concurrent test forks can briefly inherit CLOEXEC descriptors until
        // their exec. Availability is only a fixture assertion, not recovery
        // authority; tolerate that bounded fork-to-exec window.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match directory.lock("birth.lock") {
                Ok(_) => return,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "lock remained held");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("unexpected lock error: {error}"),
            }
        }
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "treer-birth-lock-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    #[test]
    fn descendant_birth_lock_probe() {
        let Some(value) = std::env::var_os("TREER_TEST_CLOSED_BIRTH_FD") else {
            return;
        };
        let raw: RawFd = value.to_str().unwrap().parse().unwrap();
        let expected_device: u64 = std::env::var("TREER_TEST_BIRTH_DEV")
            .unwrap()
            .parse()
            .unwrap();
        let expected_inode: u64 = std::env::var("TREER_TEST_BIRTH_INO")
            .unwrap()
            .parse()
            .unwrap();
        let expected_present = std::env::var("TREER_TEST_BIRTH_EXPECT_PRESENT").unwrap() == "true";
        match std::fs::metadata(format!("/dev/fd/{raw}")) {
            Ok(metadata) => assert_eq!(
                (metadata.dev(), metadata.ino()) == (expected_device, expected_inode),
                expected_present,
                "unexpected birth lock inheritance"
            ),
            Err(error) => {
                assert!(!expected_present, "expected inherited lock is missing");
                assert!(
                    error.kind() == io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(rustix::io::Errno::BADF.raw_os_error()),
                    "unexpected descriptor metadata error: {error}"
                );
            }
        }
    }

    #[test]
    fn inherited_holder() {
        let Some(value) = std::env::var_os("TREER_TEST_INHERITED_BIRTH_FD") else {
            return;
        };
        let raw: RawFd = value.to_str().unwrap().parse().unwrap();
        assert!(raw >= 3);
        // SAFETY: only this isolated test parent supplies the descriptor that
        // its BirthLockInheritance passed across exec; borrow never closes it.
        let inherited = unsafe { BorrowedFd::borrow_raw(raw) };
        assert!(!fcntl_getfd(inherited).unwrap().contains(FdFlags::CLOEXEC));
        // First establish the probe detects inheritance, then restore CLOEXEC
        // before the second substitute child, as the real supervisor must.
        for expected_present in [true, false] {
            if !expected_present {
                fcntl_setfd(inherited, FdFlags::CLOEXEC).unwrap();
            }
            let metadata = std::fs::metadata(format!("/dev/fd/{raw}")).unwrap();
            let mut probe = OwnedChild(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "birth_lock::tests::descendant_birth_lock_probe",
                        "--nocapture",
                    ])
                    .env_clear()
                    .env("TREER_TEST_CLOSED_BIRTH_FD", raw.to_string())
                    .env("TREER_TEST_BIRTH_DEV", metadata.dev().to_string())
                    .env("TREER_TEST_BIRTH_INO", metadata.ino().to_string())
                    .env(
                        "TREER_TEST_BIRTH_EXPECT_PRESENT",
                        expected_present.to_string(),
                    )
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = probe.0.try_wait().unwrap() {
                    assert!(status.success(), "descendant descriptor probe failed");
                    break;
                }
                assert!(Instant::now() < deadline, "descendant probe timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        std::io::stdout().write_all(b"inherited-ready\n").unwrap();
        std::io::stdout().flush().unwrap();
        let mut byte = [0];
        std::io::Read::read_exact(&mut std::io::stdin(), &mut byte).unwrap();
        std::process::exit(0); // OS closes held fd, without Rust destructors.
    }

    #[test]
    fn child_keeps_same_lock_after_all_parent_copies_drop_until_exit_or_sigkill() {
        for sigkill in [false, true] {
            let fixture = Fixture::new();
            let directory = PrivateDirectory::open(&fixture.0).unwrap();
            let held = directory.lock("birth.lock").unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "birth_lock::tests::inherited_holder",
                    "--nocapture",
                ])
                .env_clear()
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            let inheritance = BirthLockInheritance::new(&held).unwrap();
            let descriptor = inheritance.descriptor();
            command.env("TREER_TEST_INHERITED_BIRTH_FD", descriptor.to_string());
            {
                // SAFETY: inheritance still owns this descriptor in the parent.
                let borrowed = unsafe { BorrowedFd::borrow_raw(descriptor) };
                assert!(fcntl_getfd(borrowed).unwrap().contains(FdFlags::CLOEXEC));
            }
            let mut child = OwnedChild(inheritance.spawn(command).unwrap());
            drop(held);
            let output = child.0.stdout.take().unwrap();
            let (send, receive) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in std::io::BufReader::new(output).lines() {
                    if line.unwrap() == "inherited-ready" {
                        let _ = send.send(());
                        return;
                    }
                }
            });
            receive.recv_timeout(Duration::from_secs(5)).unwrap();
            reader.join().unwrap();
            assert!(directory.lock("birth.lock").is_err());
            if sigkill {
                child.0.kill().unwrap();
            } else {
                child.0.stdin.take().unwrap().write_all(b"x").unwrap();
            }
            let status = child.0.wait().unwrap();
            assert_eq!(status.success(), !sigkill);
            assert_lock_eventually_available(&directory);
        }
    }

    #[test]
    fn unspawned_inheritance_keeps_parent_cloexec_copy_then_releases_on_drop() {
        let fixture = Fixture::new();
        let directory = PrivateDirectory::open(&fixture.0).unwrap();
        let held = directory.lock("birth.lock").unwrap();
        let inheritance = BirthLockInheritance::new(&held).unwrap();
        let descriptor = inheritance.descriptor();
        {
            // SAFETY: inheritance owns the live descriptor returned immediately above.
            let borrowed = unsafe { BorrowedFd::borrow_raw(descriptor) };
            assert!(fcntl_getfd(borrowed).unwrap().contains(FdFlags::CLOEXEC));
        }
        drop(held);
        assert!(directory.lock("birth.lock").is_err());
        drop(inheritance);
        assert_lock_eventually_available(&directory);
    }

    #[test]
    fn failed_spawn_releases_parent_duplicate() {
        let fixture = Fixture::new();
        let directory = PrivateDirectory::open(&fixture.0).unwrap();
        let held = directory.lock("birth.lock").unwrap();
        let inheritance = BirthLockInheritance::new(&held).unwrap();
        drop(held);
        assert!(directory.lock("birth.lock").is_err());
        assert!(inheritance
            .spawn(Command::new(fixture.0.join("missing-supervisor")))
            .is_err());
        assert_lock_eventually_available(&directory);
    }

    #[tokio::test]
    async fn tokio_adapter_preserves_inheritance_and_descendant_cloexec() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let fixture = Fixture::new();
        let directory = PrivateDirectory::open(&fixture.0).unwrap();
        let held = directory.lock("birth.lock").unwrap();
        let inheritance = BirthLockInheritance::new(&held).unwrap();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "birth_lock::tests::inherited_holder",
                "--nocapture",
            ])
            .env_clear()
            .env(
                "TREER_TEST_INHERITED_BIRTH_FD",
                inheritance.descriptor().to_string(),
            )
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = inheritance.spawn_tokio(command).unwrap();
        drop(held);
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let line = lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("supervisor exited early");
                if line == "inherited-ready" {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(directory.lock("birth.lock").is_err());
        child.stdin.take().unwrap().write_all(b"x").await.unwrap();
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
        assert_lock_eventually_available(&directory);
    }
}
