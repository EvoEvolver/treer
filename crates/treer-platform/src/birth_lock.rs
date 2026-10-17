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
        let inherited = self.inherited;
        // SAFETY: the post-fork callback only calls async-signal-safe fcntl on
        // an owned, already-duplicated descriptor, without allocation/locking.
        // Command owns the captured fd and is dropped before returning, so no
        // caller can reuse that command to inherit the lock a second time.
        unsafe {
            command.pre_exec(move || {
                let mut flags = fcntl_getfd(&inherited)?;
                flags.remove(FdFlags::CLOEXEC);
                fcntl_setfd(&inherited, flags)?;
                Ok(())
            });
        }
        command.spawn()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_directory::PrivateDirectory;
    use std::{
        io::{BufRead, Write},
        os::fd::BorrowedFd,
        os::unix::fs::DirBuilderExt,
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
        // A real supervisor must do this before creating installer children.
        fcntl_setfd(inherited, FdFlags::CLOEXEC).unwrap();
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
                .stderr(Stdio::null());
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
}
