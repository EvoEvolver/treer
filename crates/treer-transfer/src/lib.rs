use std::path::{Component, Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use treer_protocol::{
    ProtocolError, TransferBinaryFrame, TransferBinaryKind, TransferEntry, TransferEntryKind,
    TransferStats,
};
use uuid::Uuid;

const CHUNK_SIZE: usize = 64 * 1024;

struct SourceEntry {
    path: PathBuf,
    metadata: TransferEntry,
}

pub async fn stream_path(
    source: PathBuf,
    recursive: bool,
    session_id: String,
    sender: mpsc::Sender<TransferBinaryFrame>,
) -> Result<TransferStats, ProtocolError> {
    let manifest = build_manifest(&source, recursive).await?;
    let mut stats = TransferStats::default();
    for entry in manifest {
        send_frame(
            &sender,
            TransferBinaryFrame {
                kind: TransferBinaryKind::Entry,
                session_id: session_id.clone(),
                payload: serde_json::to_vec(&entry.metadata)
                    .map_err(|error| transfer_error("metadata_encode_failed", error))?,
            },
        )
        .await?;
        if entry.metadata.kind == TransferEntryKind::File {
            let mut file = tokio::fs::File::open(&entry.path)
                .await
                .map_err(|error| path_error("source_open_failed", &entry.path, error))?;
            let mut buffer = vec![0_u8; CHUNK_SIZE];
            loop {
                let read = file
                    .read(&mut buffer)
                    .await
                    .map_err(|error| path_error("source_read_failed", &entry.path, error))?;
                if read == 0 {
                    break;
                }
                stats.bytes = stats.bytes.saturating_add(read as u64);
                send_frame(
                    &sender,
                    TransferBinaryFrame {
                        kind: TransferBinaryKind::Data,
                        session_id: session_id.clone(),
                        payload: buffer[..read].to_vec(),
                    },
                )
                .await?;
            }
        }
        stats.entries = stats.entries.saturating_add(1);
        send_frame(
            &sender,
            TransferBinaryFrame {
                kind: TransferBinaryKind::EntryEnd,
                session_id: session_id.clone(),
                payload: Vec::new(),
            },
        )
        .await?;
    }
    send_frame(
        &sender,
        TransferBinaryFrame {
            kind: TransferBinaryKind::TransferEnd,
            session_id,
            payload: serde_json::to_vec(&stats)
                .map_err(|error| transfer_error("metadata_encode_failed", error))?,
        },
    )
    .await?;
    Ok(stats)
}

async fn send_frame(
    sender: &mpsc::Sender<TransferBinaryFrame>,
    frame: TransferBinaryFrame,
) -> Result<(), ProtocolError> {
    sender
        .send(frame)
        .await
        .map_err(|_| ProtocolError::new("transfer_cancelled", "transfer receiver disconnected"))
}

async fn build_manifest(source: &Path, recursive: bool) -> Result<Vec<SourceEntry>, ProtocolError> {
    let original_metadata = tokio::fs::symlink_metadata(source)
        .await
        .map_err(|error| path_error("source_not_found", source, error))?;
    if original_metadata.file_type().is_symlink() {
        return Err(ProtocolError::new(
            "unsupported_file_type",
            format!("symbolic links are not supported: {}", source.display()),
        ));
    }
    let source = tokio::fs::canonicalize(source)
        .await
        .map_err(|error| path_error("source_not_found", source, error))?;
    let root_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ProtocolError::new("invalid_source", "source must have a UTF-8 file name"))?
        .to_string();
    let root_metadata = tokio::fs::symlink_metadata(&source)
        .await
        .map_err(|error| path_error("source_metadata_failed", &source, error))?;
    if root_metadata.is_dir() && !recursive {
        return Err(ProtocolError::new(
            "recursive_required",
            "source is a directory; use --recursive",
        ));
    }

    let mut pending = vec![(source, PathBuf::from(root_name))];
    let mut manifest = Vec::new();
    while let Some((path, relative)) = pending.pop() {
        let metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|error| path_error("source_metadata_failed", &path, error))?;
        let kind = if metadata.file_type().is_symlink() {
            return Err(ProtocolError::new(
                "unsupported_file_type",
                format!("symbolic links are not supported: {}", path.display()),
            ));
        } else if metadata.is_file() {
            TransferEntryKind::File
        } else if metadata.is_dir() {
            TransferEntryKind::Directory
        } else {
            return Err(ProtocolError::new(
                "unsupported_file_type",
                format!(
                    "only regular files and directories can be copied: {}",
                    path.display()
                ),
            ));
        };
        let wire_path = relative.to_str().ok_or_else(|| {
            ProtocolError::new(
                "invalid_source",
                format!("source path is not UTF-8: {}", relative.display()),
            )
        })?;
        manifest.push(SourceEntry {
            path: path.clone(),
            metadata: TransferEntry {
                path: wire_path.to_string(),
                kind,
                size: if kind == TransferEntryKind::File {
                    metadata.len()
                } else {
                    0
                },
                mode: file_mode(&metadata),
            },
        });
        if kind == TransferEntryKind::Directory {
            let mut reader = tokio::fs::read_dir(&path)
                .await
                .map_err(|error| path_error("source_read_failed", &path, error))?;
            let mut children = Vec::new();
            while let Some(child) = reader
                .next_entry()
                .await
                .map_err(|error| path_error("source_read_failed", &path, error))?
            {
                children.push((child.path(), relative.join(child.file_name())));
            }
            children.sort_by(|left, right| left.1.cmp(&right.1));
            pending.extend(children.into_iter().rev());
        }
    }
    Ok(manifest)
}

