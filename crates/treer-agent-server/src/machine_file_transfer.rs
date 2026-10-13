//! Short-lived Controller-owned transfer handles; Proxy owns policy and audit.
//! All state transitions are serialized with disconnect/expiry and digest checks.
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
use treer_protocol::{
    MachineFileTransferRequest as Request, MachineTransferActor, ProtocolError,
    MACHINE_TRANSFER_CHUNK_BYTES, MACHINE_TRANSFER_DEFAULT_LIMIT, MACHINE_TRANSFER_HARD_LIMIT,
    MACHINE_TRANSFER_IDLE_SECONDS, MACHINE_TRANSFER_MAX_ACTIVE, MACHINE_TRANSFER_TOTAL_SECONDS,
};

fn failure(code: &str, message: &str) -> ProtocolError {
    ProtocolError::new(code, message)
}

fn source_metadata_changed(original: &std::fs::Metadata, current: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        original.mtime() != current.mtime()
            || original.mtime_nsec() != current.mtime_nsec()
            || original.ctime() != current.ctime()
            || original.ctime_nsec() != current.ctime_nsec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        original.last_write_time() != current.last_write_time()
            || original.creation_time() != current.creation_time()
            || original.file_attributes() != current.file_attributes()
    }
}

fn io_failure(error: std::io::Error) -> ProtocolError {
    if treer_platform::machine_file::is_publication_uncertain(&error) {
        return failure("machine_file_commit_uncertain", "destination was published but durability/cleanup is unconfirmed; inspect before retrying");
    }
    use std::io::ErrorKind::*;
    let (code, message) = match error.kind() {
        AlreadyExists => (
            "machine_file_exists",
            "destination already exists; use --force to replace it",
        ),
        NotFound => (
            "machine_file_not_found",
            "file or parent directory is unavailable",
        ),
        PermissionDenied => ("machine_file_denied", "file transfer path is not permitted"),
        InvalidInput => (
            "invalid_machine_path",
            "file transfer requires a safe root-relative regular-file path",
        ),
        _ => (
            "machine_file_io_failed",
            "file transfer filesystem operation failed",
        ),
    };
    failure(code, message)
}

#[cfg(any(unix, windows))]
enum Direction {
    Put {
        staged: Option<treer_platform::machine_file::StagedFile>,
        overwrite: bool,
        expected_size: Option<u64>,
    },
    Get {
        file: std::fs::File,
        original: std::fs::Metadata,
        eof: bool,
    },
}
#[cfg(any(unix, windows))]
struct Pending {
    actor: MachineTransferActor,
    path: String,
    created: Instant,
    touched: Instant,
    limit: u64,
    offset: u64,
    hash: Sha256,
    direction: Direction,
}

pub struct Transfers {
    root: PathBuf,
    workspace_id: String,
    protected: Vec<PathBuf>,
    limit: u64,
    recovered: std::sync::atomic::AtomicBool,
    recovery_lease: Mutex<Option<std::fs::File>>,
    generation: std::sync::atomic::AtomicU64,
    #[cfg(any(unix, windows))]
    pending: Mutex<HashMap<String, Pending>>,
}

