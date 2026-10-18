//! Controller side of browser machine uploads (#166): the legacy base64 route
//! (append, no declared size) and the chunked, resumable route (declared
//! size, offset-bearing chunks, per-part SHA-256, idempotent commit receipt).
//! Staging reuses the platform `Root`/`StagedFile` boundary the CLI transfer
//! uses: the destination parent is held as a directory descriptor from begin
//! to publish, the staging file is created exclusively with owner-only mode
//! under a recorded random name, publish re-verifies the parent and uses an
//! atomic no-replace link when overwrite is off, and dropping a staged file
//! deletes it. Intents and receipts are journaled next to the transfer
//! journal so a restarted Controller sweeps only its own recorded names.
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use treer_platform::machine_file::{Root, StagedFile};
use treer_protocol::{
    ProtocolError, UploadMachineFileResponse, MACHINE_UPLOAD_CHUNK_BYTES,
    MACHINE_UPLOAD_IDLE_SECONDS, MACHINE_UPLOAD_MAX_ACTIVE, MACHINE_UPLOAD_MAX_BYTES,
    MACHINE_UPLOAD_MAX_LIFETIME_SECONDS, MACHINE_UPLOAD_RESULT_RETENTION_SECONDS,
};

/// Free space the destination volume must keep beyond the declared sizes:
/// the larger of 5 percent of the file and 128 MiB.
const SPACE_RESERVE_MIN_BYTES: u64 = 128 * 1024 * 1024;
/// Final mode of an uploaded file (owner-only, like the CLI default).
const FINAL_MODE: u32 = 0o600;
const JOURNAL_INTENTS: &str = "uploads.json";
const JOURNAL_RESULTS: &str = "upload-results.json";

fn failure(code: &str, message: &str) -> ProtocolError {
    ProtocolError::new(code, message)
}