enum CurrentEntry {
    Directory,
    File {
        file: tokio::fs::File,
        temporary: PathBuf,
        destination: PathBuf,
        expected: u64,
        written: u64,
        mode: Option<u32>,
    },
}

struct DestinationMapping {
    source_root: String,
    destination_root: PathBuf,
}

pub struct TransferReceiver {
    destination: PathBuf,
    destination_requires_directory: bool,
    confinement: Option<PathBuf>,
    recursive: bool,
    session_id: String,
    mapping: Option<DestinationMapping>,
    current: Option<CurrentEntry>,
    directory_modes: Vec<(PathBuf, Option<u32>)>,
    stats: TransferStats,
}

impl Drop for TransferReceiver {
    fn drop(&mut self) {
        if let Some(CurrentEntry::File { temporary, .. }) = &self.current {
            let _ = std::fs::remove_file(temporary);
        }
    }
}

impl TransferReceiver {
    pub async fn new(
        destination: PathBuf,
        confinement: Option<PathBuf>,
        recursive: bool,
        session_id: String,
    ) -> Result<Self, ProtocolError> {
        let destination_requires_directory = destination
            .to_string_lossy()
            .ends_with(std::path::MAIN_SEPARATOR);
        let confinement = match confinement {
            Some(root) => Some(
                tokio::fs::canonicalize(&root)
                    .await
                    .map_err(|error| path_error("invalid_workspace_root", &root, error))?,
            ),
            None => None,
        };
        let destination = if let Some(root) = &confinement {
            validate_requested_path(&destination)?;
            root.join(destination)
        } else {
            destination
        };
        Ok(Self {
            destination,
            destination_requires_directory,
            confinement,
            recursive,
            session_id,
            mapping: None,
            current: None,
            directory_modes: Vec::new(),
            stats: TransferStats::default(),
        })
    }

    pub async fn receive(
        &mut self,
        frame: TransferBinaryFrame,
    ) -> Result<Option<TransferStats>, ProtocolError> {
        if frame.session_id != self.session_id {
            return Err(ProtocolError::new(
                "transfer_identity_mismatch",
                "transfer frame belongs to another session",
            ));
        }
        match frame.kind {
            TransferBinaryKind::Entry => {
                if self.current.is_some() {
                    return Err(ProtocolError::new(
                        "invalid_transfer_order",
                        "received a new entry before the previous entry ended",
                    ));
                }
                let entry: TransferEntry = serde_json::from_slice(&frame.payload)
                    .map_err(|error| transfer_error("invalid_transfer_entry", error))?;
                self.begin_entry(entry).await?;
                Ok(None)
            }
            TransferBinaryKind::Data => {
                let Some(CurrentEntry::File {
                    file,
                    expected,
                    written,
                    ..
                }) = self.current.as_mut()
                else {
                    return Err(ProtocolError::new(
                        "invalid_transfer_order",
                        "received file data without an active file entry",
                    ));
                };
                let next = written.saturating_add(frame.payload.len() as u64);
                if next > *expected {
                    return Err(ProtocolError::new(
                        "file_size_mismatch",
                        "received more file data than declared",
                    ));
                }
                file.write_all(&frame.payload)
                    .await
                    .map_err(|error| transfer_error("destination_write_failed", error))?;
                *written = next;
                self.stats.bytes = self.stats.bytes.saturating_add(frame.payload.len() as u64);
                Ok(None)
            }
            TransferBinaryKind::EntryEnd => {
                self.finish_entry().await?;
                self.stats.entries = self.stats.entries.saturating_add(1);
                Ok(None)
            }
            TransferBinaryKind::TransferEnd => {
                if self.current.is_some() {
                    return Err(ProtocolError::new(
                        "invalid_transfer_order",
                        "transfer ended before the current entry",
                    ));
                }
                if self.mapping.is_none() {
                    return Err(ProtocolError::new(
                        "empty_transfer",
                        "transfer contained no entries",
                    ));
                }
                let declared: TransferStats = serde_json::from_slice(&frame.payload)
                    .map_err(|error| transfer_error("invalid_transfer_summary", error))?;
                if declared != self.stats {
                    return Err(ProtocolError::new(
                        "transfer_summary_mismatch",
                        "transfer entry or byte count did not match the sender summary",
                    ));
                }
                for (path, mode) in self.directory_modes.iter().rev() {
                    set_mode(path, *mode).await?;
                }
                Ok(Some(self.stats))
            }
        }
    }

