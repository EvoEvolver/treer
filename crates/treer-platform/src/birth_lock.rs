//! Inherit an already-owned OS lock across fork into a trusted supervisor.
//! Descriptor mechanics only: no lock acquisition, child identity, execution
//! permit, negative liveness evidence, process containment or recovery authority.
use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, RawFd},
    os::unix::process::CommandExt,
    process::Command,
};

use rustix::io::{fcntl_dupfd_cloexec, fcntl_getfd, fcntl_setfd, FdFlags};

/// Duplicate the caller's held lock using CLOEXEC atomically. The duplicate
/// shares its open-file description (and flock ownership), not a reopened file.
/// It remains CLOEXEC in the parent and unrelated concurrent execs. Only the
/// intended child's pre_exec clears that flag, after fork inherited the lock.
///
/// The returned number is a locator for the trusted supervisor's private
/// bootstrap, NOT ownership evidence. Command retains the duplicate until Drop,
/// even after spawn: drop it when no longer needed, and do not reuse the command
/// to launch another attempt. Callers must hold original ownership, authenticate
/// the supervisor and restore CLOEXEC in it before any installer child exec.
/// This helper neither spawns nor authorizes the command being configured.
pub fn prepare_inheritance(command: &mut Command, held_lock: &File) -> io::Result<RawFd> {
    let inherited = fcntl_dupfd_cloexec(held_lock, 3)?;
    let descriptor = inherited.as_raw_fd();
    // SAFETY: the post-fork callback only calls async-signal-safe fcntl on an
    // owned, already-duplicated descriptor, with no allocation or locking.
    // The owning descriptor is captured to stay alive until Command is dropped.
    unsafe {
        command.pre_exec(move || {
            let mut flags = fcntl_getfd(&inherited)?;
            flags.remove(FdFlags::CLOEXEC);
            fcntl_setfd(&inherited, flags)?;
            Ok(())
        });
    }
    Ok(descriptor)
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
        time::Duration,
    };

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
        // its prepare_inheritance passed across exec; borrow never closes it.
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
            let descriptor = prepare_inheritance(&mut command, &held).unwrap();
            command.env("TREER_TEST_INHERITED_BIRTH_FD", descriptor.to_string());
            let mut child = {
                // SAFETY: Command owns this still-live descriptor in the parent.
                let borrowed = unsafe { BorrowedFd::borrow_raw(descriptor) };
                assert!(fcntl_getfd(borrowed).unwrap().contains(FdFlags::CLOEXEC));
                let child = OwnedChild(command.spawn().unwrap());
                assert!(fcntl_getfd(borrowed).unwrap().contains(FdFlags::CLOEXEC));
                child
            };
            drop(command);
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
            assert!(directory.lock("birth.lock").is_ok());
        }
    }

    #[test]
    fn unspawned_command_keeps_parent_cloexec_copy_then_releases_on_drop() {
        let fixture = Fixture::new();
        let directory = PrivateDirectory::open(&fixture.0).unwrap();
        let held = directory.lock("birth.lock").unwrap();
        let mut command = Command::new("/never/executed");
        let descriptor = prepare_inheritance(&mut command, &held).unwrap();
        {
            // SAFETY: same live Command-owned descriptor as returned immediately above.
            let borrowed = unsafe { BorrowedFd::borrow_raw(descriptor) };
            assert!(fcntl_getfd(borrowed).unwrap().contains(FdFlags::CLOEXEC));
        }
        drop(held);
        assert!(directory.lock("birth.lock").is_err());
        drop(command);
        assert!(directory.lock("birth.lock").is_ok());
    }
}
