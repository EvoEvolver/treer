//! Root-relative single-file operations. No product identity or authorization
//! belongs here: Controller must authorize before exposing handles or bytes.
use std::{fs::File, io};
use unicode_normalization::UnicodeNormalization;

fn reserved_name(value: &str) -> String {
    value.nfc().flat_map(char::to_uppercase).collect()
}

/// Classify only the portable reserved namespace, without touching the path.
/// Invalid wire paths remain errors, including when reading recovery metadata.
pub fn is_reserved_namespace(path: &str) -> io::Result<bool> {
    Ok(validate_relative_path(path)?.iter().any(|part| {
        let name = reserved_name(part);
        name == ".TREER" || name.starts_with(".TREER-TRANSFER-")
    }))
}
// Reserve the first missing Core component at an existing physical parent.
// Conservatively fold reserved names only; ordinary Linux paths remain distinct.
fn reserved_parent(path: &std::path::Path) -> io::Result<(File, String)> {
    let mut candidate = path;
    while let Some(parent) = candidate.parent() {
        match open_protected(parent) {
            Ok(file) => {
                return Ok((
                    file,
                    reserved_name(
                        candidate
                            .file_name()
                            .and_then(|name| name.to_str())
                            .ok_or_else(|| {
                                io::Error::new(
                                    io::ErrorKind::InvalidInput,
                                    "invalid Core state path",
                                )
                            })?,
                    ),
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => candidate = parent,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "Core state path has no physical parent",
    ))
}

#[derive(Debug)]
struct PublishedButUnconfirmed;
impl std::fmt::Display for PublishedButUnconfirmed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "destination published but durability/cleanup is unconfirmed; inspect before retrying",
        )
    }
}
impl std::error::Error for PublishedButUnconfirmed {}
#[cfg(unix)]
fn publication_uncertain() -> io::Error {
    io::Error::other(PublishedButUnconfirmed)
}
pub fn is_publication_uncertain(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<PublishedButUnconfirmed>())
}

pub fn open_protected(path: &std::path::Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        File::open(path)
    }
    #[cfg(windows)]
    {
        windows::open_protected(path)
    }
}

/// Portable wire paths deliberately exclude Windows aliases on Unix too.
/// This is validation, not a canonical filesystem name or Policy selector.
pub fn validate_relative_path(path: &str) -> io::Result<Vec<&str>> {
    let parts: Vec<_> = path.split('/').collect();
    if path.len() > 4096
        || parts.len() > 128
        || parts.iter().any(|part| {
            let base = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            part.is_empty()
                || base.ends_with(' ')
                || *part == "."
                || *part == ".."
                || part.len() > 255
                || part.ends_with(['.', ' '])
                || part
                    .chars()
                    .any(|c| c.is_control() || "\\:<>\"|?*".contains(c))
                || matches!(
                    base.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
                )
                || ["COM", "LPT"].iter().any(|prefix| {
                    base.strip_prefix(prefix).is_some_and(|suffix| {
                        matches!(
                            suffix,
                            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                        )
                    })
                })
                || part
                    .split_once('~')
                    .is_some_and(|(_, tail)| tail.starts_with(|c: char| c.is_ascii_digit()))
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid root-relative file path",
        ));
    }
    Ok(parts)
}