    pub async fn cancel(&mut self) {
        if let Some(CurrentEntry::File { temporary, .. }) = self.current.take() {
            let _ = tokio::fs::remove_file(temporary).await;
        }
    }

    async fn begin_entry(&mut self, entry: TransferEntry) -> Result<(), ProtocolError> {
        let components = validate_entry_path(&entry.path)?;
        if self.mapping.is_none() {
            if components.len() != 1 {
                return Err(ProtocolError::new(
                    "invalid_transfer_entry",
                    "the first transfer entry must be the source root",
                ));
            }
            if entry.kind == TransferEntryKind::Directory && !self.recursive {
                return Err(ProtocolError::new(
                    "recursive_required",
                    "source is a directory; use --recursive",
                ));
            }
            self.mapping = Some(self.create_mapping(&components[0], entry.kind).await?);
        }
        let destination = self.entry_destination(&components)?;
        self.validate_destination(&destination).await?;
        match entry.kind {
            TransferEntryKind::Directory => {
                if destination.exists() && !destination.is_dir() {
                    return Err(ProtocolError::new(
                        "destination_type_mismatch",
                        format!("destination is not a directory: {}", destination.display()),
                    ));
                }
                tokio::fs::create_dir_all(&destination)
                    .await
                    .map_err(|error| {
                        path_error("destination_create_failed", &destination, error)
                    })?;
                self.validate_destination(&destination).await?;
                self.directory_modes.push((destination, entry.mode));
                self.current = Some(CurrentEntry::Directory);
            }
            TransferEntryKind::File => {
                let parent = destination.parent().ok_or_else(|| {
                    ProtocolError::new("invalid_destination", "destination has no parent directory")
                })?;
                if !parent.is_dir() {
                    return Err(ProtocolError::new(
                        "destination_not_found",
                        format!("destination directory does not exist: {}", parent.display()),
                    ));
                }
                self.validate_destination(parent).await?;
                if destination.is_dir() {
                    return Err(ProtocolError::new(
                        "destination_type_mismatch",
                        format!("destination is a directory: {}", destination.display()),
                    ));
                }
                let temporary = parent.join(format!(
                    ".treer-upload-{}-{}",
                    self.session_id,
                    Uuid::new_v4().simple()
                ));
                let file = tokio::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&temporary)
                    .await
                    .map_err(|error| path_error("destination_create_failed", &temporary, error))?;
                self.current = Some(CurrentEntry::File {
                    file,
                    temporary,
                    destination,
                    expected: entry.size,
                    written: 0,
                    mode: entry.mode,
                });
            }
        }
        Ok(())
    }

    async fn finish_entry(&mut self) -> Result<(), ProtocolError> {
        let current = self.current.take().ok_or_else(|| {
            ProtocolError::new(
                "invalid_transfer_order",
                "received entry end without an entry",
            )
        })?;
        let CurrentEntry::File {
            mut file,
            temporary,
            destination,
            expected,
            written,
            mode,
        } = current
        else {
            return Ok(());
        };
        if written != expected {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(ProtocolError::new(
                "file_size_mismatch",
                format!("received {written} bytes, expected {expected}"),
            ));
        }
        if let Err(error) = file.flush().await {
            drop(file);
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(transfer_error("destination_write_failed", error));
        }
        drop(file);
        if let Err(error) = tokio::fs::rename(&temporary, &destination).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(path_error("destination_commit_failed", &destination, error));
        }
        set_mode(&destination, mode).await
    }

    async fn create_mapping(
        &self,
        source_root: &str,
        source_kind: TransferEntryKind,
    ) -> Result<DestinationMapping, ProtocolError> {
        match tokio::fs::symlink_metadata(&self.destination).await {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(ProtocolError::new(
                "unsupported_file_type",
                format!(
                    "destination is a symbolic link: {}",
                    self.destination.display()
                ),
            )),
            Ok(metadata) if metadata.is_dir() => Ok(DestinationMapping {
                source_root: source_root.to_string(),
                destination_root: self.destination.join(source_root),
            }),
            Ok(metadata) if metadata.is_file() && source_kind == TransferEntryKind::File => {
                Ok(DestinationMapping {
                    source_root: source_root.to_string(),
                    destination_root: self.destination.clone(),
                })
            }
            Ok(_) => Err(ProtocolError::new(
                "destination_type_mismatch",
                format!(
                    "cannot copy into destination: {}",
                    self.destination.display()
                ),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if self.destination_requires_directory {
                    return Err(ProtocolError::new(
                        "destination_not_found",
                        format!(
                            "destination directory does not exist: {}",
                            self.destination.display()
                        ),
                    ));
                }
                let parent = self.destination.parent().ok_or_else(|| {
                    ProtocolError::new("invalid_destination", "destination has no parent directory")
                })?;
                if !parent.is_dir() {
                    return Err(ProtocolError::new(
                        "destination_not_found",
                        format!("destination directory does not exist: {}", parent.display()),
                    ));
                }
                self.validate_destination(parent).await?;
                Ok(DestinationMapping {
                    source_root: source_root.to_string(),
                    destination_root: self.destination.clone(),
                })
            }
            Err(error) => Err(path_error(
                "destination_metadata_failed",
                &self.destination,
                error,
            )),
        }
    }

    fn entry_destination(&self, components: &[String]) -> Result<PathBuf, ProtocolError> {
        let mapping = self.mapping.as_ref().ok_or_else(|| {
            ProtocolError::new(
                "invalid_transfer_order",
                "destination mapping is not initialized",
            )
        })?;
        if components.first() != Some(&mapping.source_root) {
            return Err(ProtocolError::new(
                "invalid_transfer_entry",
                "all entries must share the same source root",
            ));
        }
        Ok(components[1..]
            .iter()
            .fold(mapping.destination_root.clone(), |path, part| {
                path.join(part)
            }))
    }

    async fn validate_destination(&self, path: &Path) -> Result<(), ProtocolError> {
        let Some(root) = &self.confinement else {
            return Ok(());
        };
        let mut existing = path;
        while !existing.exists() {
            existing = existing.parent().ok_or_else(|| {
                ProtocolError::new("invalid_destination", "destination escapes workspace root")
            })?;
        }
        let canonical = tokio::fs::canonicalize(existing)
            .await
            .map_err(|error| path_error("destination_metadata_failed", existing, error))?;
        if !canonical.starts_with(root) {
            return Err(ProtocolError::new(
                "invalid_destination",
                "destination escapes workspace root",
            ));
        }
        if path.exists() {
            let metadata = tokio::fs::symlink_metadata(path)
                .await
                .map_err(|error| path_error("destination_metadata_failed", path, error))?;
            if metadata.file_type().is_symlink() {
                return Err(ProtocolError::new(
                    "unsupported_file_type",
                    format!("destination is a symbolic link: {}", path.display()),
                ));
            }
        }
        Ok(())
    }
}

