//! Handle-relative private transaction storage for local maintenance.
//!
//! This prevents following filesystem aliases; it does not isolate mutually
//! untrusted processes running as the same OS user.
use std::{
    fs::File,
    io,
    path::{Component, Path},
};

use rustix::fs::{self as sys, AtFlags, FileType, Mode, OFlags};

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "unsafe private transaction storage",
    )
}

pub(super) fn name(value: &str) -> io::Result<&str> {
    if value.is_empty()
        || value.len() > 255
        || matches!(value, "." | "..")
        || value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a single storage name",
        ));
    }
    Ok(value)
}

fn check(file: &File, directory: bool) -> io::Result<()> {
    let stat = sys::fstat(file)?;
    let expected = if directory {
        FileType::Directory
    } else {
        FileType::RegularFile
    };
    if FileType::from_raw_mode(stat.st_mode) != expected
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o077 != 0
        || (!directory && stat.st_nlink != 1)
    {
        return Err(denied());
    }
    Ok(())
}

/// An owned 0700 directory; all mutations retain the checked parent handle.
pub struct PrivateDirectory {
    file: File,
}

impl PrivateDirectory {
    pub(super) fn from_handle(file: File) -> io::Result<Self> {
        check(&file, true)?;
        Ok(Self { file })
    }
    /// Persist directory entries after creating transaction files.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    /// Open an existing absolute physical directory, rejecting every symlink
    /// component. Callers canonicalize trusted OS aliases before selecting it.
    /// This never chmods a pre-existing directory or creates missing ancestors.
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::from_handle(open_physical_directory(path)?)
    }

    pub fn open_child(&self, child: &str) -> io::Result<Self> {
        let file: File = sys::openat(
            &self.file,
            name(child)?,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        check(&file, true)?;
        Ok(Self { file })
    }

    /// Exclusive creation: an existing job must be opened/recovered explicitly.
    pub fn create_child(&self, child: &str) -> io::Result<Self> {
        sys::mkdirat(
            &self.file,
            name(child)?,
            Mode::RUSR | Mode::WUSR | Mode::XUSR,
        )?;
        let result = self.open_child(child)?;
        result.file.sync_all()?;
        self.file.sync_all()?;
        Ok(result)
    }

    pub fn create_file(&self, child: &str) -> io::Result<File> {
        let file: File = sys::openat(
            &self.file,
            name(child)?,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?
        .into();
        check(&file, false)?;
        Ok(file)
    }

    /// Reject devices, FIFOs, symlinks and hard-link aliases before reading.
    pub fn read_file(&self, child: &str) -> io::Result<File> {
        let file: File = sys::openat(
            &self.file,
            name(child)?,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        check(&file, false)?;
        Ok(file)
    }

    /// OS advisory lock, released with the returned handle (including on exit).
    /// Lock names must be stable, and must never be unlinked while in use.
    pub fn lock(&self, child: &str) -> io::Result<File> {
        let file: File = sys::openat(
            &self.file,
            name(child)?,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?
        .into();
        check(&file, false)?;
        sys::flock(&file, sys::FlockOperation::NonBlockingLockExclusive)?;
        Ok(file)
    }

    /// Source must already be fully written and synced. This never removes the
    /// old destination first. A directory-sync error means publication may have
    /// happened: callers must reconcile, not blindly resubmit.
    pub fn replace_file(&self, temporary: &str, destination: &str) -> io::Result<()> {
        let source = self.read_file(temporary)?;
        source.sync_all()?;
        match sys::statat(&self.file, name(destination)?, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => {
                self.read_file(destination)?;
            }
            Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(error.into()),
        }
        sys::renameat(&self.file, name(temporary)?, &self.file, destination)?;
        self.file.sync_all()
    }

    /// Only an exact owned regular filename is removable, never a directory or
    /// link. The caller must separately prove it created/registered this name.
    pub fn remove_file(&self, child: &str) -> io::Result<()> {
        self.read_file(child)?;
        sys::unlinkat(&self.file, name(child)?, AtFlags::empty())?;
        self.file.sync_all()
    }
}

pub(super) fn open_physical_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(denied());
    }
    let mut file: File = sys::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(component) => {
                file = sys::openat(
                    &file,
                    component,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?
                .into();
            }
            _ => return Err(denied()),
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::fs::{symlink, PermissionsExt},
        path::PathBuf,
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "treer-private-directory-{}-{}",
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
    use std::os::unix::fs::DirBuilderExt;
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn locks_are_exclusive_and_release_on_drop() {
        let f = Fixture::new();
        let root = PrivateDirectory::open(&f.0).unwrap();
        let lock = root.lock("installation.lock").unwrap();
        assert!(root.lock("installation.lock").is_err());
        drop(lock);
        assert!(root.lock("installation.lock").is_ok());
    }

    #[test]
    fn receipt_replacement_preserves_open_reader_and_private_mode() {
        let f = Fixture::new();
        let root = PrivateDirectory::open(&f.0).unwrap();
        let job = root.create_child("job").unwrap();
        job.create_file("receipt")
            .unwrap()
            .write_all(b"prepared")
            .unwrap();
        let mut old = job.read_file("receipt").unwrap();
        job.create_file("next")
            .unwrap()
            .write_all(b"complete")
            .unwrap();
        job.replace_file("next", "receipt").unwrap();
        let mut text = String::new();
        old.read_to_string(&mut text).unwrap();
        assert_eq!(text, "prepared");
        assert_eq!(std::fs::read(f.0.join("job/receipt")).unwrap(), b"complete");
        assert_eq!(
            std::fs::metadata(f.0.join("job/receipt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(!f.0.join("job/next").exists());
    }

    #[test]
    fn rejects_aliases_nonprivate_storage_and_nonfiles_without_mutation() {
        let f = Fixture::new();
        let root = PrivateDirectory::open(&f.0).unwrap();
        root.create_file("original")
            .unwrap()
            .write_all(b"keep")
            .unwrap();
        std::fs::hard_link(f.0.join("original"), f.0.join("alias")).unwrap();
        assert!(root.read_file("alias").is_err());
        symlink(f.0.join("original"), f.0.join("link")).unwrap();
        assert!(root.read_file("link").is_err());
        assert!(root.lock("link").is_err());
        symlink(&f.0, f.0.join("directory-link")).unwrap();
        assert!(root.open_child("directory-link").is_err());
        assert!(PrivateDirectory::open(&f.0.join("directory-link")).is_err());
        for bad in ["", "..", "../outside", "a/b", "a\\b", "C:stream", "x\n"] {
            assert!(root.create_file(bad).is_err());
            assert!(root.open_child(bad).is_err());
        }
        root.create_file("next").unwrap().write_all(b"new").unwrap();
        assert!(root.replace_file("next", "link").is_err());
        assert_eq!(std::fs::read(f.0.join("original")).unwrap(), b"keep");
        assert!(f.0.join("next").exists());
        let public = f.0.join("public");
        std::fs::create_dir(&public).unwrap();
        std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(PrivateDirectory::open(&public).is_err());
        assert_eq!(
            std::fs::metadata(public).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn retained_parent_handle_does_not_follow_replaced_directory_name() {
        let f = Fixture::new();
        let root = PrivateDirectory::open(&f.0).unwrap();
        let job = root.create_child("job").unwrap();
        std::fs::rename(f.0.join("job"), f.0.join("moved")).unwrap();
        symlink(&f.0, f.0.join("job")).unwrap();
        job.create_file("receipt")
            .unwrap()
            .write_all(b"private")
            .unwrap();
        assert_eq!(
            std::fs::read(f.0.join("moved/receipt")).unwrap(),
            b"private"
        );
        assert!(!f.0.join("receipt").exists());
    }

    #[test]
    fn child_lock_holder() {
        let Some(path) = std::env::var_os("TREER_TEST_PRIVATE_DIRECTORY_LOCK") else {
            return;
        };
        let root = PrivateDirectory::open(Path::new(&path)).unwrap();
        let _lock = root.lock("installation.lock").unwrap();
        std::io::stdout().write_all(b"locked\n").unwrap();
        std::io::stdout().flush().unwrap();
        let mut command = [0];
        std::io::stdin().read_exact(&mut command).unwrap();
        // Deliberately skip Rust destructors: the OS must release the lock.
        std::process::exit(0);
    }

    #[test]
    fn process_exit_releases_the_os_lock_without_pid_file_recovery() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let f = Fixture::new();
        let root = PrivateDirectory::open(&f.0).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "private_directory::tests::child_lock_holder",
                "--nocapture",
            ])
            .env_clear()
            .env("TREER_TEST_PRIVATE_DIRECTORY_LOCK", &f.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert!(output.read_line(&mut line).unwrap() > 0);
            if line.trim() == "locked" {
                break;
            }
        }
        let excluded = root.lock("installation.lock").is_err();
        child.stdin.take().unwrap().write_all(b"x").unwrap();
        assert!(child.wait().unwrap().success());
        assert!(excluded);
        assert!(root.lock("installation.lock").is_ok());
    }
}