impl Transfers {
    pub fn new(
        root: PathBuf,
        workspace_id: String,
        protected: Vec<PathBuf>,
        configured_limit: u64,
    ) -> Self {
        Self {
            root,
            workspace_id,
            protected,
            limit: configured_limit.clamp(1, MACHINE_TRANSFER_HARD_LIMIT),
            recovered: std::sync::atomic::AtomicBool::new(false),
            recovery_lease: Mutex::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
            #[cfg(any(unix, windows))]
            pending: Mutex::new(HashMap::new()),
        }
    }
    pub fn default_limit() -> u64 {
        MACHINE_TRANSFER_DEFAULT_LIMIT
    }
    pub fn info(&self) -> Value {
        json!({"capabilities":[treer_protocol::CAPABILITY_MACHINE_FILE_TRANSFER], "root":self.root, "max_bytes":self.limit, "chunk_bytes":MACHINE_TRANSFER_CHUNK_BYTES})
    }
    pub fn clear(&self) {
        #[cfg(any(unix, windows))]
        if let Ok(mut pending) = self.pending.lock() {
            self.generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            pending.clear();
        }
    }
    pub fn expire(&self) {
        #[cfg(any(unix, windows))]
        if let Ok(mut pending) = self.pending.try_lock() {
            pending.retain(|_, entry| {
                entry.touched.elapsed() < Duration::from_secs(MACHINE_TRANSFER_IDLE_SECONDS)
                    && entry.created.elapsed() < Duration::from_secs(MACHINE_TRANSFER_TOTAL_SECONDS)
            });
        }
    }

    #[cfg(not(any(unix, windows)))]
    pub fn execute(
        &self,
        _actor: MachineTransferActor,
        _authorized_path: &str,
        _request: Request,
    ) -> Result<Value, ProtocolError> {
        Err(failure(
            "file_transfer_unsupported",
            "safe native file-transfer operations are not available on this Controller platform",
        ))
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
    }

    #[cfg(test)]
    pub fn execute(
        &self,
        actor: MachineTransferActor,
        authorized_path: &str,
        request: Request,
    ) -> Result<Value, ProtocolError> {
        self.execute_generation(self.generation(), actor, authorized_path, request)
    }

