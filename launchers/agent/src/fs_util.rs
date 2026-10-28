//! The few file operations whose flags differ by platform. Private files and
//! directories are created current-user-only: Unix mode 0600/0700, Windows the
//! protected current-user DACL from treer-platform. Same-user processes are
//! trusted, so existing Windows paths are not re-verified; on Unix the cheap
//! owner/mode/link checks keep other OS users' files out (shared `/tmp`).
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
};

/// Create a new private file exclusively (never follows a final symlink on Unix).
pub fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    treer_platform::secure_file(path)?;
    Ok(file)
}

/// Open (creating if needed) a private file for read/write without truncating.
pub fn open_or_create(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    treer_platform::secure_file(path)?;
    Ok(file)
}

/// Create a private directory. `Ok(false)`: it already existed (left as is).
pub fn create_dir(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path)
    };
    #[cfg(windows)]
    let created = fs::create_dir(path);
    match created {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    }
    #[cfg(windows)]
    treer_platform::secure_directory(path)?;
    Ok(true)
}

/// Open for reading. Unix: never follows a final symlink or blocks on a FIFO.
pub fn open_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

/// A regular file. Unix additionally: owned by this user, single link, no
/// group/other permission bits.
pub fn is_private_file(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.mode() & 0o077 == 0
    }
    #[cfg(windows)]
    metadata.is_file()
}

/// Atomically replace `target` with `source` (Windows: write-through).
pub fn replace(source: &Path, target: &Path) -> io::Result<()> {
    treer_platform::replace_file(source, target)
}

/// Make directory entry changes durable. Windows has no directory fsync; the
/// write-through replace above already flushed the rename.
pub fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return File::open(path)?.sync_all();
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
}
