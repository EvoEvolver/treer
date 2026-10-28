//! Read-only workspace file routes (#158 M1/M2): GET /v1/files/tree, /v1/files (64 KiB preview) and /v1/files/download.
//! A port of launchers/agent/src/files.ts and download.ts with the same confinement and the #165 limits: a 1 GiB per-file limit
//! decided from the open descriptor, two concurrent downloads, a write-idle timeout (see `WriteTimeout`), a 60 minute cap per
//! download and a refusal of cross-site navigations. Slots are RAII guards, so every exit path (including a client that goes away
//! while the file is still being opened) releases exactly once.

use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::Stream;
use serde_json::{json, Value};
use std::{
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};

pub const PREVIEW_LIMIT: usize = 64 * 1024;
pub const DOWNLOAD_LIMIT_BYTES: u64 = 1 << 30;
const MAX_CONCURRENT: usize = 2;
const TREE_ENTRY_LIMIT: usize = 4096;

#[derive(Debug)]
pub struct FileError {
    status: StatusCode,
    message: String,
}
impl FileError {
    fn new(status: u16, message: &str) -> Self {
        Self { status: StatusCode::from_u16(status).unwrap(), message: message.into() }
    }
}
fn io_error(error: &io::Error) -> FileError {
    // The Node mapping: ENOENT/ENOTDIR 404, EACCES/EPERM/ELOOP 403, anything else 500.
    let status = match (error.kind(), error.raw_os_error()) {
        (io::ErrorKind::NotFound, _) | (_, Some(20)) => 404,
        (io::ErrorKind::PermissionDenied, _) | (_, Some(62)) | (_, Some(40)) => 403,
        _ => 500,
    };
    FileError::new(status, "")
}

struct Resolved {
    canonical: PathBuf,
    path: String,
}

async fn filesystem_slot(slots: Arc<tokio::sync::Semaphore>) -> Result<tokio::sync::OwnedSemaphorePermit, FileError> {
    tokio::time::timeout(Duration::from_secs(1), slots.acquire_owned()).await
        .ok().and_then(Result::ok)
        .ok_or_else(|| FileError::new(429, "Workspace filesystem workers are busy; try again"))
}

#[cfg(feature = "fixture-harness")]
fn fixture_worker_gate() {
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(path) = std::env::var("RUST_SPIKE_FILES_WORKER_GATE") else { return; };
    let directory = PathBuf::from(path);
    if !directory.is_dir() { return; }
    let _entry = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
        .open(directory.join(format!("started-{}", uuid::Uuid::new_v4())));
    let deadline = Instant::now() + Duration::from_secs(5);
    while directory.is_dir() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Resolve `rel` inside `root`: no `..`, no NUL, no absolute path or symlink leaving the canonical root.
async fn workspace_path(root: &Path, rel: &str, slots: Arc<tokio::sync::Semaphore>) -> Result<Resolved, FileError> {
    let slot = filesystem_slot(slots).await?;
    let (root, rel) = (root.to_path_buf(), rel.to_owned());
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        #[cfg(feature = "fixture-harness")]
        fixture_worker_gate();
        workspace_path_blocking(&root, &rel)
    }).await
        .map_err(|_| FileError::new(500, "Unable to resolve workspace path"))?
}
fn workspace_path_blocking(root: &Path, rel: &str) -> Result<Resolved, FileError> {
    if rel.contains('\0') || rel.split(['/', '\\']).any(|segment| segment == "..") {
        return Err(FileError::new(403, "Parent traversal is not allowed"));
    }
    // `root` was canonicalized and anchored once at startup. Never reinterpret
    // a replacement root symlink as a new workspace on a later request.
    let canonical_root = root.to_path_buf();
    let requested = Path::new(if rel.is_empty() { "." } else { rel });
    let mut candidate = if requested.is_absolute() { PathBuf::new() } else { canonical_root.clone() };
    for component in requested.components() {
        match component {
            std::path::Component::CurDir => {}
            other => candidate.push(other.as_os_str()),
        }
    }
    if !candidate.starts_with(&canonical_root) {
        return Err(FileError::new(403, "Path is outside the working directory"));
    }
    let canonical = std::fs::canonicalize(&candidate).map_err(|e| io_error(&e))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(FileError::new(403, "Symlink points outside the working directory"));
    }
    let path = candidate
        .strip_prefix(&canonical_root)
        .unwrap()
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok(Resolved { canonical, path })
}

