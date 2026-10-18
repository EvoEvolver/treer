//! Retained non-delete-sharing directory handles prevent path replacement while
//! the Win32 path-based atomic rename is in flight. Reparse points are rejected.
use super::*;
use std::{
    fs::OpenOptions,
    io::Write,
    os::windows::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf, Prefix},
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity(u64, u64);
fn metadata(file: &File) -> io::Result<(Identity, u32, u32)> {
    let (volume, index, links, attributes) = crate::windows::transfer_file_identity(file)?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "reparse points are not transferable",
        ));
    }
    Ok((Identity(volume, index), links, attributes))
}
fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}
pub(super) fn open_protected(path: &Path) -> io::Result<File> {
    let file = open(path)?;
    metadata(&file)?;
    Ok(file)
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "protected file transfer object",
    )
}

pub struct Root {
    path: PathBuf,
    guards: Vec<File>,
    protected: Vec<Identity>,
    volume: u64,
    allow_mounts: bool,
    reserved_names: Vec<(Identity, String)>,
}
impl Root {
    pub fn open(path: &Path, protected_files: &[File]) -> io::Result<Self> {
        let mut parts = path.components();
        let Some(Component::Prefix(prefix)) = parts.next() else {
            return Err(denied());
        };
        if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            || parts.next() != Some(Component::RootDir)
        {
            return Err(denied());
        }
        let mut current = PathBuf::from(prefix.as_os_str());
        current.push("\\");
        let mut guards = Vec::new();
        let root = open(&current)?;
        let (id, _, attrs) = metadata(&root)?;
        if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(denied());
        }
        let volume = id.0;
        guards.push(root);
        for part in parts {
            let Component::Normal(part) = part else {
                return Err(denied());
            };
            current.push(part);
            let directory = open(&current)?;
            let (id, _, attrs) = metadata(&directory)?;
            if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 || id.0 != volume {
                return Err(denied());
            }
            guards.push(directory);
        }
        let mut protected = protected_files
            .iter()
            .map(|file| metadata(file).map(|value| value.0))
            .collect::<io::Result<Vec<_>>>()?;
        match open(&current.join(".treer")) {
            Ok(file) => protected.push(metadata(&file)?.0),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(Self {
            path: current,
            guards,
            protected,
            volume,
            allow_mounts: false,
            reserved_names: vec![],
        })
    }
    pub fn open_local(root: &Path) -> io::Result<Self> {
        let mut result = Self::open(root, &[])?;
        result.allow_mounts = true;
        Ok(result)
    }
    pub fn available_bytes(&self) -> io::Result<u64> {
        crate::windows::transfer_available_bytes(&self.path)
    }
    pub fn reserve_path(&mut self, path: &Path) -> io::Result<()> {
        let (parent, name) = reserved_parent(path)?;
        self.reserved_names.push((metadata(&parent)?.0, name));
        Ok(())
    }
    pub fn validate_target(&self, path: &str) -> io::Result<()> {
        let (path, _guards) = self.parent(path)?;
        self.check_target(&path)
    }
    pub fn transfer_journal(&self) -> io::Result<Self> {
        let mut path = self.path.clone();
        let mut guards = self
            .guards
            .iter()
            .map(File::try_clone)
            .collect::<io::Result<Vec<_>>>()?;
        for part in [".treer", "file-transfers"] {
            path.push(part);
            match std::fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let directory = open(&path)?;
            let (_, _, attrs) = metadata(&directory)?;
            if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
                return Err(denied());
            }
            crate::windows::secure_path(&path)?;
            guards.push(directory);
        }
        Ok(Self {
            path,
            guards,
            protected: vec![],
            volume: self.volume,
            allow_mounts: false,
            reserved_names: vec![],
        })
    }
    pub fn exclusive_lease(&self) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.path.join(".registry.lock"))?;
        metadata(&file)?;
        Ok(file)
    }
    pub fn cleanup_staging(&self, path: &str, temporary_name: &str) -> io::Result<()> {
        if validate_relative_path(temporary_name)?.len() != 1
            || !(temporary_name.starts_with(".treer-transfer-xfer_")
                || temporary_name.starts_with(".treer-transfer-upl_"))
        {
            return Err(denied());
        }
        let (target, _guards) = self.parent(path)?;
        let temporary = target.parent().ok_or_else(denied)?.join(temporary_name);
        match open(&temporary) {
            Ok(file) => self.check(&file, false)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        std::fs::remove_file(temporary)
    }
    fn check(&self, file: &File, directory: bool) -> io::Result<()> {
        let (id, links, attrs) = metadata(file)?;
        if self.protected.contains(&id)
            || (!self.allow_mounts && id.0 != self.volume)
            || ((attrs & FILE_ATTRIBUTE_DIRECTORY != 0) != directory)
            || (!directory && links != 1)
        {
            return Err(denied());
        }
        Ok(())
    }
    fn parent(&self, path: &str) -> io::Result<(PathBuf, Vec<File>)> {
        let parts = validate_relative_path(path)?;
        if is_reserved_namespace(path)? {
            return Err(denied());
        }
        let mut guards = self
            .guards
            .iter()
            .map(File::try_clone)
            .collect::<io::Result<Vec<_>>>()?;
        let mut parent = self.path.clone();
        self.check(guards.last().ok_or_else(denied)?, true)?;
        for (index, part) in parts.iter().enumerate() {
            if self.reserved_names.contains(&(
                metadata(guards.last().ok_or_else(denied)?)?.0,
                reserved_name(part),
            )) {
                return Err(denied());
            }
            if index == parts.len() - 1 {
                break;
            }
            parent.push(part);
            let file = open(&parent)?;
            self.check(&file, true)?;
            guards.push(file);
        }
        parent.push(parts.last().unwrap());
        Ok((parent, guards))
    }
    fn check_target(&self, path: &Path) -> io::Result<()> {
        match open(path) {
            Ok(file) => self.check(&file, false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    pub fn read(&self, path: &str) -> io::Result<File> {
        let (path, _guards) = self.parent(path)?;
        let file = open(&path)?;
        self.check(&file, false)?;
        Ok(file)
    }
    pub fn stage(&self, path: &str, temporary_name: &str, mode: u32) -> io::Result<StagedFile> {
        validate_mode(mode)?;
        if mode != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows transfers support only private mode 600",
            ));
        }
        if validate_relative_path(temporary_name)?.len() != 1
            || !temporary_name.starts_with(".treer-transfer-")
        {
            return Err(denied());
        }
        let (target, guards) = self.parent(path)?;
        self.check_target(&target)?;
        let temporary = target.parent().ok_or_else(denied)?.join(temporary_name);
        let file = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&temporary)?;
        let staged = StagedFile {
            file: Some(file),
            target,
            temporary,
            guards,
            protected: self.protected.clone(),
            installed: false,
        };
        // No content is written until the current-user/SYSTEM-only DACL exists.
        crate::windows::secure_path(&staged.temporary)?;
        Ok(staged)
    }
}

