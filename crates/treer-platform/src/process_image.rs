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
