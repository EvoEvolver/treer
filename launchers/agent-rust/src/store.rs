//! Experiment-owned state only: never open or migrate a Node launcher's files.
use anyhow::{bail, Result};
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub struct Store {
    pub root: PathBuf,
    cwd: PathBuf,
}
impl Store {
    pub fn open(root: &Path, cwd: &Path) -> Result<Self> {
        if !root.is_absolute() || !root.exists() {
            bail!("private root must already exist");
        }
        // Inspect the lexical chain before canonicalization (do not erase links).
        for ancestor in root.ancestors() {
            if fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
                bail!("linked private ancestor")
            }
        }
        let root = fs::canonicalize(root)?;
        let cwd = fs::canonicalize(cwd)?;
        if root.starts_with(&cwd) {
            bail!("spike private root must be outside cwd");
        }
        for ancestor in root.ancestors() {
            let metadata = fs::symlink_metadata(ancestor)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                bail!("unsafe private ancestor");
            }
            #[cfg(unix)]
            if (metadata.uid() != rustix::process::geteuid().as_raw() && metadata.uid() != 0)
                || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
            {
                bail!("private ancestor owner/mode");
            }
            for marker in [".git", ".hg", ".jj", ".svn", ".bzr", ".sl"] {
                match fs::symlink_metadata(ancestor.join(marker)) {
                    Ok(_) => bail!("private state in VCS"),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        #[cfg(unix)]
        {
            let m = fs::metadata(&root)?;
            if m.uid() != rustix::process::geteuid().as_raw() || m.mode() & 0o077 != 0 {
                bail!("private root owner/mode");
            }
        }
        Ok(Self { root, cwd })
    }
    pub fn load(&self) -> Result<Option<Value>> {
        let path = self.root.join("rust-spike.json");
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = match options.open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if !file.metadata()?.is_file() || file.metadata()?.len() > 32 * 1024 * 1024 {
            bail!("invalid private state file");
        }
        #[cfg(unix)]
        {
            let m = file.metadata()?;
            if m.uid() != fs::metadata(&self.root)?.uid() || m.mode() & 0o077 != 0 || m.nlink() != 1
            {
                bail!("private state file owner/mode/links");
            }
        }
        let mut data = Vec::new();
        Read::by_ref(&mut file)
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut data)?;
        if data.len() > 32 * 1024 * 1024 {
            bail!("state too large");
        }
        Ok(Some(serde_json::from_slice(&data)?))
    }
    pub fn recheck(&self) -> Result<()> {
        Self::open(&self.root, &self.cwd)?;
        Ok(())
    }
    pub fn save(&self, state: &Value) -> Result<()> {
        // Re-evaluate placement on each journal update; git init disables writes.
        Self::open(&self.root, &self.cwd)?;
        let data = serde_json::to_vec(state)?;
        if data.len() > 32 * 1024 * 1024 {
            bail!("state too large");
        }
        let temp = self
            .root
            .join(format!(".rust-spike-{}.tmp", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let mut f = options.open(&temp)?;
        f.write_all(&data)?;
        f.sync_all()?;
        fs::rename(&temp, self.root.join("rust-spike.json"))?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}