fn io_failure(error: std::io::Error) -> ProtocolError {
    if treer_platform::machine_file::is_publication_uncertain(&error) {
        return failure(
            "machine_upload_commit_uncertain",
            "destination was published but durability/cleanup is unconfirmed; inspect before retrying",
        );
    }
    use std::io::ErrorKind::*;
    let (code, message) = match error.kind() {
        AlreadyExists => ("machine_file_exists", "destination already exists"),
        NotFound => (
            "machine_file_not_found",
            "file or parent directory is unavailable",
        ),
        PermissionDenied => ("machine_file_denied", "upload path is not permitted"),
        InvalidInput => (
            "invalid_machine_path",
            "upload requires a safe root-relative regular-file path",
        ),
        Unsupported => (
            "machine_upload_publish_unsupported",
            "the destination filesystem cannot publish without replacing",
        ),
        _ => (
            "machine_upload_failed",
            "upload filesystem operation failed",
        ),
    };
    failure(code, message)
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartRecord {
    pub offset: u64,
    pub length: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredResult {
    upload_id: String,
    path: String,
    bytes_written: u64,
    sha256: String,
    committed_at: u64,
    #[serde(default)]
    cleanup_pending: bool,
}

struct Upload {
    path: String,
    staged: Option<StagedFile>,
    overwrite: bool,
    /// `None` on the legacy append route.
    declared: Option<u64>,
    written: u64,
    committed: u64,
    parts: Vec<PartRecord>,
    part_hash: Sha256,
    file_hash: Sha256,
    committed_hash: Sha256,
    created: Instant,
    touched: Instant,
    /// Test-only ageing added to the elapsed times the reaper compares, so a
    /// test can expire an upload without an `Instant` before the platform's
    /// epoch (Windows cannot represent one before boot).
    #[cfg(test)]
    aged_idle: Duration,
    #[cfg(test)]
    aged_lifetime: Duration,
}

impl Upload {
    fn idle_for(&self) -> Duration {
        #[cfg(test)]
        return self.touched.elapsed() + self.aged_idle;
        #[cfg(not(test))]
        self.touched.elapsed()
    }
    fn lived_for(&self) -> Duration {
        #[cfg(test)]
        return self.created.elapsed() + self.aged_lifetime;
        #[cfg(not(test))]
        self.created.elapsed()
    }
    fn chunked(&self) -> bool {
        self.declared.is_some()
    }
}

pub struct Uploads {
    root: PathBuf,
    server_id: String,
    protected: Vec<PathBuf>,
    entries: Mutex<HashMap<String, Upload>>,
    results: Mutex<Vec<StoredResult>>,
    recovered: std::sync::atomic::AtomicBool,
    /// Recorded staging names the startup sweep could not remove; they stay
    /// journaled so a later start retries them.
    unswept: Mutex<Vec<(String, String)>>,
}

/// Whether this Controller offers the chunked, resumable upload contract
/// (`machine.upload.v3`). The Windows code path compiles and is exercised by
/// the tests, but has not been validated on a real machine (#171), so the
/// capability is not advertised there and the command dispatch refuses the
/// v3 commands; the legacy append route stays available everywhere.
pub const fn chunked_upload_supported() -> bool {
    !cfg!(windows)
}

/// Test hook: the next chunk write for the named upload stores only half its
/// bytes and fails, as a full disk would. Keyed by upload id so parallel tests
/// in the same process cannot consume each other's injected failure.
#[cfg(test)]
pub(crate) static FAIL_NEXT_WRITE_FOR: std::sync::Mutex<Option<String>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn take_injected_write_failure(upload_id: &str) -> bool {
    let mut armed = FAIL_NEXT_WRITE_FOR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if armed.as_deref() == Some(upload_id) {
        *armed = None;
        true
    } else {
        false
    }
}

fn validate_upload_id(upload_id: &str) -> Result<(), ProtocolError> {
    if upload_id.len() < 8
        || upload_id.len() > 80
        || !upload_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(failure("invalid_machine_upload", "upload ID is invalid"));
    }
    Ok(())
}

pub(crate) fn validate_file_name(file_name: &str) -> Result<(), ProtocolError> {
    let mut components = std::path::Path::new(file_name).components();
    let valid = matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
        && !file_name.contains(['/', '\\'])
        && !file_name.chars().any(char::is_control);
    if !valid {
        return Err(failure(
            "invalid_machine_file_name",
            "file_name must be one file name without path separators",
        ));
    }
    Ok(())
}

/// Joins the requested directory and file name into the root-relative path
/// the platform boundary validates (`.`, empty and `./` prefixes mean root).
fn relative_path(directory: &str, file_name: &str) -> Result<String, ProtocolError> {
    validate_file_name(file_name)?;
    let directory = directory.trim();
    let directory = directory
        .strip_prefix("./")
        .unwrap_or(directory)
        .trim_end_matches('/');
    if std::path::Path::new(directory).is_absolute() {
        return Err(failure(
            "invalid_machine_path",
            "directory must be relative to the machine root",
        ));
    }
    let path = if directory.is_empty() || directory == "." {
        file_name.to_owned()
    } else {
        format!("{directory}/{file_name}")
    };
    treer_platform::machine_file::validate_relative_path(&path).map_err(io_failure)?;
    Ok(path)
}

impl Uploads {
    pub fn new(root: PathBuf, server_id: String, protected: Vec<PathBuf>) -> Self {
        Self {
            root,
            server_id,
            protected,
            entries: Mutex::new(HashMap::new()),
            results: Mutex::new(Vec::new()),
            recovered: std::sync::atomic::AtomicBool::new(false),
            unswept: Mutex::new(Vec::new()),
        }
    }

    /// Recorded staging names the last sweep could not remove (for tests and
    /// diagnostics).
    #[cfg(test)]
    pub fn unswept_for_test(&self) -> Vec<(String, String)> {
        self.unswept.lock().map(|u| u.clone()).unwrap_or_default()
    }

    fn open_root(&self) -> Result<Root, ProtocolError> {
        let mut protected = Vec::new();
        for path in &self.protected {
            match treer_platform::machine_file::open_protected(path) {
                Ok(file) => protected.push(file),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_failure(error)),
            }
        }
        let mut root = Root::open(&self.root, &protected).map_err(io_failure)?;
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

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, Upload>>, ProtocolError> {
        self.entries
            .lock()
            .map_err(|_| failure("state_error", "upload registry lock poisoned"))
    }

    /// Startup sweep (once): deletes staging files recorded by a previous
    /// Controller instance, by recorded path and name only, and reloads the
    /// persisted receipts still inside their retention.
    fn recover(&self) -> Result<(), ProtocolError> {
        use std::sync::atomic::Ordering;
        if self.recovered.load(Ordering::Acquire) {
            return Ok(());
        }
        let root = self.open_root()?;
        let journal = root.transfer_journal().map_err(io_failure)?;
        for (file, temporary) in [
            (JOURNAL_INTENTS, ".treer-transfer-upl_journal"),
            (JOURNAL_RESULTS, ".treer-transfer-upl_results"),
        ] {
            journal
                .cleanup_staging(file, temporary)
                .map_err(io_failure)?;
        }
        let intents: Vec<(String, String)> = read_journal(&journal, JOURNAL_INTENTS)?;
        if intents.len() > MACHINE_UPLOAD_MAX_ACTIVE + 1 {
            return Err(failure("state_error", "too many upload recovery records"));
        }
        // A recorded name that cannot be removed now (held open by another
        // process, an antivirus scan, a stale handle on Windows) must not
        // block every later upload: it stays journaled for the next sweep and
        // is reported, and recovery continues with the others.
        let mut kept: Vec<(String, String)> = Vec::new();
        for (path, id) in intents {
            if validate_upload_id(&id).is_err() || !id.starts_with("upl_") {
                return Err(failure("state_error", "invalid upload recovery identifier"));
            }
            if treer_platform::machine_file::is_reserved_namespace(&path).map_err(io_failure)? {
                continue;
            }
            match root.cleanup_staging(&path, &format!(".treer-transfer-{id}")) {
                Ok(()) => {
                    tracing::warn!(upload_id = %id, "removed the staging file of an upload interrupted by a Controller restart");
                }
                Err(error) => {
                    tracing::warn!(upload_id = %id, ?error, "could not remove the staging file of an interrupted upload; kept for the next sweep");
                    kept.push((path, id));
                }
            }
        }
        write_journal(
            &journal,
            JOURNAL_INTENTS,
            ".treer-transfer-upl_journal",
            &kept,
        )?;
        *self
            .unswept
            .lock()
            .map_err(|_| failure("state_error", "upload sweep lock poisoned"))? = kept;
        let mut results: Vec<StoredResult> = read_journal(&journal, JOURNAL_RESULTS)?;
        let now = now_epoch();
        results.retain(|r| {
            now.saturating_sub(r.committed_at) < MACHINE_UPLOAD_RESULT_RETENTION_SECONDS
        });
        *self
            .results
            .lock()
            .map_err(|_| failure("state_error", "upload results lock poisoned"))? = results;
        self.recovered.store(true, Ordering::Release);
        Ok(())
    }

    /// Runs the startup sweep now instead of on the first upload command.
    pub fn recover_at_startup(&self) {
        if let Err(error) = self.recover() {
            tracing::warn!(
                ?error,
                "machine upload recovery failed at startup; retried on the first upload"
            );
        }
    }

    fn persist_intents(&self, entries: &HashMap<String, Upload>) -> Result<(), ProtocolError> {
        let mut records: Vec<(String, String)> =
            self.unswept.lock().map(|u| u.clone()).unwrap_or_default();
        records.extend(
            entries
                .iter()
                .map(|(id, upload)| (upload.path.clone(), id.clone())),
        );
        let journal = self.open_root()?.transfer_journal().map_err(io_failure)?;
        write_journal(
            &journal,
            JOURNAL_INTENTS,
            ".treer-transfer-upl_journal",
            &records,
        )
    }

    fn persist_results(&self, results: &[StoredResult]) -> Result<(), ProtocolError> {
        let journal = self.open_root()?.transfer_journal().map_err(io_failure)?;
        write_journal(
            &journal,
            JOURNAL_RESULTS,
            ".treer-transfer-upl_results",
            &results.to_vec(),
        )
    }

    /// Begins staging. A declared size selects the chunked contract; `None`
    /// is the legacy append route.
    pub fn begin(
        &self,
        upload_id: &str,
        directory: &str,
        file_name: &str,
        overwrite: bool,
        size: Option<u64>,
    ) -> Result<(), ProtocolError> {
        validate_upload_id(upload_id)?;
        if size.is_some_and(|size| size > MACHINE_UPLOAD_MAX_BYTES) {
            return Err(failure(
                "machine_upload_too_large",
                &format!("uploaded files may contain at most {MACHINE_UPLOAD_MAX_BYTES} bytes"),
            ));
        }
        let path = relative_path(directory, file_name)?;
        self.recover()?;
        let root = self.open_root()?;
        root.validate_target(&path).map_err(io_failure)?;
        let mut entries = self.lock()?;
        if entries.contains_key(upload_id) {
            return Err(failure(
                "machine_upload_exists",
                "upload ID is already active",
            ));
        }
        if entries.len() >= MACHINE_UPLOAD_MAX_ACTIVE {
            return Err(failure(
                "machine_upload_busy",
                &format!("this machine is already staging {MACHINE_UPLOAD_MAX_ACTIVE} uploads"),
            ));
        }
        if let Some(size) = size {
            // Declared sizes of every active upload not yet written stay
            // counted, so idle uploads cannot let a new one fill the volume.
            let outstanding: u64 = entries
                .values()
                .map(|u| u.declared.unwrap_or(0).saturating_sub(u.written))
                .sum();
            let reserve = (size / 20).max(SPACE_RESERVE_MIN_BYTES);
            let available = root.available_bytes().map_err(io_failure)?;
            if available < size.saturating_add(reserve).saturating_add(outstanding) {
                return Err(failure(
                    "machine_upload_space_insufficient",
                    "the destination volume does not have enough free space for this upload",
                ));
            }
        }
        // Intent first, then the staging file, so a crash in between leaves a
        // recorded name for the startup sweep.
        let mut planned: Vec<(String, String)> =
            self.unswept.lock().map(|u| u.clone()).unwrap_or_default();
        planned.extend(
            entries
                .iter()
                .map(|(id, upload)| (upload.path.clone(), id.clone())),
        );
        planned.push((path.clone(), upload_id.to_owned()));
        let journal = root.transfer_journal().map_err(io_failure)?;
        write_journal(
            &journal,
            JOURNAL_INTENTS,
            ".treer-transfer-upl_journal",
            &planned,
        )?;
        let staged = match root.stage(&path, &format!(".treer-transfer-{upload_id}"), FINAL_MODE) {
            Ok(staged) => staged,
            Err(error) => {
                let _ = root.cleanup_staging(&path, &format!(".treer-transfer-{upload_id}"));
                let _ = self.persist_intents(&entries);
                return Err(io_failure(error));
            }
        };
        let now = Instant::now();
        entries.insert(
            upload_id.to_owned(),
            Upload {
                path,
                staged: Some(staged),
                overwrite,
                declared: size,
                written: 0,
                committed: 0,
                parts: Vec::new(),
                part_hash: Sha256::new(),
                file_hash: Sha256::new(),
                committed_hash: Sha256::new(),
                created: now,
                touched: now,
                #[cfg(test)]
                aged_idle: Duration::ZERO,
                #[cfg(test)]
                aged_lifetime: Duration::ZERO,
            },
        );
        Ok(())
    }

    /// Writes one chunk. Chunked uploads must give the offset: the
    /// written-through position appends, the committed position (below it)
    /// rewinds a failed part; anything else is refused. Legacy uploads append.
    pub fn chunk(
        &self,
        upload_id: &str,
        offset: Option<u64>,
        content_base64: &str,
    ) -> Result<u64, ProtocolError> {
        let content = base64::engine::general_purpose::STANDARD
            .decode(content_base64)
            .map_err(|_| failure("invalid_machine_upload", "chunk is not valid base64"))?;
        if content.len() > MACHINE_UPLOAD_CHUNK_BYTES {
            return Err(failure(
                "machine_upload_chunk_too_large",
                &format!("upload chunks may contain at most {MACHINE_UPLOAD_CHUNK_BYTES} bytes"),
            ));
        }
        let mut entries = self.lock()?;
        let upload = entries
            .get_mut(upload_id)
            .ok_or_else(|| failure("machine_upload_not_found", "upload is not active"))?;
        match (upload.chunked(), offset) {
            (false, None) => {}
            (true, Some(offset)) => {
                if offset == upload.committed && upload.committed < upload.written {
                    rewind(upload)?;
                } else if offset != upload.written {
                    return Err(failure(
                        "machine_upload_offset",
                        "chunk offset is neither the written-through nor the committed position",
                    ));
                }
            }
            _ => {
                return Err(failure(
                    "machine_upload_mode_mismatch",
                    "chunk offsets are required for declared-size uploads and refused for legacy uploads",
                ))
            }
        }
        let cap = upload.declared.unwrap_or(MACHINE_UPLOAD_MAX_BYTES);
        let total = upload.written.saturating_add(content.len() as u64);
        if total > cap {
            if upload.chunked() {
                let _ = rewind(upload);
            } else {
                entries.remove(upload_id);
                let _ = self.persist_intents(&entries);
            }
            return Err(failure(
                "machine_upload_too_large",
                &format!("upload exceeds its declared size or the {cap} byte limit"),
            ));
        }
        let written = {
            let file = upload
                .staged
                .as_mut()
                .expect("active upload keeps its staging file")
                .file_mut();
            #[cfg(test)]
            if take_injected_write_failure(upload_id) {
                let _ = file.write_all(&content[..content.len() / 2]);
                Err(std::io::Error::other("injected short write"))
            } else {
                file.write_all(&content)
            }
            #[cfg(not(test))]
            file.write_all(&content)
        };
        if let Err(error) = written {
            // A partial write (disk full, I/O error) must never stay in the
            // file: discard everything since the committed offset so the
            // retry starts clean.
            if upload.chunked() {
                let _ = rewind(upload);
            } else {
                entries.remove(upload_id);
                let _ = self.persist_intents(&entries);
            }
            return Err(io_failure(error));
        }
        upload.part_hash.update(&content);
        upload.file_hash.update(&content);
        upload.written = total;
        upload.touched = Instant::now();
        Ok(upload.written)
    }

    /// Closes a part: verifies the SHA-256 of the bytes since the committed
    /// offset, advances it and snapshots the whole-file hash. An identical,
    /// already committed part is idempotent; a conflicting one is refused.
    pub fn part_commit(
        &self,
        upload_id: &str,
        offset: u64,
        length: u64,
        sha256: &str,
    ) -> Result<Value, ProtocolError> {
        let sha256 = sha256.to_ascii_lowercase();
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(failure("invalid_machine_upload", "part SHA-256 is invalid"));
        }
        let mut entries = self.lock()?;
        let upload = entries
            .get_mut(upload_id)
            .ok_or_else(|| failure("machine_upload_not_found", "upload is not active"))?;
        if !upload.chunked() {
            return Err(failure(
                "machine_upload_mode_mismatch",
                "legacy uploads have no parts",
            ));
        }
        if offset.saturating_add(length) <= upload.committed {
            let known = upload
                .parts
                .iter()
                .any(|p| p.offset == offset && p.length == length && p.sha256 == sha256);
            return if known {
                Ok(json!({"committed_offset": upload.committed, "idempotent": true}))
            } else {
                Err(failure(
                    "upload_part_conflict",
                    "a different part is already committed at this offset",
                ))
            };
        }
        if offset != upload.committed || offset.saturating_add(length) != upload.written {
            return Err(failure(
                "machine_upload_offset",
                "part does not cover exactly the bytes written since the committed offset",
            ));
        }
        let on_disk = upload
            .staged
            .as_mut()
            .expect("active upload keeps its staging file")
            .file_mut()
            .metadata()
            .map_err(io_failure)?
            .len();
        if on_disk != upload.written {
            rewind(upload)?;
            return Err(failure(
                "machine_upload_file_mismatch",
                "the staging file does not hold exactly the bytes written; the part was discarded",
            ));
        }
        let actual = format!("{:x}", upload.part_hash.clone().finalize());
        if actual != sha256 {
            rewind(upload)?;
            return Err(failure(
                "upload_part_hash_mismatch",
                "the Controller's SHA-256 of the part differs from the declared one; the part was discarded",
            ));
        }
        upload.committed = upload.written;
        upload.committed_hash = upload.file_hash.clone();
        upload.part_hash = Sha256::new();
        upload.parts.push(PartRecord {
            offset,
            length,
            sha256,
        });
        upload.touched = Instant::now();
        Ok(json!({"committed_offset": upload.committed, "idempotent": false}))
    }

    /// Publishes the staged file. Chunked uploads must be complete; the
    /// receipt is remembered for the retention window and repeated commits
    /// return it.
    pub fn commit(&self, upload_id: &str) -> Result<UploadMachineFileResponse, ProtocolError> {
        if let Some(result) = self.stored_result(upload_id)? {
            return Ok(result);
        }
        let mut entries = self.lock()?;
        let mut upload = entries
            .remove(upload_id)
            .ok_or_else(|| failure("machine_upload_not_found", "upload is not active"))?;
        if let Some(declared) = upload.declared {
            if upload.committed != declared || upload.written != declared {
                entries.insert(upload_id.to_owned(), upload);
                return Err(failure(
                    "machine_upload_size_mismatch",
                    "the upload did not deliver exactly its declared size",
                ));
            }
        }
        // The upload is out of the registry while it is verified and
        // published, so the (up to 128 MiB) read-back never holds the lock
        // other uploads' chunks need; a concurrent chunk or abort for this id
        // sees "not active" meanwhile.
        drop(entries);
        let hash = if upload.chunked() {
            upload.committed_hash.clone()
        } else {
            upload.file_hash.clone()
        };
        // Fail closed before publishing: the staging file must hold exactly
        // the written bytes and hash to what was acknowledged. On a mismatch
        // the upload stays registered (staging kept) for abort or the reaper.
        if let Err(error) = verify_staged(&mut upload, &hash) {
            self.lock()?.insert(upload_id.to_owned(), upload);
            return Err(error);
        }
        let staged = upload
            .staged
            .take()
            .expect("active upload keeps its staging file");
        // Publish re-validates the parent and the target through the held
        // directory descriptor (platform boundary); a failure drops the
        // staging file.
        let outcome = self
            .open_root()
            .and_then(|root| root.validate_target(&upload.path).map_err(io_failure))
            .and_then(|()| staged.commit(upload.overwrite).map_err(io_failure));
        {
            let entries = self.lock()?;
            let _ = self.persist_intents(&entries);
        }
        // A failed unlink after a successful link means the file is published
        // and only the staging name lingers: committed, cleanup pending.
        let cleanup_pending = match outcome {
            Ok(()) => false,
            Err(error) if error.code == "machine_upload_commit_uncertain" => {
                tracing::warn!(
                    upload_id,
                    "upload published but its staging name could not be removed"
                );
                true
            }
            Err(error) => return Err(error),
        };
        let result = StoredResult {
            upload_id: upload_id.to_owned(),
            path: upload.path.clone(),
            bytes_written: upload.written,
            sha256: format!("{:x}", hash.finalize()),
            committed_at: now_epoch(),
            cleanup_pending,
        };
        if upload.chunked() {
            let mut results = self
                .results
                .lock()
                .map_err(|_| failure("state_error", "upload results lock poisoned"))?;
            let now = now_epoch();
            results.retain(|r| {
                now.saturating_sub(r.committed_at) < MACHINE_UPLOAD_RESULT_RETENTION_SECONDS
            });
            results.push(result.clone());
            let snapshot = results.clone();
            drop(results);
            if let Err(error) = self.persist_results(&snapshot) {
                tracing::warn!(?error, upload_id, "failed to persist the upload receipt");
            }
        }
        Ok(UploadMachineFileResponse {
            server_id: self.server_id.clone(),
            path: result.path,
            bytes_written: result.bytes_written,
            sha256: Some(result.sha256),
            cleanup_pending: result.cleanup_pending,
        })
    }

    fn stored_result(
        &self,
        upload_id: &str,
    ) -> Result<Option<UploadMachineFileResponse>, ProtocolError> {
        self.recover()?;
        let results = self
            .results
            .lock()
            .map_err(|_| failure("state_error", "upload results lock poisoned"))?;
        Ok(results
            .iter()
            .find(|r| r.upload_id == upload_id)
            .map(|r| UploadMachineFileResponse {
                server_id: self.server_id.clone(),
                path: r.path.clone(),
                bytes_written: r.bytes_written,
                sha256: Some(r.sha256.clone()),
                cleanup_pending: r.cleanup_pending,
            }))
    }

    /// The Controller's view for recovery: a committed receipt, the progress
    /// of an active upload, or unknown.
    pub fn result(&self, upload_id: &str) -> Result<Value, ProtocolError> {
        if let Some(result) = self.stored_result(upload_id)? {
            return Ok(json!({"state": "committed", "result": result}));
        }
        let entries = self.lock()?;
        match entries.get(upload_id) {
            Some(upload) => Ok(json!({
                "state": "active",
                "committed_offset": upload.committed,
                "written_offset": upload.written,
                "parts": upload.parts,
            })),
            None => Ok(json!({"state": "unknown"})),
        }
    }

    /// Idempotent: dropping the staged file deletes it.
    pub fn abort(&self, upload_id: &str) -> Result<bool, ProtocolError> {
        let mut entries = self.lock()?;
        let Some(upload) = entries.remove(upload_id) else {
            return Ok(false);
        };
        drop(upload);
        let _ = self.persist_intents(&entries);
        Ok(true)
    }

    /// Deletes staging files of uploads with no chunk for the idle window or
    /// older than the hard lifetime; returns how many were reaped.
    pub fn reap(&self) -> usize {
        let Ok(mut entries) = self.entries.try_lock() else {
            return 0;
        };
        let idle = Duration::from_secs(MACHINE_UPLOAD_IDLE_SECONDS);
        let lifetime = Duration::from_secs(MACHINE_UPLOAD_MAX_LIFETIME_SECONDS);
        let before = entries.len();
        entries.retain(|id, upload| {
            let keep = upload.idle_for() < idle && upload.lived_for() < lifetime;
            if !keep {
                tracing::warn!(upload_id = %id, "reaped an abandoned machine upload");
            }
            keep
        });
        let reaped = before - entries.len();
        if reaped > 0 {
            let _ = self.persist_intents(&entries);
        }
        reaped
    }

    /// Ages one upload for the reaper without moving any `Instant` backwards.
    #[cfg(test)]
    pub fn age_for_test(&self, upload_id: &str, idle: Duration, lifetime: Duration) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(upload) = entries.get_mut(upload_id) {
            upload.aged_idle = idle;
            upload.aged_lifetime = lifetime;
        }
    }

    /// Writes bytes into the staging file through the Controller's own handle
    /// (the only handle that may write it; Windows opens it with read-only
    /// sharing), then restores the write position. Models a corrupted or
    /// mislengthed staging file for the pre-publish verification tests.
    #[cfg(test)]
    pub fn write_staging_for_test(&self, upload_id: &str, offset: u64, bytes: &[u8]) {
        let mut entries = self.entries.lock().unwrap();
        let upload = entries.get_mut(upload_id).expect("active upload");
        let written = upload.written;
        let file = upload.staged.as_mut().expect("staging").file_mut();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(bytes).unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(written)).unwrap();
    }
}

