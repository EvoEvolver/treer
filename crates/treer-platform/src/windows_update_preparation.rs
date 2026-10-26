//! Bounded existing configuration snapshots and exclusive private staging.
//! The Controller selects enrollment, authenticates bytes and grants no mutation
//! authority from these mechanical handles. Partial staging is never retried.
use crate::{
    machine_file::Root, windows_update_inputs::PinnedPair, windows_update_store::UpdateStore,
};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

fn refused() -> io::Error {
    io::Error::other("Windows update preparation refused")
}

pub struct ConfigDirectory {
    root: Root,
    path: PathBuf,
}
pub struct ConfigSnapshot {
    _file: File,
    bytes: Vec<u8>,
}
impl ConfigSnapshot {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl ConfigDirectory {
    pub fn open(path: &Path) -> io::Result<Self> {
        let root = Root::open(path, &[])?;
        if !crate::private_path_is_secure(path)? {
            return Err(refused());
        }
        crate::windows::require_update_ntfs(path)?;
        Ok(Self {
            root,
            path: path.into(),
        })
    }
    pub fn controller_names(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for (index, entry) in std::fs::read_dir(&self.path)?.enumerate() {
            if index >= 1024 {
                return Err(refused());
            }
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| refused())?;
            if name.ends_with("-controller.json") {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }
    pub fn read(&self, name: &str) -> io::Result<ConfigSnapshot> {
        if name.is_empty()
            || name.len() > 200
            || name
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && !b"_-.".contains(&b))
            || !name.ends_with(".json")
        {
            return Err(refused());
        }
        let path = self.path.join(name);
        let mut file = crate::windows_update_store::open_record(&path, false)?;
        let (volume, _, _, _) = crate::transfer_file_identity(&file)?;
        if volume != self.root.identity()?.0 {
            return Err(refused());
        }
        let mut bytes = Vec::new();
        (&mut file).take(256 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.is_empty()
            || bytes.len() > 256 * 1024
            || file.metadata()?.len() != bytes.len() as u64
        {
            return Err(refused());
        }
        Ok(ConfigSnapshot { _file: file, bytes })
    }
}

/// Create only under the runtime-owned, already pinned state directory. Every
/// name is exclusive; failure preserves any created evidence. No cleanup API.
pub fn stage_pair(
    parent: &crate::windows_update_backup::BackupDirectory,
    operation: &str,
    controller: &[u8],
    cli: &[u8],
) -> io::Result<(PinnedPair, UpdateStore)> {
    if operation.len() != 32
        || !operation
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || [controller, cli]
            .iter()
            .any(|b| b.is_empty() || b.len() > 128 * 1024 * 1024)
    {
        return Err(refused());
    }
    let path = parent
        .directory_path()
        .join(format!("controller-update-{operation}"));
    std::fs::create_dir(&path)?;
    crate::secure_directory(&path)?;
    let root = Root::open(&path, &[])?;
    if root.identity()?.0 != parent.identity()?.0 {
        return Err(refused());
    }
    let write = |name: &str, bytes: &[u8]| -> io::Result<()> {
        let file_path = path.join(name);
        let mut file = crate::windows::create_private_update_file(&file_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        crate::windows_update_store::checked_file(&file_path, &file)?;
        Ok(())
    };
    write("treer-agent-server.exe", controller)?;
    write("treer.exe", cli)?;
    let job = path.join("job");
    std::fs::create_dir(&job)?;
    crate::secure_directory(&job)?;
    let store = UpdateStore::open(&job)?;
    // This is new, not-yet-admitted staging. Reopen strict read-only pins and
    // let Controller admission authenticate ALL held bytes after this boundary.
    // No authenticated sender custody is released or re-created here.
    let pair = PinnedPair::open(&path)?;
    if pair.identity()? != root.identity()? {
        return Err(refused());
    }
    Ok((pair, store))
}
