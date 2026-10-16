//! Handle-relative binary installation mechanics. This is not an update engine:
//! callers authenticate bytes, retain transaction locks and journal each rename.
//! A same-account process is trusted; permissions do not isolate workspaces.
use std::{fs::File, io, path::Path};

use rustix::fs::{self as sys, FileType, Mode, OFlags};

use crate::private_directory::{name, open_physical_directory, PrivateDirectory};

fn check(file: &File, directory: bool) -> io::Result<()> {
    let stat = sys::fstat(file)?;
    let expected = if directory {
        FileType::Directory
    } else {
        FileType::RegularFile
    };
    if FileType::from_raw_mode(stat.st_mode) != expected
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o7022 != 0
        || (!directory && stat.st_nlink != 1)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe binary installation",
        ));
    }
    Ok(())
}

/// An owned installation parent (typically 0755, not a private state directory).
/// Open all components without following symlinks and retain the final handle.
pub struct InstallationDirectory {
    file: File,
}

impl InstallationDirectory {
    /// Physical ordering key for taking more than one installation-parent lock.
    /// Shared CLI and Controller parents may differ; order by object identity,
    /// not caller-selected workspace/state paths, to avoid inverse lock order.
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let metadata = self.file.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }

    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = open_physical_directory(path)?;
        check(&file, true)?;
        Ok(Self { file })
    }

    /// The lock is anchored beside the shared binaries, independent of workspace
    /// or state-root selection. Never delete this directory or lock filename.
    /// Acquire this BEFORE any enrollment lock, including recovery workers.
    pub fn lock(&self) -> io::Result<File> {
        const CHILD: &str = ".treer-self-update";
        match sys::mkdirat(&self.file, CHILD, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
        let file: File = sys::openat(
            &self.file,
            CHILD,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        let directory = PrivateDirectory::from_handle(file)?;
        directory.sync()?;
        self.file.sync_all()?;
        let lock = directory.lock("installation.lock")?;
        directory.sync()?;
        Ok(lock)
    }

    /// Exclusive non-executable sibling stage. Do not execute or publish until
    /// the caller has verified the full digest, closed writers and synced it.
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

    /// Replace an existing checked file atomically without unlinking it first.
    /// Only executable stages are publishable. A sync failure after rename is
    /// ambiguous: recover by checking actual digests against the durable intent.
    pub fn replace_executable(&self, stage: &str, destination: &str) -> io::Result<()> {
        if name(stage)? == name(destination)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "distinct binary names required",
            ));
        }
        let source = self.read_file(stage)?;
        self.read_file(destination)?;
        if sys::fstat(&source)?.st_mode & 0o100 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "binary stage is not executable",
            ));
        }
        source.sync_all()?;
        sys::renameat(&self.file, stage, &self.file, destination)?;
        self.file.sync_all()
    }

    /// Remove only a caller-registered temporary regular file, never aliases.
    pub fn remove_file(&self, child: &str) -> io::Result<()> {
        self.read_file(child)?;
        sys::unlinkat(&self.file, name(child)?, sys::AtFlags::empty())?;
        self.file.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
        path::PathBuf,
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "treer-installation-{}-{}",
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
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn different_workspaces_share_the_installation_lock() {
        let f = Fixture::new();
        std::fs::set_permissions(&f.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(PrivateDirectory::open(&f.0).is_err());
        let a = InstallationDirectory::open(&f.0).unwrap();
        let b = InstallationDirectory::open(&f.0).unwrap();
        let held = a.lock().unwrap();
        assert!(b.lock().is_err());
        drop(held);
        assert!(b.lock().is_ok());
        assert_eq!(
            std::fs::metadata(f.0.join(".treer-self-update"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(f.0.join(".treer-self-update/installation.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn publication_is_atomic_and_preserves_the_open_old_image() {
        let f = Fixture::new();
        let dir = InstallationDirectory::open(&f.0).unwrap();
        let _lock = dir.lock().unwrap();
        dir.create_file("treer").unwrap().write_all(b"old").unwrap();
        let mut old = dir.read_file("treer").unwrap();
        let mut writer = dir.create_file("staged").unwrap();
        writer.write_all(b"verified").unwrap();
        writer.sync_all().unwrap();
        assert!(dir.replace_executable("staged", "treer").is_err());
        writer
            .set_permissions(std::fs::Permissions::from_mode(0o755))
            .unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        dir.replace_executable("staged", "treer").unwrap();
        let mut before = String::new();
        old.read_to_string(&mut before).unwrap();
        assert_eq!(before, "old");
        assert_eq!(std::fs::read(f.0.join("treer")).unwrap(), b"verified");
        assert!(!f.0.join("staged").exists());
        assert!(dir.replace_executable("treer", "treer").is_err());
    }

    #[test]
    fn rejects_aliases_insecure_modes_and_unexpected_existing_stages() {
        let f = Fixture::new();
        let dir = InstallationDirectory::open(&f.0).unwrap();
        dir.create_file("treer").unwrap();
        assert!(dir.create_file("treer").is_err());
        symlink("treer", f.0.join("alias")).unwrap();
        assert!(dir.read_file("alias").is_err());
        assert!(dir.remove_file("alias").is_err());
        std::fs::hard_link(f.0.join("treer"), f.0.join("hard")).unwrap();
        assert!(dir.read_file("hard").is_err());
        assert!(dir.read_file("treer").is_err());
        for child in ["../escape", "", "a/b", "a\\b", "a:b"] {
            assert!(dir.create_file(child).is_err());
        }
        symlink("treer", f.0.join(".treer-self-update")).unwrap();
        assert!(dir.lock().is_err());
        std::fs::set_permissions(&f.0, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(InstallationDirectory::open(&f.0).is_err());
    }

    #[test]
    fn retained_parent_does_not_follow_a_replaced_path() {
        let f = Fixture::new();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(f.0.join("bin"))
            .unwrap();
        let dir = InstallationDirectory::open(&f.0.join("bin")).unwrap();
        std::fs::rename(f.0.join("bin"), f.0.join("old-bin")).unwrap();
        std::fs::create_dir(f.0.join("other")).unwrap();
        symlink("other", f.0.join("bin")).unwrap();
        assert!(InstallationDirectory::open(&f.0.join("bin")).is_err());
        dir.create_file("stage").unwrap();
        let _lock = dir.lock().unwrap();
        assert!(f.0.join("old-bin/stage").exists());
        assert!(f
            .0
            .join("old-bin/.treer-self-update/installation.lock")
            .exists());
        assert!(!f.0.join("other/stage").exists());
    }
}
