//! OS-reported process creation identity and executable file, without ps/PATH,
//! argv or environment reads. This is not isolation from a same-account writer
//! or attestation of every mapped code page; one OS account is one trust domain.
use std::{fs::File, io, os::unix::fs::MetadataExt};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "process image identity unavailable",
    )
}

/// Called by the owned Unix worker after exec, before receiving descriptors or
/// starting a runtime. No pre_exec/fork callback, shell, or external setsid tool.
/// A process-group leader cannot detach this way: refuse rather than continuing
/// in the submitter's session. This alone is not proof of handoff or ownership.
pub fn enter_private_session() -> io::Result<()> {
    let own = rustix::process::getpid();
    if rustix::process::setsid()? != own
        || rustix::process::getsid(None)? != own
        || rustix::process::getpgid(None)? != own
    {
        return Err(invalid());
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
struct Identity {
    creation: String,
    parent: u32,
}

/// Keeps an opened image inode, not a pathname which can later be replaced.
/// Intentionally no Debug/Serialize: process paths never enter diagnostics.
pub struct ProcessImage {
    pid: u32,
    creation: String,
    parent: u32,
    image: File,
    device: u64,
    inode: u64,
}

impl ProcessImage {
    pub fn capture(pid: u32) -> io::Result<Self> {
        if pid == 0 || pid > i32::MAX as u32 {
            return Err(invalid());
        }
        #[cfg(target_os = "linux")]
        let (creation, image, after) = {
            let creation = linux::creation(pid)?;
            // Deliberate kernel magic-link dereference: this identifies the
            // process's actual executable inode, even after rename/unlink.
            let image = File::open(format!("/proc/{pid}/exe"))?;
            (creation, image, linux::creation(pid)?)
        };
        #[cfg(target_os = "macos")]
        let (creation, image, after) = {
            let creation = macos::creation(pid)?;
            let path = macos::image_path(pid)?;
            let mapped_identity = macos::mapped_image_identity(pid, &path)?;
            let parent = path.parent().ok_or_else(invalid)?;
            let directory = super::private_directory::open_physical_directory(parent)?;
            let image: File = rustix::fs::openat(
                directory,
                path.file_name().ok_or_else(invalid)?,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )?
            .into();
            let metadata = image.metadata()?;
            if macos::image_path(pid)? != path
                // Darwin dev_t is 32 bits; MetadataExt may sign-extend it to
                // u64, while libproc's vnode stat exposes the unsigned bits.
                || (metadata.dev() as u32, metadata.ino()) != mapped_identity
                || macos::mapped_image_identity(pid, &path)? != mapped_identity
            {
                return Err(invalid());
            }
            (creation, image, macos::creation(pid)?)
        };
        if creation != after {
            return Err(invalid());
        }
        let metadata = image.metadata()?;
        if !metadata.is_file()
            || !matches!(metadata.uid(), uid if uid == 0 || uid == rustix::process::geteuid().as_raw())
            || metadata.mode() & 0o022 != 0
            || metadata.mode() & 0o111 == 0
        {
            return Err(invalid());
        }
        Ok(Self {
            pid,
            creation: creation.creation,
            parent: creation.parent,
            image,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn creation_identity(&self) -> &str {
        &self.creation
    }
    pub fn executable(&self) -> &File {
        &self.image
    }

    /// Re-observe the direct OS parent, with both retained process identities
    /// still live. A saved PPID or an executable shared by siblings is not proof.
    /// This is point-in-time provenance, not containment or writer quiescence.
    pub fn verify_parent(&self, parent: &Self) -> io::Result<()> {
        parent.verify_current()?;
        for _ in 0..2 {
            let current = Self::capture(self.pid)?;
            if current.creation != self.creation
                || current.device != self.device
                || current.inode != self.inode
                || current.parent != parent.pid
                || self.pid == parent.pid
            {
                return Err(invalid());
            }
            parent.verify_current()?;
        }
        Ok(())
    }

    /// Enumerate only this parent's direct children, not unrelated same-user
    /// processes. Recheck PPID on each captured candidate because process lists
    /// can race exit/reparenting. Exited/exec-racing candidates are not adopted.
    pub fn children(&self) -> io::Result<Vec<Self>> {
        self.verify_current()?;
        #[cfg(target_os = "macos")]
        let pids = macos::children(self.pid)?;
        #[cfg(target_os = "linux")]
        let pids = linux::children(self.pid)?;
        let children = pids
            .into_iter()
            .filter_map(|pid| Self::capture(pid).ok())
            .filter(|image| image.parent == self.pid)
            .collect();
        self.verify_current()?;
        Ok(children)
    }

    pub fn matches_file(&self, file: &File) -> io::Result<bool> {
        let metadata = file.metadata()?;
        Ok(metadata.is_file() && metadata.dev() == self.device && metadata.ino() == self.inode)
    }

    /// A separate session and process group must both be led by this exact
    /// live process. Recheck creation/image identity around the OS queries;
    /// a saved PID/session number or merely a new process group is insufficient.
    /// No claim about inherited descriptors, cgroups or service-manager scope.
    pub fn verify_private_session(&self) -> io::Result<()> {
        self.verify_current()?;
        let pid = rustix::process::Pid::from_raw(self.pid as i32).ok_or_else(invalid)?;
        if rustix::process::getsid(Some(pid))? != pid || rustix::process::getpgid(Some(pid))? != pid
        {
            return Err(invalid());
        }
        self.verify_current()
    }

    /// Re-observe after hashing/other async work. A live PID alone is not enough:
    /// PID reuse and exec of a different inode must both fail this comparison.
    pub fn verify_current(&self) -> io::Result<()> {
        let current = Self::capture(self.pid)?;
        if current.creation != self.creation
            || current.device != self.device
            || current.inode != self.inode
        {
            return Err(invalid());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, Write},
        process::{Command, Stdio},
    };

    #[test]
    fn private_session_is_verified_on_an_owned_process_not_a_group_only() {
        use std::{
            os::unix::{fs::DirBuilderExt, process::CommandExt},
            time::{Duration, Instant, SystemTime, UNIX_EPOCH},
        };
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        for group_only in [false, true] {
            let path = std::env::temp_dir().join(format!(
                "treer-session-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            let directory = Directory(path);
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "process_image::tests::private_session_child",
                    "--ignored",
                ])
                .env_clear()
                .env("TREER_TEST_SESSION_DIR", &directory.0)
                .env(
                    "TREER_TEST_SESSION_GROUP_ONLY",
                    if group_only { "1" } else { "0" },
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            if group_only {
                command.process_group(0);
            }
            let mut child = Child(command.spawn().unwrap());
            let deadline = Instant::now() + Duration::from_secs(5);
            while !directory.0.join("ready").exists() {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "session child exited early"
                );
                assert!(
                    Instant::now() < deadline,
                    "session child did not become ready"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let image = ProcessImage::capture(child.0.id()).unwrap();
            let parent = ProcessImage::capture(std::process::id()).unwrap();
            image.verify_parent(&parent).unwrap();
            assert!(image.verify_parent(&image).is_err());
            assert!(parent.verify_parent(&image).is_err());
            let mut stale_parent = ProcessImage::capture(std::process::id()).unwrap();
            stale_parent.creation.push_str(":stale");
            assert!(image.verify_parent(&stale_parent).is_err());
            let mut sibling = Child(command.spawn().unwrap());
            // A real live sibling has the same UID and executable but cannot
            // be substituted for the parent. Keep its owned handle until done.
            let sibling_image = ProcessImage::capture(sibling.0.id()).unwrap();
            assert!(image.verify_parent(&sibling_image).is_err());
            assert!(sibling.0.try_wait().unwrap().is_none());
            assert_eq!(image.verify_private_session().is_ok(), !group_only);
            std::fs::write(directory.0.join("release"), b"release").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success(), "session child failed");
                    break;
                }
                assert!(Instant::now() < deadline, "session child did not exit");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                image.verify_private_session().is_err(),
                "exited identity refused"
            );
            assert!(image.verify_parent(&parent).is_err());
        }
    }

    #[test]
    #[ignore = "isolated process, invoked by private_session_is_verified_on_an_owned_process_not_a_group_only"]
    fn private_session_child() {
        use std::time::{Duration, Instant};
        let directory =
            std::path::PathBuf::from(std::env::var_os("TREER_TEST_SESSION_DIR").unwrap());
        let group_only = std::env::var("TREER_TEST_SESSION_GROUP_ONLY").unwrap() == "1";
        let before = ProcessImage::capture(std::process::id()).unwrap();
        assert!(before.verify_private_session().is_err());
        if group_only {
            assert!(
                enter_private_session().is_err(),
                "no fallback for a group leader"
            );
            assert!(before.verify_private_session().is_err());
        } else {
            enter_private_session().unwrap();
            before.verify_private_session().unwrap();
            assert!(
                enter_private_session().is_err(),
                "second detach fails closed"
            );
            before.verify_private_session().unwrap();
        }
        std::fs::write(directory.join("ready"), b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !directory.join("release").exists() {
            assert!(
                Instant::now() < deadline,
                "parent did not release session fixture"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn different_boot_component_rejects_same_pid_start_time_and_image() {
        let mut image = ProcessImage::capture(std::process::id()).unwrap();
        image.verify_current().unwrap();
        let original = image.creation.clone();
        let mut parts: Vec<_> = original.split(':').collect();
        #[cfg(target_os = "linux")]
        {
            assert_eq!(parts.len(), 3);
            parts[1] = if parts[1] == "00000000-0000-0000-0000-000000000000" {
                "11111111-1111-1111-1111-111111111111"
            } else {
                "00000000-0000-0000-0000-000000000000"
            };
        }
        #[cfg(target_os = "macos")]
        {
            assert_eq!(parts.len(), 5);
            parts[1] = if parts[1] == "1" { "2" } else { "1" };
        }
        // Keep PID, process start fields and retained executable unchanged;
        // only the saved boot component is forged, never the machine clock.
        image.creation = parts.join(":");
        assert!(image.verify_current().is_err());
        image.creation = original;
        image.verify_current().unwrap();
    }

    #[test]
    fn self_identity_uses_a_real_opened_image_and_rejects_invalid_pids() {
        let image = ProcessImage::capture(std::process::id()).unwrap();
        assert_eq!(image.pid(), std::process::id());
        assert!(!image.creation_identity().is_empty());
        assert!(image.executable().metadata().unwrap().is_file());
        image.verify_current().unwrap();
        assert!(ProcessImage::capture(0).is_err());
        assert!(ProcessImage::capture(u32::MAX).is_err());
    }

    #[test]
    fn exited_process_and_exec_to_another_inode_do_not_keep_the_old_identity() {
        // This child belongs solely to this test; no real Host or service is
        // inspected/signalled. The shell blocks until the test requests exec.
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf 'ready\\n'; read value; exec /bin/sleep 30"])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let result = (|| -> io::Result<()> {
            let mut line = String::new();
            std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line)?;
            if line != "ready\n" {
                return Err(invalid());
            }
            let before = ProcessImage::capture(child.id())?;
            let parent = ProcessImage::capture(std::process::id())?;
            if !parent.children()?.iter().any(|image| {
                image.pid() == child.id() && image.creation_identity() == before.creation_identity()
            }) {
                return Err(invalid());
            }
            child.stdin.take().unwrap().write_all(b"continue\n")?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let after = loop {
                if let Ok(after) = ProcessImage::capture(child.id()) {
                    if after.device != before.device || after.inode != before.inode {
                        break after;
                    }
                }
                if child.try_wait()?.is_some() {
                    return Err(invalid());
                }
                if std::time::Instant::now() >= deadline {
                    return Err(invalid());
                }
                std::thread::yield_now();
            };
            if before.creation != after.creation || before.verify_current().is_ok() {
                return Err(invalid());
            }
            Ok(())
        })();
        // Always reap the exact test-owned child, including on an assertion's
        // later failure. No kill-by-name or shared service manipulation.
        let _ = child.kill();
        let pid = child.id();
        child.wait().unwrap();
        result.unwrap();
        assert!(ProcessImage::capture(pid).is_err());
    }
}