async fn list_files(root: &Path, anchor: &std::fs::File, rel: &str, slot: Option<tokio::sync::OwnedSemaphorePermit>) -> Result<Value, FileError> {
    let (root, anchor, rel) = (root.to_path_buf(), anchor.try_clone().map_err(|e| io_error(&e))?, rel.to_owned());
    tokio::task::spawn_blocking(move || {
        let _slot = slot; // Cancellation must not release the slot while the worker still runs.
        #[cfg(feature = "fixture-harness")]
        fixture_worker_gate();
        list_files_blocking(&root, &anchor, &rel)
    }).await.map_err(|_| FileError::new(500, "Unable to list workspace directory"))?
}
fn list_files_blocking(root: &Path, anchor: &std::fs::File, rel: &str) -> Result<Value, FileError> {
    let directory = workspace_path_blocking(root, rel)?;
    let relative = directory.canonical.strip_prefix(root).map_err(|_| FileError::new(403, "Path is outside the working directory"))?;
    let reader = directory_stream(anchor, relative)?;
    let mut nodes: Vec<(bool, String, Value)> = Vec::new();
    let mut scanned = 0;
    for entry in reader {
        let entry = entry.map_err(|e| io_error(&e.into()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), "." | "..") { continue; }
        scanned += 1;
        if scanned > TREE_ENTRY_LIMIT {
            return Err(FileError::new(413, "Directory exceeds the 4096 entry listing limit"));
        }
        let child_path = if directory.path.is_empty() { name.clone() } else { format!("{}/{}", directory.path, name) };
        // Outside/broken symlinks are not navigable; other errors stay visible.
        let child = match workspace_path_blocking(root, &child_path) {
            Ok(child) => child,
            Err(error) if error.status == StatusCode::FORBIDDEN || error.status == StatusCode::NOT_FOUND => continue,
            Err(error) => return Err(error),
        };
        let relative = child.canonical.strip_prefix(root).map_err(|_| FileError::new(403, "Path is outside the working directory"))?;
        let metadata = match metadata_beneath(anchor, relative) {
            Ok(metadata) => metadata,
            Err(error) if matches!(io_error(&error).status.as_u16(), 403 | 404) => continue,
            Err(error) => return Err(io_error(&error)),
        };
        let kind = rustix::fs::FileType::from_raw_mode(metadata.st_mode);
        if !matches!(kind, rustix::fs::FileType::Directory | rustix::fs::FileType::RegularFile) {
            continue;
        }
        let is_dir = kind == rustix::fs::FileType::Directory;
        nodes.push((
            is_dir,
            name.clone(),
            json!({"name":name,"path":child_path,"kind":if is_dir {"directory"} else {"file"},"size":if is_dir {Value::Null} else {json!(metadata.st_size)},"has_children":is_dir}),
        ));
    }
    // kind first ("directory" < "file"), then the name case-insensitively like the Node localeCompare.
    nodes.sort_by(|a, b| (!a.0).cmp(&!b.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())).then_with(|| a.1.cmp(&b.1)));
    Ok(json!({"path":directory.path,"entries":nodes.into_iter().map(|n| n.2).collect::<Vec<_>>()}))
}

/// Walk from an already-owned directory capability. Every component is opened
/// without following symlinks, so an intermediate pathname swap cannot redirect
/// an admitted read outside that capability. Open directories stay pinned even
/// if subsequently renamed; this is not a same-account isolation boundary.
fn open_beneath(root: &std::fs::File, relative: &Path) -> io::Result<std::fs::File> {
    use rustix::fs::{openat, Mode, OFlags};
    let mut directory = root.try_clone()?;
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let std::path::Component::Normal(name) = component else {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "invalid workspace path"));
        };
        let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
        if components.peek().is_some() {
            flags |= OFlags::DIRECTORY;
        }
        directory = openat(&directory, name, flags, Mode::empty())?.into();
    }
    Ok(directory)
}