pub fn validate_mode(mode: u32) -> io::Result<()> {
    if mode & !0o777 != 0 || mode & 0o022 != 0 {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file mode must contain ordinary bits and must not permit group/other writes",
        ))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use rustix::fs::{self as sys, AtFlags, FileType, Mode, OFlags};
    use std::{io::Write, path::Path};

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Identity {
        device: u64,
        inode: u64,
    }
    fn identity(file: &File) -> io::Result<Identity> {
        Ok(stat_identity(&sys::fstat(file)?))
    }
    // dev_t/ino_t widths vary across supported Unix targets.
    #[allow(clippy::unnecessary_cast)]
    fn stat_identity(stat: &sys::Stat) -> Identity {
        Identity {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
        }
    }
    fn protected() -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Treer private state is not transferable",
        )
    }

    pub struct Root {
        file: File,
        device: u64,
        protected: Vec<Identity>,
        allow_mounts: bool,
        reserved_names: Vec<(Identity, String)>,
    }

    impl Root {
        /// Root must be the enrolled physical root, never a user-supplied prefix.
        /// Extra protected handles identify Core state even through hard-link aliases.
        pub fn open(root: &Path, protected_files: &[File]) -> io::Result<Self> {
            let file: File = sys::open(
                root,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into();
            let device = identity(&file)?.device;
            let mut protected = protected_files
                .iter()
                .map(identity)
                .collect::<io::Result<Vec<_>>>()?;
            match sys::openat(
                &file,
                ".treer",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => protected.push(identity(&File::from(fd))?),
                Err(error) if error == rustix::io::Errno::NOENT => {}
                Err(error) => return Err(error.into()),
            }
            Ok(Self {
                file,
                device,
                protected,
                allow_mounts: false,
                reserved_names: vec![],
            })
        }

        /// Local CLI paths have no enrolled root boundary. Retain NOFOLLOW
        /// traversal but allow ordinary mounted home/temp directories.
        pub fn open_local(root: &Path) -> io::Result<Self> {
            let mut result = Self::open(root, &[])?;
            result.allow_mounts = true;
            Ok(result)
        }
        pub fn available_bytes(&self) -> io::Result<u64> {
            let stat = sys::fstatvfs(&self.file)?;
            Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
        }
        pub fn reserve_path(&mut self, path: &Path) -> io::Result<()> {
            let (parent, name) = reserved_parent(path)?;
            self.reserved_names.push((identity(&parent)?, name));
            Ok(())
        }
        pub fn validate_target(&self, path: &str) -> io::Result<()> {
            let (parent, name) = self.parent(path)?;
            self.check_target(&parent, &name)
        }

        /// Controller-owned recovery metadata, opened with retained handles.
        pub fn transfer_journal(&self) -> io::Result<Self> {
            let mut directory = self.file.try_clone()?;
            for name in [".treer", "file-transfers"] {
                match sys::mkdirat(&directory, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                    Ok(()) => {}
                    Err(error) if error == rustix::io::Errno::EXIST => {}
                    Err(error) => return Err(error.into()),
                }
                directory = sys::openat(
                    &directory,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?
                .into();
                let stat = sys::fstat(&directory)?;
                if identity(&directory)?.device != self.device
                    || stat.st_uid != rustix::process::geteuid().as_raw()
                    || stat.st_mode & if name == ".treer" { 0o022 } else { 0o077 } != 0
                {
                    return Err(protected());
                }
            }
            Ok(Self {
                file: directory,
                device: self.device,
                protected: vec![],
                allow_mounts: false,
                reserved_names: vec![],
            })
        }

        pub fn exclusive_lease(&self) -> io::Result<File> {
            let file: File = sys::openat(
                &self.file,
                ".registry.lock",
                OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?
            .into();
            sys::flock(&file, sys::FlockOperation::NonBlockingLockExclusive)?;
            Ok(file)
        }

        /// Only a recorded random staging name can be removed by recovery.
        pub fn cleanup_staging(&self, path: &str, temporary_name: &str) -> io::Result<()> {
            if validate_relative_path(temporary_name)?.len() != 1
                || !(temporary_name.starts_with(".treer-transfer-xfer_")
                    || temporary_name.starts_with(".treer-transfer-upl_"))
            {
                return Err(protected());
            }
            let (parent, _) = self.parent(path)?;
            match sys::statat(&parent, temporary_name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => {
                    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
                        || self.protected.contains(&stat_identity(&stat))
                    {
                        return Err(protected());
                    }
                    sys::unlinkat(&parent, temporary_name, AtFlags::empty())?;
                    parent.sync_all()
                }
                Err(error) if error == rustix::io::Errno::NOENT => Ok(()),
                Err(error) => Err(error.into()),
            }
        }

        fn check(&self, file: &File) -> io::Result<()> {
            let id = identity(file)?;
            if !self.allow_mounts && id.device != self.device {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "file transfer cannot cross a filesystem mount",
                ));
            }
            if self.protected.contains(&id) {
                return Err(protected());
            }
            Ok(())
        }

        fn parent(&self, path: &str) -> io::Result<(File, String)> {
            let parts = validate_relative_path(path)?;
            // Reserve the namespace even before .treer exists. Physical identity
            // checks below also cover case-insensitive aliases of existing state.
            if is_reserved_namespace(path)? {
                return Err(protected());
            }
            let mut directory = self.file.try_clone()?;
            self.check(&directory)?;
            for (index, component) in parts.iter().enumerate() {
                if self
                    .reserved_names
                    .contains(&(identity(&directory)?, reserved_name(component)))
                {
                    return Err(protected());
                }
                if index == parts.len() - 1 {
                    break;
                }
                directory = sys::openat(
                    &directory,
                    *component,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?
                .into();
                self.check(&directory)?;
            }
            Ok((directory, parts.last().unwrap().to_string()))
        }

        pub fn read(&self, path: &str) -> io::Result<File> {
            let (parent, name) = self.parent(path)?;
            // NONBLOCK prevents a malicious FIFO/device from blocking before fstat.
            let file: File = sys::openat(
                &parent,
                name.as_str(),
                OFlags::RDONLY
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK
                    | OFlags::NOCTTY,
                Mode::empty(),
            )?
            .into();
            let stat = sys::fstat(&file)?;
            if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "transfer source must be an unlinked-alias-free regular file",
                ));
            }
            self.check(&file)?;
            Ok(file)
        }

        /// Caller supplies a random validated filename, not a path, for staging.
        pub fn stage(&self, path: &str, temporary_name: &str, mode: u32) -> io::Result<StagedFile> {
            validate_mode(mode)?;
            let names = validate_relative_path(temporary_name)?;
            if names.len() != 1 || !temporary_name.starts_with(".treer-transfer-") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid transfer staging name",
                ));
            }
            let (parent, name) = self.parent(path)?;
            self.check_target(&parent, &name)?;
            let file: File = sys::openat(
                &parent,
                temporary_name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?
            .into();
            // Staging stays private until digest verification and publication.
            let staged = StagedFile {
                parent,
                file: Some(file),
                name,
                temporary_name: temporary_name.to_owned(),
                installed: false,
                protected: self.protected.clone(),
                final_mode: mode,
                root: self.file.try_clone()?,
                root_device: self.device,
                allow_mounts: self.allow_mounts,
                path: path.to_owned(),
                reserved_names: self.reserved_names.clone(),
            };
            sys::fchmod(staged.file.as_ref().unwrap(), Mode::RUSR | Mode::WUSR)?;
            Ok(staged)
        }

        fn check_target(&self, parent: &File, name: &str) -> io::Result<()> {
            match sys::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => {
                    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "destination must be a regular file",
                        ));
                    }
                    if self.protected.contains(&stat_identity(&stat)) {
                        return Err(protected());
                    }
                }
                Err(error) if error == rustix::io::Errno::NOENT => {}
                Err(error) => return Err(error.into()),
            }
            Ok(())
        }
    }

    pub struct StagedFile {
        parent: File,
        file: Option<File>,
        name: String,
        temporary_name: String,
        installed: bool,
        protected: Vec<Identity>,
        final_mode: u32,
        root: File,
        root_device: u64,
        allow_mounts: bool,
        path: String,
        reserved_names: Vec<(Identity, String)>,
    }

    impl StagedFile {
        pub fn file_mut(&mut self) -> &mut File {
            self.file.as_mut().expect("staged file is not committed")
        }
        /// A read-only handle on the same staging inode, through the held
        /// parent descriptor, so the caller can verify the bytes it wrote
        /// before publishing (the write handle is write-only).
        pub fn reopen_read(&self) -> io::Result<File> {
            let written = self.file.as_ref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "staged file is committed")
            })?;
            let file: File = sys::openat(
                &self.parent,
                self.temporary_name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NOCTTY,
                Mode::empty(),
            )?
            .into();
            if identity(&file)? != identity(written)? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "staging name no longer refers to the staged file",
                ));
            }
            Ok(file)
        }
        pub fn commit(mut self, overwrite: bool) -> io::Result<()> {
            // A same-UID actor can still race this check using direct OS
            // operations; this boundary is not a sandbox against that actor.
            let root = Root {
                file: self.root.try_clone()?,
                device: self.root_device,
                protected: self.protected.clone(),
                allow_mounts: self.allow_mounts,
                reserved_names: self.reserved_names.clone(),
            };
            let (current, _) = root.parent(&self.path)?;
            if identity(&current)? != identity(&self.parent)? {
                return Err(protected());
            }
            // Never truncate or follow an existing target. Replacing its directory
            // entry does not modify a hard-linked inode or a symlink referent.
            match sys::statat(&self.parent, self.name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => {
                    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "destination must be a regular file",
                        ));
                    }
                    if self.protected.contains(&stat_identity(&stat)) {
                        return Err(protected());
                    }
                }
                Err(error) if error == rustix::io::Errno::NOENT => {}
                Err(error) => return Err(error.into()),
            }
            if let Some(mut file) = self.file.take() {
                file.flush()?;
                sys::fchmod(&file, Mode::from_bits_retain(self.final_mode as _))?;
                file.sync_all()?;
            }
            if overwrite {
                sys::renameat(
                    &self.parent,
                    self.temporary_name.as_str(),
                    &self.parent,
                    self.name.as_str(),
                )?;
            } else {
                // Atomic no-replace without an exists()+rename race. Linux
                // tries renameat2(RENAME_NOREPLACE) first (one name at every
                // instant); kernels or filesystems that reject the flag, and
                // other platforms, use linkat + unlinkat, where a failed unlink
                // after a successful link is a published file with its staging
                // name still present (publication_uncertain).
                #[cfg(target_os = "linux")]
                let renamed = match sys::renameat_with(
                    &self.parent,
                    self.temporary_name.as_str(),
                    &self.parent,
                    self.name.as_str(),
                    sys::RenameFlags::NOREPLACE,
                ) {
                    Ok(()) => true,
                    Err(error)
                        if matches!(
                            error,
                            rustix::io::Errno::NOSYS
                                | rustix::io::Errno::INVAL
                                | rustix::io::Errno::NOTSUP
                                | rustix::io::Errno::OPNOTSUPP
                        ) =>
                    {
                        false
                    }
                    Err(error) => return Err(error.into()),
                };
                #[cfg(not(target_os = "linux"))]
                let renamed = false;
                if !renamed {
                    sys::linkat(
                        &self.parent,
                        self.temporary_name.as_str(),
                        &self.parent,
                        self.name.as_str(),
                        AtFlags::empty(),
                    )?;
                    sys::unlinkat(&self.parent, self.temporary_name.as_str(), AtFlags::empty())
                        .map_err(|_| publication_uncertain())?;
                }
            }
            self.installed = true;
            self.parent
                .sync_all()
                .map_err(|_| publication_uncertain())?;
            Ok(())
        }
    }

    impl Drop for StagedFile {
        fn drop(&mut self) {
            self.file.take();
            if !self.installed {
                let _ = sys::unlinkat(&self.parent, self.temporary_name.as_str(), AtFlags::empty());
            }
        }
    }
}