    #[cfg(any(unix, windows))]
    pub fn execute_generation(
        &self,
        generation: u64,
        actor: MachineTransferActor,
        authorized_path: &str,
        request: Request,
    ) -> Result<Value, ProtocolError> {
        use std::io::{Read, Write};
        if actor.workspace_id != self.workspace_id
            || actor.id.is_empty()
            || !matches!(actor.kind.as_str(), "agent" | "user" | "machine" | "human")
        {
            return Err(failure(
                "machine_file_denied",
                "file transfer caller does not belong to this workspace",
            ));
        }
        treer_platform::machine_file::validate_relative_path(authorized_path)
            .map_err(io_failure)?;
        self.expire();
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| failure("state_error", "transfer registry unavailable"))?;
        if generation != self.generation() {
            return Err(failure(
                "machine_transfer_stale",
                "Controller connection changed; start a new transfer",
            ));
        }
        self.recover_staging()?;
        match &request {
            Request::PutBegin { path, .. } | Request::GetBegin { path, .. } => {
                if path != authorized_path {
                    return Err(failure(
                        "machine_file_denied",
                        "authorized path differs from transfer path",
                    ));
                }
                if pending.len() >= MACHINE_TRANSFER_MAX_ACTIVE
                    || pending
                        .values()
                        .filter(|entry| entry.actor == actor)
                        .count()
                        >= 2
                {
                    return Err(failure(
                        "machine_transfer_limit",
                        "too many active file transfers",
                    ));
                }
            }
            _ => {}
        }
        match request {
            Request::PutBegin {
                path,
                overwrite,
                mode,
                size,
                limit,
            } => {
                let limit = limit.min(self.limit);
                if limit == 0 || size.is_some_and(|size| size > limit) {
                    return Err(failure(
                        "machine_file_too_large",
                        "file exceeds transfer size limit",
                    ));
                }
                let root = self.open_root()?;
                if root.available_bytes().map_err(io_failure)? < size.unwrap_or(limit) {
                    return Err(failure(
                        "machine_file_no_space",
                        "insufficient available space for bounded staging",
                    ));
                }
                let id = format!("xfer_{}", uuid::Uuid::new_v4().simple());
                self.record_staging(&pending, &path, &id)?;
                let staged = root
                    .stage(&path, &format!(".treer-transfer-{id}"), mode)
                    .map_err(io_failure)?;
                // No destination bytes are visible before a verified finish.
                let now = Instant::now();
                pending.insert(
                    id.clone(),
                    Pending {
                        actor,
                        path: path.clone(),
                        created: now,
                        touched: now,
                        limit,
                        offset: 0,
                        hash: Sha256::new(),
                        direction: Direction::Put {
                            staged: Some(staged),
                            overwrite,
                            expected_size: size,
                        },
                    },
                );
                Ok(
                    json!({"id":id,"path":path,"limit":limit,"chunk_bytes":MACHINE_TRANSFER_CHUNK_BYTES,"size":size}),
                )
            }
            Request::GetBegin { path, limit } => {
                let limit = limit.min(self.limit);
                let file = self.open_root()?.read(&path).map_err(io_failure)?;
                let original = file.metadata().map_err(io_failure)?;
                if limit == 0 || original.len() > limit {
                    return Err(failure(
                        "machine_file_too_large",
                        "file exceeds transfer size limit",
                    ));
                }
                let size = original.len();
                let now = Instant::now();
                let id = format!("xfer_{}", uuid::Uuid::new_v4().simple());
                pending.insert(
                    id.clone(),
                    Pending {
                        actor,
                        path: path.clone(),
                        created: now,
                        touched: now,
                        limit,
                        offset: 0,
                        hash: Sha256::new(),
                        direction: Direction::Get {
                            file,
                            original,
                            eof: false,
                        },
                    },
                );
                Ok(
                    json!({"id":id,"path":path,"limit":limit,"chunk_bytes":MACHINE_TRANSFER_CHUNK_BYTES,"size":size}),
                )
            }
            request => {
                let id = match &request {
                    Request::PutChunk { id, .. }
                    | Request::GetChunk { id, .. }
                    | Request::PutFinish { id, .. }
                    | Request::GetFinish { id }
                    | Request::Abort { id, .. } => id,
                    _ => unreachable!(),
                };
                let Some(mut entry) = pending.remove(id) else {
                    return Err(failure(
                        "machine_transfer_not_found",
                        "file transfer expired or is no longer active",
                    ));
                };
                if entry.actor != actor || entry.path != authorized_path {
                    pending.insert(id.clone(), entry);
                    return Err(failure(
                        "machine_file_denied",
                        "file transfer does not belong to this caller/path",
                    ));
                }
                entry.touched = Instant::now();
                let result = (|| -> Result<Value, ProtocolError> {
                    match request.clone() {
                        Request::PutChunk {
                            offset,
                            content_base64,
                            ..
                        } => {
                            if offset != entry.offset {
                                return Err(failure(
                                    "machine_transfer_offset",
                                    "file transfer offset does not match",
                                ));
                            }
                            if content_base64.len() > MACHINE_TRANSFER_CHUNK_BYTES.div_ceil(3) * 4 {
                                Err(failure(
                                    "machine_transfer_chunk_too_large",
                                    "file transfer chunk exceeds limit",
                                ))
                            } else {
                                let bytes = base64::engine::general_purpose::STANDARD
                                    .decode(content_base64)
                                    .map_err(|_| {
                                        failure(
                                            "invalid_machine_file",
                                            "invalid file transfer encoding",
                                        )
                                    })?;
                                if bytes.len() > MACHINE_TRANSFER_CHUNK_BYTES
                                    || entry.offset + bytes.len() as u64 > entry.limit
                                {
                                    Err(failure(
                                        "machine_file_too_large",
                                        "file exceeds transfer size limit",
                                    ))
                                } else if let Direction::Put { staged, .. } = &mut entry.direction {
                                    staged
                                        .as_mut()
                                        .unwrap()
                                        .file_mut()
                                        .write_all(&bytes)
                                        .map_err(io_failure)?;
                                    entry.hash.update(&bytes);
                                    entry.offset += bytes.len() as u64;
                                    Ok(json!({"offset":entry.offset}))
                                } else {
                                    Err(failure(
                                        "machine_transfer_direction",
                                        "transfer is not a put",
                                    ))
                                }
                            }
                        }
                        Request::GetChunk { offset, .. } => {
                            if offset != entry.offset {
                                return Err(failure(
                                    "machine_transfer_offset",
                                    "file transfer offset does not match",
                                ));
                            }
                            if let Direction::Get { file, eof, .. } = &mut entry.direction {
                                let mut bytes = vec![0u8; MACHINE_TRANSFER_CHUNK_BYTES];
                                let count = file.read(&mut bytes).map_err(io_failure)?;
                                bytes.truncate(count);
                                if entry.offset + count as u64 > entry.limit {
                                    Err(failure(
                                        "machine_file_too_large",
                                        "file exceeds transfer size limit",
                                    ))
                                } else {
                                    *eof = count == 0;
                                    entry.hash.update(&bytes);
                                    entry.offset += count as u64;
                                    Ok(
                                        json!({"offset":offset,"content_base64":base64::engine::general_purpose::STANDARD.encode(&bytes),"eof":eof}),
                                    )
                                }
                            } else {
                                Err(failure(
                                    "machine_transfer_direction",
                                    "transfer is not a get",
                                ))
                            }
                        }
                        Request::PutFinish { size, sha256, .. } => {
                            if let Direction::Put { expected_size, .. } = &entry.direction {
                                if size != entry.offset
                                    || expected_size.is_some_and(|expected| expected != size)
                                    || sha256 != format!("{:x}", entry.hash.clone().finalize())
                                {
                                    Err(failure(
                                        "machine_file_checksum_mismatch",
                                        "file transfer size or SHA-256 mismatch; discarded",
                                    ))
                                } else {
                                    let Direction::Put {
                                        staged, overwrite, ..
                                    } = &mut entry.direction
                                    else {
                                        unreachable!()
                                    };
                                    self.open_root()?
                                        .validate_target(&entry.path)
                                        .map_err(io_failure)?;
                                    staged
                                        .take()
                                        .unwrap()
                                        .commit(*overwrite)
                                        .map_err(io_failure)?;
                                    Ok(json!({"path":entry.path,"size":size,"sha256":sha256}))
                                }
                            } else {
                                Err(failure(
                                    "machine_transfer_direction",
                                    "transfer is not a put",
                                ))
                            }
                        }
                        Request::GetFinish { .. } => {
                            if let Direction::Get {
                                file,
                                original,
                                eof,
                            } = &entry.direction
                            {
                                let current = file.metadata().map_err(io_failure)?;
                                if !eof
                                    || current.len() != entry.offset
                                    || original.len() != current.len()
                                    || source_metadata_changed(original, &current)
                                {
                                    Err(failure(
                                        "machine_file_changed",
                                        "source changed or read is incomplete",
                                    ))
                                } else {
                                    Ok(
                                        json!({"path":entry.path,"size":entry.offset,"sha256":format!("{:x}",entry.hash.clone().finalize())}),
                                    )
                                }
                            } else {
                                Err(failure(
                                    "machine_transfer_direction",
                                    "transfer is not a get",
                                ))
                            }
                        }
                        Request::Abort { read, .. } => {
                            if read != matches!(entry.direction, Direction::Get { .. }) {
                                return Err(failure(
                                    "machine_transfer_direction",
                                    "transfer abort direction does not match",
                                ));
                            }
                            Ok(json!({"aborted":true}))
                        }
                        _ => unreachable!(),
                    }
                })();
                if result.is_ok()
                    && matches!(request, Request::PutChunk { .. } | Request::GetChunk { .. })
                {
                    pending.insert(id.clone(), entry);
                }
                result
            }
        }
    }

    #[cfg(any(unix, windows))]
    fn open_root(&self) -> Result<treer_platform::machine_file::Root, ProtocolError> {
        let mut protected = Vec::new();
        for path in &self.protected {
            match treer_platform::machine_file::open_protected(path) {
                Ok(file) => protected.push(file),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_failure(error)),
            }
        }
        let mut root =
            treer_platform::machine_file::Root::open(&self.root, &protected).map_err(io_failure)?;
        for path in &self.protected {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                std::env::current_dir().map_err(io_failure)?.join(path)
            };
            root.reserve_path(&path).map_err(io_failure)?;
        }
        Ok(root)
    }

    fn recover_staging(&self) -> Result<(), ProtocolError> {
        use std::{io::Read, sync::atomic::Ordering};
        if self.recovered.load(Ordering::Acquire) {
            return Ok(());
        }
        let root = self.open_root()?;
        let journal = root.transfer_journal().map_err(io_failure)?;
        let lease = journal.exclusive_lease().map_err(|_| {
            failure(
                "machine_transfer_busy",
                "another Controller owns file-transfer recovery; retry after it exits",
            )
        })?;
        journal
            .cleanup_staging("pending.json", ".treer-transfer-xfer_journal")
            .map_err(io_failure)?;
        let records: Vec<(String, String)> = match journal.read("pending.json") {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(64 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(io_failure)?;
                if bytes.len() > 64 * 1024 {
                    return Err(failure(
                        "state_error",
                        "transfer recovery metadata exceeds limit",
                    ));
                }
                serde_json::from_slice(&bytes)
                    .map_err(|_| failure("state_error", "invalid transfer recovery metadata"))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(io_failure(error)),
        };
        if records.len() > MACHINE_TRANSFER_MAX_ACTIVE + 1 {
            return Err(failure("state_error", "too many transfer recovery records"));
        }
        for (path, id) in records {
            if id.len() != 37
                || !id.starts_with("xfer_")
                || !id[5..].bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(failure(
                    "state_error",
                    "invalid transfer recovery identifier",
                ));
            }
            root.cleanup_staging(&path, &format!(".treer-transfer-{id}"))
                .map_err(io_failure)?;
        }
        self.write_journal(&journal, &[])?;
        *self
            .recovery_lease
            .lock()
            .map_err(|_| failure("state_error", "transfer recovery lock unavailable"))? =
            Some(lease);
        self.recovered.store(true, Ordering::Release);
        Ok(())
    }

    fn record_staging(
        &self,
        pending: &HashMap<String, Pending>,
        path: &str,
        id: &str,
    ) -> Result<(), ProtocolError> {
        let mut records: Vec<_> = pending
            .iter()
            .filter(|(_, entry)| matches!(&entry.direction, Direction::Put { .. }))
            .map(|(id, entry)| (entry.path.clone(), id.clone()))
            .collect();
        records.push((path.to_owned(), id.to_owned()));
        self.write_journal(
            &self.open_root()?.transfer_journal().map_err(io_failure)?,
            &records,
        )
    }

    fn write_journal(
        &self,
        journal: &treer_platform::machine_file::Root,
        records: &[(String, String)],
    ) -> Result<(), ProtocolError> {
        use std::io::Write;
        let bytes = serde_json::to_vec(records)
            .map_err(|_| failure("state_error", "cannot serialize transfer recovery metadata"))?;
        let mut staged = journal
            .stage("pending.json", ".treer-transfer-xfer_journal", 0o600)
            .map_err(io_failure)?;
        staged.file_mut().write_all(&bytes).map_err(io_failure)?;
        staged.commit(true).map_err(io_failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        transfers: Transfers,
        actor: MachineTransferActor,
    }
    impl Fixture {
        fn new(limit: u64) -> Self {
            let root = std::env::temp_dir()
                .join(format!("treer-transfer-registry-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let root = std::fs::canonicalize(root).unwrap();
            Self {
                transfers: Transfers::new(root.clone(), "ws_fixture".into(), vec![], limit),
                root,
                actor: MachineTransferActor {
                    workspace_id: "ws_fixture".into(),
                    kind: "agent".into(),
                    id: "ag_fixture".into(),
                },
            }
        }
        fn execute(&self, request: Request) -> Result<Value, ProtocolError> {
            self.transfers
                .execute(self.actor.clone(), "result", request)
        }
        fn begin(&self, overwrite: bool) -> String {
            self.execute(Request::PutBegin {
                path: "result".into(),
                overwrite,
                mode: 0o600,
                size: None,
                limit: MACHINE_TRANSFER_DEFAULT_LIMIT,
            })
            .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
        fn chunk(&self, id: &str, bytes: &[u8]) -> Result<Value, ProtocolError> {
            self.execute(Request::PutChunk {
                id: id.into(),
                offset: 0,
                content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
        }
        fn finish(&self, id: &str, bytes: &[u8]) -> Result<Value, ProtocolError> {
            self.execute(Request::PutFinish {
                id: id.into(),
                size: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(bytes)),
            })
        }
        fn visible_files(&self) -> usize {
            std::fs::read_dir(&self.root)
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().file_name() != ".treer")
                .count()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.transfers.clear();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn put_get_round_trip_checks_hash_and_never_overwrites_implicitly() {
        let f = Fixture::new(MACHINE_TRANSFER_DEFAULT_LIMIT);
        let bytes = b"\0binary\xff\n";
        let id = f.begin(false);
        f.chunk(&id, bytes).unwrap();
        f.finish(&id, bytes).unwrap();
        assert_eq!(std::fs::read(f.root.join("result")).unwrap(), bytes);
        let id = f.begin(false);
        f.chunk(&id, b"replacement").unwrap();
        assert_eq!(
            f.finish(&id, b"replacement").unwrap_err().code,
            "machine_file_exists"
        );
        assert_eq!(std::fs::read(f.root.join("result")).unwrap(), bytes);
        let id = f
            .execute(Request::GetBegin {
                path: "result".into(),
                limit: MACHINE_TRANSFER_DEFAULT_LIMIT,
            })
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let chunk = f
            .execute(Request::GetChunk {
                id: id.clone(),
                offset: 0,
            })
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(chunk["content_base64"].as_str().unwrap())
                .unwrap(),
            bytes
        );
        assert!(!chunk["eof"].as_bool().unwrap());
        assert!(f
            .execute(Request::GetChunk {
                id: id.clone(),
                offset: bytes.len() as u64
            })
            .unwrap()["eof"]
            .as_bool()
            .unwrap());
        let complete = f.execute(Request::GetFinish { id }).unwrap();
        assert_eq!(complete["sha256"], format!("{:x}", Sha256::digest(bytes)));
        let id = f.begin(true);
        f.chunk(&id, b"new").unwrap();
        f.finish(&id, b"new").unwrap();
        assert_eq!(std::fs::read(f.root.join("result")).unwrap(), b"new");
    }

    #[test]
    fn corruption_limit_encoding_and_disconnect_discard_staging() {
        let f = Fixture::new(4);
        let id = f.begin(false);
        f.chunk(&id, b"safe").unwrap();
        assert_eq!(
            f.finish(&id, b"oops").unwrap_err().code,
            "machine_file_checksum_mismatch"
        );
        assert_eq!(f.visible_files(), 0);
        let id = f.begin(false);
        assert_eq!(
            f.chunk(&id, b"oversize").unwrap_err().code,
            "machine_file_too_large"
        );
        assert_eq!(f.visible_files(), 0);
        let id = f.begin(false);
        assert_eq!(
            f.execute(Request::PutChunk {
                id,
                offset: 0,
                content_base64: "invalid!".into()
            })
            .unwrap_err()
            .code,
            "invalid_machine_file"
        );
        assert_eq!(f.visible_files(), 0);
        let id = f.begin(false);
        f.chunk(&id, b"safe").unwrap();
        f.transfers.clear();
        assert_eq!(
            f.finish(&id, b"safe").unwrap_err().code,
            "machine_transfer_not_found"
        );
        assert_eq!(f.visible_files(), 0);
    }

    #[test]
    fn authenticated_actor_path_binding_expiry_and_active_limit() {
        let f = Fixture::new(64);
        let id = f.begin(false);
        let mut thief = f.actor.clone();
        thief.id = "ag_thief".into();
        assert_eq!(
            f.transfers
                .execute(
                    thief,
                    "result",
                    Request::Abort {
                        id: id.clone(),
                        read: false
                    }
                )
                .unwrap_err()
                .code,
            "machine_file_denied"
        );
        assert_eq!(
            f.transfers
                .execute(
                    f.actor.clone(),
                    "other",
                    Request::Abort {
                        id: id.clone(),
                        read: false
                    }
                )
                .unwrap_err()
                .code,
            "machine_file_denied"
        );
        let mut foreign = f.actor.clone();
        foreign.workspace_id = "ws_foreign".into();
        assert_eq!(
            f.transfers
                .execute(
                    foreign,
                    "result",
                    Request::Abort {
                        id: id.clone(),
                        read: false
                    }
                )
                .unwrap_err()
                .code,
            "machine_file_denied"
        );
        f.chunk(&id, b"safe").unwrap();
        f.finish(&id, b"safe").unwrap();
        let id = f.begin(true);
        f.transfers
            .pending
            .lock()
            .unwrap()
            .get_mut(&id)
            .unwrap()
            .touched -= Duration::from_secs(31);
        f.transfers.expire();
        assert_eq!(
            f.finish(&id, b"").unwrap_err().code,
            "machine_transfer_not_found"
        );
        let _first = f.begin(true);
        let _second = f.begin(true);
        assert_eq!(
            f.execute(Request::PutBegin {
                path: "result".into(),
                overwrite: true,
                mode: 0o600,
                size: None,
                limit: 64
            })
            .unwrap_err()
            .code,
            "machine_transfer_limit"
        );
    }

    #[test]
    fn crashed_transfer_child() {
        let Some(root) = std::env::var_os("TREER_TEST_TRANSFER_CRASH_ROOT") else {
            return;
        };
        let transfers = Transfers::new(root.clone().into(), "ws_fixture".into(), vec![], 64);
        let actor = MachineTransferActor {
            workspace_id: "ws_fixture".into(),
            kind: "agent".into(),
            id: "ag_fixture".into(),
        };
        let begin = transfers
            .execute(
                actor.clone(),
                "result",
                Request::PutBegin {
                    path: "result".into(),
                    overwrite: false,
                    mode: 0o600,
                    size: None,
                    limit: 64,
                },
            )
            .unwrap();
        transfers
            .execute(
                actor,
                "result",
                Request::PutChunk {
                    id: begin["id"].as_str().unwrap().into(),
                    offset: 0,
                    content_base64: base64::engine::general_purpose::STANDARD.encode(b"partial"),
                },
            )
            .unwrap();
        #[cfg(unix)]
        if std::env::var_os("TREER_TEST_TRANSFER_CRASH_AFTER_PUBLISH").is_some() {
            let id = begin["id"].as_str().unwrap();
            std::fs::hard_link(
                PathBuf::from(&root).join(format!(".treer-transfer-{id}")),
                PathBuf::from(&root).join("result"),
            )
            .unwrap();
        }
        // Exit without Rust destructors, just as an OS-killed Controller would.
        std::process::exit(0);
    }

    #[test]
    fn process_exit_without_drop_is_recovered_without_publishing_partial_files() {
        let f = Fixture::new(64);
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "machine_file_transfer::tests::crashed_transfer_child",
                "--nocapture",
            ])
            .env("TREER_TEST_TRANSFER_CRASH_ROOT", &f.root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(f.visible_files(), 1);
        assert!(!f.root.join("result").exists());
        let id = f.begin(false);
        assert_eq!(f.visible_files(), 1); // Only the new transfer's temp remains.
        f.execute(Request::Abort { id, read: false }).unwrap();
        assert_eq!(f.visible_files(), 0);
    }

    #[test]
    fn queued_old_connection_begin_is_fenced_after_disconnect() {
        let f = Fixture::new(64);
        let captured_generation = f.transfers.generation();
        f.transfers.clear();
        let error = f
            .transfers
            .execute_generation(
                captured_generation,
                f.actor.clone(),
                "result",
                Request::PutBegin {
                    path: "result".into(),
                    overwrite: false,
                    mode: 0o600,
                    size: Some(1),
                    limit: 64,
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "machine_transfer_stale");
        assert_eq!(std::fs::read_dir(&f.root).unwrap().count(), 0);
        let id = f.begin(false);
        f.execute(Request::Abort { id, read: false }).unwrap();
    }

    #[test]
    fn absent_and_atomically_replaced_core_state_paths_are_reserved_before_io() {
        let mut f = Fixture::new(64);
        f.transfers
            .protected
            .push(f.root.join("future/private.json"));
        for path in ["future/private.json", "FUTURE/private.json"] {
            let error = f
                .transfers
                .execute(
                    f.actor.clone(),
                    path,
                    Request::PutBegin {
                        path: path.into(),
                        overwrite: true,
                        mode: 0o600,
                        size: None,
                        limit: 64,
                    },
                )
                .unwrap_err();
            assert_eq!(error.code, "machine_file_denied");
        }
        std::fs::create_dir(f.root.join("future")).unwrap();
        for path in ["future/private.json", "future/PRIVATE.JSON"] {
            let error = f
                .transfers
                .execute(
                    f.actor.clone(),
                    path,
                    Request::PutBegin {
                        path: path.into(),
                        overwrite: true,
                        mode: 0o600,
                        size: None,
                        limit: 64,
                    },
                )
                .unwrap_err();
            assert_eq!(error.code, "machine_file_denied");
        }
        std::fs::write(f.root.join("future/private.json"), b"synthetic-secret").unwrap();
        let replacement = f.root.join("replacement");
        std::fs::write(&replacement, b"replacement-secret").unwrap();
        #[cfg(unix)]
        std::fs::rename(&replacement, f.root.join("future/private.json")).unwrap();
        #[cfg(windows)]
        {
            std::fs::remove_file(f.root.join("future/private.json")).unwrap();
            std::fs::rename(&replacement, f.root.join("future/private.json")).unwrap();
        }
        for path in ["future/private.json", "future/PRIVATE.JSON"] {
            let error = f
                .transfers
                .execute(
                    f.actor.clone(),
                    path,
                    Request::GetBegin {
                        path: path.into(),
                        limit: 64,
                    },
                )
                .unwrap_err();
            assert_eq!(error.code, "machine_file_denied");
        }
        assert_eq!(
            std::fs::read(f.root.join("future/private.json")).unwrap(),
            b"replacement-secret"
        );
    }

    #[cfg(unix)]
    #[test]
    fn crash_after_atomic_no_replace_preserves_published_file_and_recovers_temporary_link() {
        let f = Fixture::new(64);
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "machine_file_transfer::tests::crashed_transfer_child",
                "--nocapture",
            ])
            .env("TREER_TEST_TRANSFER_CRASH_ROOT", &f.root)
            .env("TREER_TEST_TRANSFER_CRASH_AFTER_PUBLISH", "1")
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(f.visible_files(), 2);
        let id = f.begin(true);
        f.execute(Request::Abort { id, read: false }).unwrap();
        assert_eq!(f.visible_files(), 1);
        assert_eq!(std::fs::read(f.root.join("result")).unwrap(), b"partial");
        let root = f.transfers.open_root().unwrap();
        assert!(root.read("result").is_ok());
    }
}