/// Metadata never opens the final object (in particular FIFO/device entries).
/// A final symlink replaced after admission is reported as a symlink and skipped.
fn metadata_beneath(root: &std::fs::File, relative: &Path) -> io::Result<rustix::fs::Stat> {
    // A permitted symlink to the workspace root resolves to an empty path.
    // Inspect the pinned capability itself without reopening its pathname.
    if relative.as_os_str().is_empty() {
        return Ok(rustix::fs::fstat(root)?);
    }
    let name = relative.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "invalid workspace path"))?;
    let parent = open_beneath(root, relative.parent().unwrap_or(Path::new("")))?;
    Ok(rustix::fs::statat(&parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?)
}

fn directory_stream(root: &std::fs::File, relative: &Path) -> Result<rustix::fs::Dir, FileError> {
    let handle = open_beneath(root, relative).map_err(|e| io_error(&e))?;
    if !handle.metadata().map_err(|e| io_error(&e))?.is_dir() {
        return Err(FileError::new(400, "Not a directory"));
    }
    // Independent streams prevent concurrent listings sharing iteration offsets.
    rustix::fs::Dir::read_from(&handle).map_err(|e| io_error(&e.into()))
}

async fn open_regular(root: &std::fs::File, root_path: &Path, canonical: &Path) -> Result<(tokio::fs::File, std::fs::Metadata), FileError> {
    let relative = canonical.strip_prefix(root_path).map_err(|_| FileError::new(403, "Path is outside the working directory"))?;
    let file = open_beneath(root, relative).map_err(|e| io_error(&e))?;
    let metadata = file.metadata().map_err(|e| io_error(&e))?;
    if !metadata.is_file() {
        return Err(FileError::new(400, "Not a regular file"));
    }
    Ok((tokio::fs::File::from_std(file), metadata))
}

async fn preview_file(root: &Path, anchor: &std::fs::File, rel: &str, slots: Arc<tokio::sync::Semaphore>) -> Result<Value, FileError> {
    let file = workspace_path(root, rel, slots).await?;
    let (mut handle, metadata) = open_regular(anchor, root, &file.canonical).await?;
    let mut bytes = Vec::with_capacity(PREVIEW_LIMIT + 1);
    (&mut handle).take(PREVIEW_LIMIT as u64 + 1).read_to_end(&mut bytes).await.map_err(|e| io_error(&e))?;
    let truncated = bytes.len() > PREVIEW_LIMIT;
    bytes.truncate(PREVIEW_LIMIT);
    let name = Path::new(&file.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(json!({"path":file.path,"name":name,"content":String::from_utf8_lossy(&bytes),"size":metadata.len(),"truncated":truncated}))
}

fn error_response(error: FileError, fallback_404: &str, fallback_500: &str) -> Response {
    let message = match error.status.as_u16() {
        404 => fallback_404.to_string(),
        500 => fallback_500.to_string(),
        _ => error.message,
    };
    json_response(error.status, json!({"error":{"code":"file_access_failed","message":message}}))
}
fn json_response(status: StatusCode, value: Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], value.to_string()).into_response()
}

/// RFC 6266 attachment header: an ASCII fallback plus a UTF-8 filename* (RFC 5987 percent-encoding).
pub fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| if (' '..='~').contains(&c) && !matches!(c, '"' | '\\' | '%' | ';') { c } else { '_' })
        .collect();
    let fallback = if fallback.is_empty() { "download".to_string() } else { fallback };
    let mut encoded = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// A download slot; released exactly once when dropped (response finished, aborted, or the request future dropped).