fn validate_requested_path(path: &Path) -> Result<(), ProtocolError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(ProtocolError::new(
            "invalid_destination",
            "remote paths must be relative to the workspace root",
        ));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_) | Component::CurDir) {
            return Err(ProtocolError::new(
                "invalid_destination",
                "remote paths cannot contain parent components",
            ));
        }
    }
    Ok(())
}

pub fn validate_remote_source(root: &Path, requested: &Path) -> Result<PathBuf, ProtocolError> {
    validate_requested_path(requested)?;
    let canonical_root = std::fs::canonicalize(root)
        .map_err(|error| path_error("invalid_workspace_root", root, error))?;
    let requested_path = canonical_root.join(requested);
    let metadata = std::fs::symlink_metadata(&requested_path)
        .map_err(|error| path_error("source_not_found", &requested_path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(ProtocolError::new(
            "unsupported_file_type",
            format!("source is a symbolic link: {}", requested_path.display()),
        ));
    }
    let canonical = std::fs::canonicalize(&requested_path)
        .map_err(|error| path_error("source_not_found", &requested_path, error))?;
    if !canonical.starts_with(canonical_root) {
        return Err(ProtocolError::new(
            "invalid_source",
            "source escapes workspace root",
        ));
    }
    Ok(canonical)
}

fn validate_entry_path(path: &str) -> Result<Vec<String>, ProtocolError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(ProtocolError::new(
            "invalid_transfer_entry",
            "entry paths must be non-empty and relative",
        ));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(ProtocolError::new(
                "invalid_transfer_entry",
                "entry paths cannot contain parent or current-directory components",
            ));
        };
        let part = part.to_str().ok_or_else(|| {
            ProtocolError::new("invalid_transfer_entry", "entry path is not UTF-8")
        })?;
        parts.push(part.to_string());
    }
    if parts.is_empty() {
        return Err(ProtocolError::new(
            "invalid_transfer_entry",
            "entry path is empty",
        ));
    }
    Ok(parts)
}