/// The staging file must be exactly `written` bytes long and hash to the
/// acknowledged digest; read back through the held parent descriptor.
fn verify_staged(upload: &mut Upload, expected: &Sha256) -> Result<(), ProtocolError> {
    let staged = upload
        .staged
        .as_mut()
        .expect("active upload keeps its staging file");
    staged.file_mut().flush().map_err(io_failure)?;
    let length = staged.file_mut().metadata().map_err(io_failure)?.len();
    if length != upload.written {
        return Err(failure(
            "machine_upload_content_mismatch",
            "the staging file length differs from the bytes written; refusing to publish",
        ));
    }
    let mut reader = staged.reopen_read().map_err(io_failure)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer).map_err(io_failure)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    if total != upload.written || hasher.finalize() != expected.clone().finalize() {
        return Err(failure(
            "machine_upload_content_mismatch",
            "the staging file does not hash to the acknowledged content; refusing to publish",
        ));
    }
    Ok(())
}

/// Discards bytes written since the committed offset and restores the hash
/// state, so the next chunk at the committed offset starts the part again.
fn rewind(upload: &mut Upload) -> Result<(), ProtocolError> {
    let file = upload
        .staged
        .as_mut()
        .expect("active upload keeps its staging file")
        .file_mut();
    file.set_len(upload.committed).map_err(io_failure)?;
    file.seek(SeekFrom::Start(upload.committed))
        .map_err(io_failure)?;
    upload.written = upload.committed;
    upload.part_hash = Sha256::new();
    upload.file_hash = upload.committed_hash.clone();
    Ok(())
}

fn read_journal<T: serde::de::DeserializeOwned + Default>(
    journal: &Root,
    name: &str,
) -> Result<T, ProtocolError> {
    match journal.read(name) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(256 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(io_failure)?;
            if bytes.len() > 256 * 1024 {
                return Err(failure("state_error", "upload journal exceeds limit"));
            }
            serde_json::from_slice(&bytes)
                .map_err(|_| failure("state_error", "invalid upload journal"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(io_failure(error)),
    }
}

fn write_journal<T: Serialize>(
    journal: &Root,
    name: &str,
    temporary: &str,
    records: &T,
) -> Result<(), ProtocolError> {
    let bytes = serde_json::to_vec(records)
        .map_err(|_| failure("state_error", "cannot serialize upload journal"))?;
    let mut staged = journal.stage(name, temporary, 0o600).map_err(io_failure)?;
    staged.file_mut().write_all(&bytes).map_err(io_failure)?;
    staged.commit(true).map_err(io_failure)
}
