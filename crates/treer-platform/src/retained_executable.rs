//! Retained identity for trusted OS dependencies (for example a Node interpreter).
//! No byte authentication, PATH lookup, canonicalization, or process execution.
//! Same-account writers remain trusted; path verification and exec are not atomic.
//! Trust checks inspect Unix uid/mode bits, not ACL-granted access or file flags.
use std::{
    ffi::OsString,
    fs::File,
    io,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use rustix::fs::{self as sys, AtFlags, FileType, Mode, OFlags};

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "unsafe retained executable",
    )
}

fn trusted_owner(uid: u32) -> bool {
    uid == 0 || uid == rustix::process::geteuid().as_raw()
}

fn check(file: &File, directory: bool) -> io::Result<()> {
    let info = file.metadata()?;
    if !trusted_owner(info.uid())
        || info.mode() & 0o022 != 0
        || if directory {
            !info.is_dir()
        } else {
            !info.is_file() || info.mode() & 0o7000 != 0 || info.mode() & 0o111 == 0
        }
    {
        return Err(denied());
    }
    Ok(())
}

fn walk(root: &File, components: &[OsString]) -> io::Result<(Vec<File>, File)> {
    check(root, true)?;
    let (name, ancestors) = components.split_last().ok_or_else(denied)?;
    let mut directories = Vec::with_capacity(ancestors.len());
    for component in ancestors {
        let parent = directories.last().unwrap_or(root);
        let directory = File::from(sys::openat(
            parent,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        check(&directory, true)?;
        directories.push(directory);
    }
    let parent = directories.last().unwrap_or(root);
    // Refuse known special files before opening. NONBLOCK also prevents a FIFO
    // substitution from blocking the subsequent open; fstat checks again.
    let stat = sys::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(denied());
    }
    let file = File::from(sys::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    check(&file, false)?;
    Ok((directories, file))
}

fn same(left: &File, right: &File) -> io::Result<bool> {
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok((left.dev(), left.ino()) == (right.dev(), right.ino()))
}

/// Retains every ancestor and the exact executable inode. Not Debug/Serialize.
/// No digest/signature or immutability claim: a trusted owner can edit bytes in
/// place. Consumers still need their own clean argv/environment, lifecycle and
/// release-image authentication, and must verify before spawn/after quiescence.
pub struct RetainedExecutable {
    root: File,
    directories: Vec<File>,
    file: File,
    components: Vec<OsString>,
    path: PathBuf,
}

impl RetainedExecutable {
    /// Input must already be a physical absolute path. Resolve trusted OS
    /// symlinks before this call, never silently adopt a newly resolved target
    /// when revalidating. Every ancestor (including /) must be root/current-user
    /// owned and not group/other writable. No directory/file is created/chmodded.
    pub fn open(path: &Path) -> io::Result<Self> {
        let result = (|| {
            if !path.is_absolute() {
                return Err(denied());
            }
            let components = path
                .components()
                .filter(|c| !matches!(c, Component::RootDir))
                .map(|c| match c {
                    Component::Normal(name) => Ok(name.to_owned()),
                    _ => Err(denied()),
                })
                .collect::<io::Result<Vec<_>>>()?;
            let root = File::from(sys::open(
                "/",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )?);
            Self::retain(root, components, path.to_owned())
        })();
        result.map_err(|_| denied())
    }

    fn retain(root: File, components: Vec<OsString>, path: PathBuf) -> io::Result<Self> {
        let (directories, file) = walk(&root, &components)?;
        let result = Self {
            root,
            directories,
            file,
            components,
            path,
        };
        result.verify()?;
        Ok(result)
    }

    pub fn executable(&self) -> &Path {
        &self.path
    }

    /// Fresh handle-relative traversal must match ALL retained directory/file
    /// identities, with fresh ownership, mode, type and executable checks.
    /// A byte-identical new inode is rejected rather than re-authenticated.
    pub fn verify(&self) -> io::Result<()> {
        let result = (|| {
            let (directories, file) = walk(&self.root, &self.components)?;
            if directories.len() != self.directories.len() || !same(&self.file, &file)? {
                return Err(denied());
            }
            for (held, current) in self.directories.iter().zip(&directories) {
                if !same(held, current)? {
                    return Err(denied());
                }
            }
            Ok(())
        })();
        result.map_err(|_| denied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "retained-executable-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            fs::create_dir(root.join("bin")).unwrap();
            fs::set_permissions(root.join("bin"), fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(
                root.join("bin/node"),
                b"fixture interpreter bytes; never executed",
            )
            .unwrap();
            fs::set_permissions(root.join("bin/node"), fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
        fn retain(&self) -> io::Result<RetainedExecutable> {
            // Same traversal/revalidation below a private fixture root, so tests
            // do not need writes to trusted system paths or a real interpreter.
            // Production open() always anchors at / and checks the full chain.
            RetainedExecutable::retain(
                File::open(&self.0)?,
                vec!["bin".into(), "node".into()],
                self.0.join("bin/node"),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn retains_handles_and_refuses_identical_bytes_at_a_new_inode() {
        let f = Fixture::new();
        let held = f.retain().unwrap();
        held.verify().unwrap();
        assert_eq!(held.executable(), f.0.join("bin/node"));
        let flags = rustix::io::fcntl_getfd(&held.file).unwrap();
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        fs::copy(f.0.join("bin/node"), f.0.join("bin/replacement")).unwrap();
        fs::rename(f.0.join("bin/replacement"), f.0.join("bin/node")).unwrap();
        assert_eq!(
            held.verify().unwrap_err().to_string(),
            "unsafe retained executable"
        );
        f.retain().unwrap().verify().unwrap(); // A fresh proof would accept it.
    }

    #[test]
    fn ancestor_replacement_fails_even_when_executable_inode_is_unchanged() {
        let f = Fixture::new();
        let held = f.retain().unwrap();
        fs::rename(f.0.join("bin"), f.0.join("old-bin")).unwrap();
        fs::create_dir(f.0.join("bin")).unwrap();
        fs::hard_link(f.0.join("old-bin/node"), f.0.join("bin/node")).unwrap();
        assert!(same(&held.file, &File::open(f.0.join("bin/node")).unwrap()).unwrap());
        assert!(held.verify().is_err());
    }

    #[test]
    fn modes_are_rechecked_on_root_ancestor_and_file() {
        for (part, mode) in [
            ("", 0o777),
            ("bin", 0o770),
            ("bin/node", 0o702),
            ("bin/node", 0o600),
            ("bin/node", 0o4700),
        ] {
            let f = Fixture::new();
            let held = f.retain().unwrap();
            fs::set_permissions(f.0.join(part), fs::Permissions::from_mode(mode)).unwrap();
            assert!(held.verify().is_err(), "changed {part}");
            assert!(f.retain().is_err(), "initial check of {part}");
        }
    }

    #[test]
    fn links_and_special_files_are_rejected_without_executable_calls() {
        for kind in [
            "symlink",
            "directory",
            "socket",
            "ancestor-symlink",
            #[cfg(target_os = "linux")]
            "fifo",
        ] {
            let f = Fixture::new();
            let held = f.retain().unwrap();
            if kind == "ancestor-symlink" {
                fs::rename(f.0.join("bin"), f.0.join("actual-bin")).unwrap();
                symlink("actual-bin", f.0.join("bin")).unwrap();
            } else {
                fs::rename(f.0.join("bin/node"), f.0.join("bin/actual-node")).unwrap();
                match kind {
                    "symlink" => symlink("actual-node", f.0.join("bin/node")).unwrap(),
                    "directory" => fs::create_dir(f.0.join("bin/node")).unwrap(),
                    "socket" => {
                        std::os::unix::net::UnixListener::bind(f.0.join("bin/node")).unwrap();
                    }
                    #[cfg(target_os = "linux")]
                    "fifo" => sys::mkfifoat(
                        rustix::fs::CWD,
                        f.0.join("bin/node"),
                        Mode::RUSR | Mode::WUSR,
                    )
                    .unwrap(),
                    _ => unreachable!(),
                }
            }
            assert!(held.verify().is_err());
            assert!(f.retain().is_err());
        }
    }

    #[test]
    fn production_entry_rejects_nonphysical_paths_and_owners_are_explicit() {
        for path in ["relative", "/", "/tmp/../bin/node"] {
            assert_eq!(
                RetainedExecutable::open(Path::new(path))
                    .err()
                    .unwrap()
                    .to_string(),
                "unsafe retained executable"
            );
        }
        assert!(trusted_owner(0));
        let uid = rustix::process::geteuid().as_raw();
        assert!(trusted_owner(uid));
        assert!(!trusted_owner(if uid == 1 { 2 } else { 1 }));
    }
}