#[cfg(unix)]
fn file_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
async fn set_mode(path: &Path, mode: Option<u32>) -> Result<(), ProtocolError> {
    use std::os::unix::fs::PermissionsExt;
    let Some(mode) = mode else { return Ok(()) };
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777))
        .await
        .map_err(|error| path_error("destination_permissions_failed", path, error))
}

#[cfg(not(unix))]
async fn set_mode(_path: &Path, _mode: Option<u32>) -> Result<(), ProtocolError> {
    Ok(())
}

fn transfer_error(code: &str, error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(code, error.to_string())
}

fn path_error(code: &str, path: &Path, error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(code, format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("treer-transfer-{name}-{}", Uuid::new_v4().simple()))
    }

    #[test]
    fn transfer_entries_reject_parent_paths() {
        let error = validate_entry_path("root/../secret").expect_err("parent path must fail");
        assert_eq!(error.code, "invalid_transfer_entry");
    }

    #[test]
    fn remote_paths_are_workspace_relative() {
        assert!(validate_requested_path(Path::new("src/main.rs")).is_ok());
        assert!(validate_requested_path(Path::new("../outside")).is_err());
        assert!(validate_requested_path(Path::new("/tmp/outside")).is_err());
    }

    #[tokio::test]
    async fn recursive_binary_tree_round_trips() {
        let root = test_directory("round-trip");
        let source = root.join("source");
        let nested = source.join("nested");
        tokio::fs::create_dir_all(&nested)
            .await
            .expect("create source");
        tokio::fs::write(source.join("plain.txt"), b"hello\nworld\n")
            .await
            .expect("write text");
        tokio::fs::write(nested.join("binary.bin"), [0_u8, 1, 2, 0xff, 0, 10])
            .await
            .expect("write binary");

        let session_id = "copy_test".to_string();
        let destination = root.join("received");
        let mut receiver =
            TransferReceiver::new(destination.clone(), None, true, session_id.clone())
                .await
                .expect("create receiver");
        let (tx, mut rx) = mpsc::channel(2);
        let producer = tokio::spawn(stream_path(source, true, session_id, tx));
        let received = loop {
            let frame = rx.recv().await.expect("transfer frame");
            if let Some(stats) = receiver.receive(frame).await.expect("receive frame") {
                break stats;
            }
        };
        let sent = producer
            .await
            .expect("producer task")
            .expect("stream source");
        assert_eq!(received, sent);
        assert_eq!(sent.entries, 4);
        assert_eq!(
            tokio::fs::read(destination.join("nested/binary.bin"))
                .await
                .expect("read binary"),
            [0_u8, 1, 2, 0xff, 0, 10]
        );
        assert_eq!(
            tokio::fs::read(destination.join("plain.txt"))
                .await
                .expect("read text"),
            b"hello\nworld\n"
        );
        tokio::fs::remove_dir_all(root)
            .await
            .expect("remove test tree");
    }

    #[tokio::test]
    async fn confined_receivers_reject_absolute_destinations() {
        let root = test_directory("confinement");
        tokio::fs::create_dir_all(&root).await.expect("create root");
        let error = TransferReceiver::new(
            PathBuf::from("/tmp/outside"),
            Some(root.clone()),
            false,
            "copy_test".to_string(),
        )
        .await
        .err()
        .expect("absolute destination must fail");
        assert_eq!(error.code, "invalid_destination");
        tokio::fs::remove_dir_all(root)
            .await
            .expect("remove test root");
    }
}