pub struct StagedFile {
    file: Option<File>,
    target: PathBuf,
    temporary: PathBuf,
    guards: Vec<File>,
    protected: Vec<Identity>,
    installed: bool,
}
impl StagedFile {
    pub fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().unwrap()
    }
    /// A read-only handle on the same staging file (volume and file index
    /// compared with the write handle), so the caller can verify the bytes it
    /// wrote before publishing. The write handle shares read access.
    pub fn reopen_read(&self) -> io::Result<File> {
        let written = self.file.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "staged file is committed")
        })?;
        let reader = open(&self.temporary)?;
        let (id, _, _) = metadata(&reader)?;
        let (own, _, _) = metadata(written)?;
        if id != own {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "staging name no longer refers to the staged file",
            ));
        }
        Ok(reader)
    }
    pub fn commit(mut self, overwrite: bool) -> io::Result<()> {
        let mut file = self.file.take().unwrap();
        file.flush()?;
        file.sync_all()?;
        drop(file);
        match open(&self.target) {
            Ok(file) => {
                let (id, links, attrs) = metadata(&file)?;
                if self.protected.contains(&id)
                    || links != 1
                    || attrs & FILE_ATTRIBUTE_DIRECTORY != 0
                {
                    return Err(denied());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        crate::windows::publish_transfer_file(&self.temporary, &self.target, overwrite)?;
        self.installed = true;
        Ok(())
    }
}
impl Drop for StagedFile {
    fn drop(&mut self) {
        self.file.take();
        if !self.installed {
            let _ = std::fs::remove_file(&self.temporary);
        }
        // Guards outlive removal/publication, including during error unwinding.
        self.guards.clear();
    }
}