struct Slot(Arc<AtomicUsize>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
/// The body: a bounded reader that holds its slot, and ends with an error after the overall cap.
struct DownloadBody<R> {
    inner: tokio_util::io::ReaderStream<tokio::io::Take<R>>,
    started: Instant,
    max: Duration,
    _slot: Slot,
}
impl<R: AsyncRead + Unpin> Stream for DownloadBody<R> {
    type Item = io::Result<Bytes>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.started.elapsed() > self.max {
            return Poll::Ready(Some(Err(io::Error::new(io::ErrorKind::TimedOut, "download duration cap"))));
        }
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

#[derive(Clone)]
pub struct Files {
    root: PathBuf,
    anchor: Arc<std::fs::File>,
    active: Arc<AtomicUsize>,
    browsing: Arc<tokio::sync::Semaphore>,
    max: Duration,
    pub idle: Duration,
}
fn millis(name: &str, default: u64) -> Duration {
    Duration::from_millis(std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0).unwrap_or(default).max(100))
}
impl Files {
    pub fn max_duration(&self) -> Duration { self.max }
    pub fn new(root: PathBuf) -> io::Result<Self> {
        // main passes the configured canonical cwd. Do not canonicalize again:
        // a swapped root symlink must fail, not retarget the configuration.
        // Canonical paths have no symlinks at resolution time. Walking from /
        // also refuses a newly swapped symlink in a root ancestor at open time.
        let anchor = open_beneath(&std::fs::File::open("/")?, root.strip_prefix("/").map_err(|_| io::Error::other("invalid workspace root"))?)?;
        if !anchor.metadata()?.is_dir() {
            return Err(io::Error::other("workspace root is not a directory"));
        }
        Ok(Self {
            root,
            anchor: Arc::new(anchor),
            active: Arc::new(AtomicUsize::new(0)),
            browsing: Arc::new(tokio::sync::Semaphore::new(2)),
            max: millis("AGENT_UI_DOWNLOAD_MAX_MS", 60 * 60_000),
            idle: millis("AGENT_UI_DOWNLOAD_IDLE_MS", 60_000),
        })
    }

    pub async fn handle(&self, method: &str, query: &str, headers: &HeaderMap, path: &str) -> Response {
        let file_path = query_value(query, "path").unwrap_or_default();
        if path == "/v1/files/download" {
            return self.download(method, headers, &file_path).await;
        }
        if method != "GET" {
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                [(header::ALLOW, "GET"), (header::CONTENT_TYPE, "application/json; charset=utf-8"), (header::CACHE_CONTROL, "no-store")],
                json!({"error":{"code":"method_not_allowed","message":"File browsing is read-only; use GET"}}).to_string(),
            )
                .into_response();
        }
        let result = if path == "/v1/files/tree" {
            match filesystem_slot(self.browsing.clone()).await {
                Ok(slot) => list_files(&self.root, &self.anchor, &file_path, Some(slot)).await,
                Err(error) => Err(error),
            }
        } else { preview_file(&self.root, &self.anchor, &file_path, self.browsing.clone()).await };
        match result {
            Ok(value) => json_response(StatusCode::OK, value),
            Err(error) => error_response(error, "File or directory not found", "Unable to read file or directory"),
        }
    }

    async fn download(&self, method: &str, headers: &HeaderMap, rel: &str) -> Response {
        if method != "GET" {
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                [(header::ALLOW, "GET"), (header::CONTENT_TYPE, "application/json; charset=utf-8"), (header::CACHE_CONTROL, "no-store")],
                json!({"error":{"code":"method_not_allowed","message":"File download is read-only; use GET"}}).to_string(),
            )
                .into_response();
        }
        // A page on another site must not start a download in the user's browser; same-origin, same-site and typed URLs pass.
        if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site") {
            return json_response(StatusCode::FORBIDDEN, json!({"error":{"code":"cross_site_download","message":"Downloads cannot be started from another site"}}));
        }
        if self.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONCURRENT {
            self.active.fetch_sub(1, Ordering::SeqCst);
            let mut response = json_response(StatusCode::TOO_MANY_REQUESTS, json!({"error":{"code":"too_many_downloads","message":"Too many downloads are running; try again in a moment"}}));
            response.headers_mut().insert(header::RETRY_AFTER, "2".parse().unwrap());
            return response;
        }
        // From here the slot is held by this guard: if the request future is dropped while the open is pending, it is released.
        let slot = Slot(self.active.clone());
        let resolved = match workspace_path(&self.root, rel, self.browsing.clone()).await {
            Ok(resolved) if !rel.is_empty() => resolved,
            Ok(_) => return error_response(FileError::new(400, "A file path is required"), "", ""),
            Err(error) => return error_response(error, "File not found", "Unable to read file"),
        };
        let (file, metadata) = match open_regular(&self.anchor, &self.root, &resolved.canonical).await {
            Ok(opened) => opened,
            Err(error) => return error_response(error, "File not found", "Unable to read file"),
        };
        let size = metadata.len();
        // Decided from the open descriptor before any byte is sent.
        if size > DOWNLOAD_LIMIT_BYTES {
            return json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({"error":{"code":"file_too_large","message":format!("This file is larger than the {} GiB download limit", DOWNLOAD_LIMIT_BYTES >> 30),"limit_bytes":DOWNLOAD_LIMIT_BYTES,"size":size}}),
            );
        }
        let name = Path::new(&resolved.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let body = DownloadBody { inner: tokio_util::io::ReaderStream::with_capacity(file.take(size), 64 * 1024), started: Instant::now(), max: self.max, _slot: slot };
        (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (header::CONTENT_LENGTH, size.to_string()),
                (header::CONTENT_DISPOSITION, content_disposition(&name)),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ],
            Body::from_stream(body),
        )
            .into_response()
    }
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then(|| percent_encoding::percent_decode_str(&v.replace('+', " ")).decode_utf8_lossy().into_owned())
    })
}