#[cfg(unix)]
pub use unix::{Root, StagedFile};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Root, StagedFile};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_paths_reject_windows_and_unix_escape_aliases() {
        for path in [
            "",
            "/a",
            "../a",
            "a/../b",
            "a/./b",
            "a//b",
            "a/",
            "C:/a",
            "c:a",
            r"\\host\share",
            r"\\?\C:\a",
            "a:b",
            "NUL",
            "nul.txt",
            "Con",
            "com1.log",
            "LPT².txt",
            "CONOUT$",
            "con .txt",
            "COM¹ .txt",
            "a.",
            "a ",
            "a\0b",
            "a\nb",
            "SECRET~1/file",
            "x?y",
            "x*y",
        ] {
            assert!(validate_relative_path(path).is_err(), "{path:?}");
        }
        assert_eq!(
            validate_relative_path("research/report.pdf").unwrap(),
            ["research", "report.pdf"]
        );
        assert!(validate_mode(0o600).is_ok());
        assert!(validate_mode(0o4755).is_err());
    }

    #[test]
    fn reserved_namespace_classification_is_portable_and_syntax_checked() {
        for path in [
            ".treer/file",
            "a/.TrEeR/file",
            "missing/.TREER-TRANSFER-x/file",
        ] {
            assert!(is_reserved_namespace(path).unwrap());
        }
        for path in ["treer-rollouts/id/scripts/worker.mjs", ".treer-other/file"] {
            assert!(!is_reserved_namespace(path).unwrap());
        }
        for path in [
            "../.treer/file",
            ".treer\\file",
            ".treer/file:stream",
            "/.treer/file",
        ] {
            assert!(is_reserved_namespace(path).is_err());
        }
    }

    #[cfg(unix)]
    struct Fixture(std::path::PathBuf);
    #[cfg(unix)]
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "treer-transfer-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(std::fs::canonicalize(root).unwrap())
        }
        fn root(&self) -> Root {
            Root::open(&self.0, &[]).unwrap()
        }
    }
    #[cfg(unix)]
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn staging_is_private_atomic_and_abort_removes_bytes() {
        use std::{io::Write, os::unix::fs::PermissionsExt};
        let f = Fixture::new();
        let root = f.root();
        let mut staged = root
            .stage("report", ".treer-transfer-first", 0o600)
            .unwrap();
        staged.file_mut().write_all(b"first").unwrap();
        assert!(!f.0.join("report").exists());
        assert_eq!(
            staged.file_mut().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        staged.commit(false).unwrap();
        assert_eq!(std::fs::read(f.0.join("report")).unwrap(), b"first");
        let mut conflict = root
            .stage("report", ".treer-transfer-conflict", 0o600)
            .unwrap();
        conflict.file_mut().write_all(b"must not replace").unwrap();
        assert_eq!(
            conflict.commit(false).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(f.0.join("report")).unwrap(), b"first");
        assert!(!f.0.join(".treer-transfer-conflict").exists());
        let mut replacement = root
            .stage("report", ".treer-transfer-replace", 0o644)
            .unwrap();
        replacement.file_mut().write_all(b"new").unwrap();
        replacement.commit(true).unwrap();
        assert_eq!(std::fs::read(f.0.join("report")).unwrap(), b"new");
        drop(
            root.stage("aborted", ".treer-transfer-abort", 0o600)
                .unwrap(),
        );
        assert!(!f.0.join("aborted").exists());
        assert!(!f.0.join(".treer-transfer-abort").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_private_state_and_special_files_are_rejected() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        std::fs::create_dir(f.0.join(".treer")).unwrap();
        std::fs::write(f.0.join(".treer/private.json"), b"synthetic private state").unwrap();
        std::fs::write(f.0.join("safe"), b"safe").unwrap();
        symlink("safe", f.0.join("link")).unwrap();
        symlink(".treer", f.0.join("alias")).unwrap();
        let root = f.root();
        assert!(root.read("link").is_err());
        assert!(root.stage("link", ".treer-transfer-link", 0o600).is_err());
        assert!(root.read("alias/private.json").is_err());
        assert!(root.read(".TREER/private.json").is_err());
        assert!(root
            .stage(".treer/new", ".treer-transfer-private", 0o600)
            .is_err());
        assert!(root.read(".treer/private.json").is_err());
        assert!(root.read(".").is_err());
        std::fs::hard_link(f.0.join("safe"), f.0.join("hardlink")).unwrap();
        assert!(root.read("hardlink").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn held_parent_prevents_symlink_swap_from_redirecting_commit() {
        use std::{io::Write, os::unix::fs::symlink};
        let f = Fixture::new();
        let outside = Fixture::new();
        std::fs::create_dir(f.0.join("directory")).unwrap();
        let root = f.root();
        let mut staged = root
            .stage("directory/result", ".treer-transfer-swap", 0o600)
            .unwrap();
        staged.file_mut().write_all(b"safe").unwrap();
        std::fs::rename(f.0.join("directory"), f.0.join("held")).unwrap();
        symlink(&outside.0, f.0.join("directory")).unwrap();
        assert!(staged.commit(false).is_err());
        assert!(!f.0.join("held/result").exists());
        assert!(!outside.0.join("result").exists());
        assert!(root.read("directory/result").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn requested_public_mode_remains_private_until_verified_publication() {
        use std::{io::Write, os::unix::fs::PermissionsExt};
        let f = Fixture::new();
        let root = f.root();
        let mut staged = root
            .stage("result", ".treer-transfer-public", 0o644)
            .unwrap();
        staged.file_mut().write_all(b"uncommitted").unwrap();
        assert_eq!(
            staged.file_mut().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        staged.commit(false).unwrap();
        assert_eq!(
            std::fs::metadata(f.0.join("result"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        assert!(validate_mode(0o666).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn absent_reserved_core_names_are_bound_to_parent_identity_and_unicode_aliases() {
        let f = Fixture::new();
        let mut root = f.root();
        root.reserve_path(&f.0.join("sécrets.json")).unwrap();
        for name in ["sécrets.json", "SE\u{301}CRETS.JSON"] {
            assert_eq!(
                root.stage(name, ".treer-transfer-reserved", 0o600)
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        assert_eq!(std::fs::read_dir(&f.0).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_no_replace_has_exactly_one_winner() {
        use std::{
            io::Write,
            sync::{Arc, Barrier},
        };
        let f = Fixture::new();
        let root = Arc::new(f.root());
        let barrier = Arc::new(Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut staged = root
                        .stage("winner", &format!(".treer-transfer-{index}"), 0o600)
                        .unwrap();
                    staged.file_mut().write_all(&[index]).unwrap();
                    barrier.wait();
                    staged.commit(false).is_ok()
                })
            })
            .collect();
        let winners = threads
            .into_iter()
            .map(|thread| usize::from(thread.join().unwrap()))
            .sum::<usize>();
        assert_eq!(winners, 1);
        assert_eq!(std::fs::read_dir(&f.0).unwrap().count(), 1);
    }
}