/// Connection wrapper: a write that stays Pending longer than `idle` (a client that stopped reading) fails the connection, which
/// destroys any download response on it. Reads and a quiet connection with nothing to write are not affected.
pub struct WriteTimeout<T> {
    inner: T,
    idle: Duration,
    sleep: Pin<Box<tokio::time::Sleep>>,
    armed: bool,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl<T> WriteTimeout<T> {
    pub fn new(inner: T, idle: Duration) -> Self {
        Self { inner, idle, sleep: Box::pin(tokio::time::sleep(idle)), armed: false, _permit: None }
    }
    pub fn with_permit(inner: T, idle: Duration, permit: tokio::sync::OwnedSemaphorePermit) -> Self {
        let mut io = Self::new(inner, idle);
        io._permit = Some(permit);
        io
    }
    fn stalled(&mut self, cx: &mut Context<'_>) -> bool {
        if !self.armed {
            self.sleep.as_mut().reset(tokio::time::Instant::now() + self.idle);
            self.armed = true;
        }
        std::future::Future::poll(self.sleep.as_mut(), cx).is_ready()
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for WriteTimeout<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for WriteTimeout<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Pending if self.stalled(cx) => Poll::Ready(Err(io::Error::new(io::ErrorKind::TimedOut, "write idle timeout"))),
            other => {
                if other.is_ready() {
                    self.armed = false;
                }
                other
            }
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Pending if self.stalled(cx) => Poll::Ready(Err(io::Error::new(io::ErrorKind::TimedOut, "write idle timeout"))),
            other => {
                if other.is_ready() {
                    self.armed = false;
                }
                other
            }
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod confinement_tests {
    use super::*;
    use std::os::unix::fs::{symlink, DirBuilderExt};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("rust-files-{}", uuid::Uuid::new_v4()));
            std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only this test's exact exclusively-created fixture root.
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[tokio::test]
    async fn intermediate_and_final_symlink_swaps_cannot_redirect_admitted_reads() {
        let fixture = Fixture::new();
        let root = fixture.0.join("workspace");
        let outside = fixture.0.join("outside");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("nested/data"), "accepted").unwrap();
        std::fs::write(outside.join("data"), "outside-sentinel").unwrap();
        let files = Files::new(root.clone()).unwrap();
        // Pause deterministically after canonical admission, before opening.
        let admitted = workspace_path(&files.root, "nested/data", files.browsing.clone()).await.unwrap();
        std::fs::rename(root.join("nested"), root.join("original")).unwrap();
        symlink(&outside, root.join("nested")).unwrap();
        assert!(open_regular(&files.anchor, &files.root, &admitted.canonical).await.is_err());
        assert!(metadata_beneath(&files.anchor, Path::new("nested/data")).is_err());
        assert!(directory_stream(&files.anchor, Path::new("nested")).is_err());
        std::fs::remove_file(root.join("nested")).unwrap();
        std::fs::rename(root.join("original"), root.join("nested")).unwrap();
        std::fs::remove_file(root.join("nested/data")).unwrap();
        symlink(outside.join("data"), root.join("nested/data")).unwrap();
        assert!(open_regular(&files.anchor, &files.root, &admitted.canonical).await.is_err());
        let metadata = metadata_beneath(&files.anchor, Path::new("nested/data")).unwrap();
        assert_eq!(rustix::fs::FileType::from_raw_mode(metadata.st_mode), rustix::fs::FileType::Symlink);
        assert_eq!(std::fs::read_to_string(outside.join("data")).unwrap(), "outside-sentinel");
    }

    #[tokio::test]
    async fn directory_stream_stays_pinned_when_its_path_is_replaced() {
        let fixture = Fixture::new();
        let root = fixture.0.join("workspace");
        let outside = fixture.0.join("outside");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("nested/inside-name"), "accepted").unwrap();
        std::fs::write(outside.join("outside-name"), "outside-sentinel").unwrap();
        let files = Files::new(root.clone()).unwrap();
        let reader = directory_stream(&files.anchor, Path::new("nested")).unwrap();
        std::fs::rename(root.join("nested"), root.join("original")).unwrap();
        symlink(&outside, root.join("nested")).unwrap();
        let names: Vec<_> = reader.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().any(|name| name == "inside-name"));
        assert!(!names.iter().any(|name| name == "outside-name"));
        assert_eq!(std::fs::read_to_string(outside.join("outside-name")).unwrap(), "outside-sentinel");
    }

    #[tokio::test]
    async fn root_target_symlinks_are_listed_and_navigable() {
        let fixture = Fixture::new();
        let root = fixture.0.join("workspace");
        let outside = fixture.0.join("outside");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("proof"), "accepted").unwrap();
        symlink(".", root.join("root-link")).unwrap();
        symlink("..", root.join("nested/back-to-root")).unwrap();
        symlink(&outside, root.join("outside-link")).unwrap();
        let files = Files::new(root).unwrap();
        for (parent, name, link) in [("", "root-link", "root-link"), ("nested", "back-to-root", "nested/back-to-root")] {
            let listing = list_files(&files.root, &files.anchor, parent, None).await.unwrap();
            let entry = listing["entries"].as_array().unwrap().iter().find(|entry| entry["name"] == name).expect("root-target link must be visible");
            assert_eq!(entry["kind"], "directory");
            assert_eq!(entry["path"], link);
            let linked = list_files(&files.root, &files.anchor, link, None).await.unwrap();
            assert_eq!(linked["path"], link);
            assert!(linked["entries"].as_array().unwrap().iter().any(|entry| entry["name"] == "proof"));
            assert!(!linked["entries"].as_array().unwrap().iter().any(|entry| entry["name"] == "outside-link"));
        }
        assert!(list_files(&files.root, &files.anchor, "outside-link", None).await.is_err());
        assert!(metadata_beneath(&files.anchor, Path::new("../outside")).is_err());
        assert!(metadata_beneath(&files.anchor, &outside).is_err());
    }

    #[tokio::test]
    async fn concurrent_tree_reads_have_independent_directory_offsets() {
        let fixture = Fixture::new();
        for n in 0..16 { std::fs::write(fixture.0.join(format!("entry-{n}")), "accepted").unwrap(); }
        let files = Files::new(fixture.0.clone()).unwrap();
        let (first, second) = tokio::join!(list_files(&files.root, &files.anchor, "", None), list_files(&files.root, &files.anchor, "", None));
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first["entries"].as_array().unwrap().len(), 16);
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn root_replacement_does_not_retarget_the_pinned_workspace() {
        let fixture = Fixture::new();
        let root = fixture.0.join("workspace");
        let outside = fixture.0.join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("data"), "accepted").unwrap();
        std::fs::write(outside.join("data"), "outside-sentinel").unwrap();
        let files = Files::new(root.clone()).unwrap();
        let admitted = workspace_path(&files.root, "data", files.browsing.clone()).await.unwrap();
        std::fs::rename(&root, fixture.0.join("original")).unwrap();
        symlink(&outside, &root).unwrap();
        // A previously admitted pathname still uses the original capability.
        let (mut file, _) = open_regular(&files.anchor, &files.root, &admitted.canonical).await.unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        assert_eq!(content, "accepted");
        // New resolutions may not redefine the workspace through the symlink.
        assert!(workspace_path(&files.root, "data", files.browsing.clone()).await.is_err());
        assert!(Files::new(root.clone()).is_err(), "startup must not recanonicalize a replaced canonical root");
        assert!(open_beneath(&files.anchor, Path::new("../outside/data")).is_err());
        assert!(open_beneath(&files.anchor, &outside.join("data")).is_err());
        assert_eq!(std::fs::read_to_string(outside.join("data")).unwrap(), "outside-sentinel");
    }
}
