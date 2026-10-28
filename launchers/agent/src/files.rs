//! Read-only workspace file routes (#158 M1/M2): GET /v1/files/tree, /v1/files (64 KiB preview) and /v1/files/download.
//! A port of the former Node launcher's files.ts and download.ts with the same confinement and the #165 limits: a 1 GiB per-file limit
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
    let Ok(path) = std::env::var("RUST_SPIKE_FILES_WORKER_GATE") else { return; };
    let directory = PathBuf::from(path);
    if !directory.is_dir() { return; }
    let _entry = crate::fs_util::create_new(&directory.join(format!("started-{}", uuid::Uuid::new_v4())));
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

async fn list_files(root: &Path, rel: &str, slot: Option<tokio::sync::OwnedSemaphorePermit>) -> Result<Value, FileError> {
    let (root, rel) = (root.to_path_buf(), rel.to_owned());
    tokio::task::spawn_blocking(move || {
        let _slot = slot; // Cancellation must not release the slot while the worker still runs.
        #[cfg(feature = "fixture-harness")]
        fixture_worker_gate();
        list_files_blocking(&root, &rel)
    }).await.map_err(|_| FileError::new(500, "Unable to list workspace directory"))?
}
fn list_files_blocking(root: &Path, rel: &str) -> Result<Value, FileError> {
    let directory = workspace_path_blocking(root, rel)?;
    if !std::fs::metadata(&directory.canonical).map_err(|e| io_error(&e))?.is_dir() {
        return Err(FileError::new(400, "Not a directory"));
    }
    let reader = std::fs::read_dir(&directory.canonical).map_err(|e| io_error(&e))?;
    let mut nodes: Vec<(bool, String, Value)> = Vec::new();
    let mut scanned = 0;
    for entry in reader {
        let entry = entry.map_err(|e| io_error(&e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
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
        let metadata = match std::fs::metadata(&child.canonical) {
            Ok(metadata) => metadata,
            Err(error) if matches!(io_error(&error).status.as_u16(), 403 | 404) => continue,
            Err(error) => return Err(io_error(&error)),
        };
        // Only directories and regular files (never FIFOs, sockets or devices).
        if !metadata.is_dir() && !metadata.is_file() {
            continue;
        }
        let is_dir = metadata.is_dir();
        nodes.push((
            is_dir,
            name.clone(),
            json!({"name":name,"path":child_path,"kind":if is_dir {"directory"} else {"file"},"size":if is_dir {Value::Null} else {json!(metadata.len())},"has_children":is_dir}),
        ));
    }
    // kind first ("directory" < "file"), then the name case-insensitively like the Node localeCompare.
    nodes.sort_by(|a, b| (!a.0).cmp(&!b.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())).then_with(|| a.1.cmp(&b.1)));
    Ok(json!({"path":directory.path,"entries":nodes.into_iter().map(|n| n.2).collect::<Vec<_>>()}))
}

/// Open an admitted (canonical, inside the root) regular file. The type is
/// checked before opening so a FIFO or device is never opened.
async fn open_regular(canonical: &Path) -> Result<(tokio::fs::File, std::fs::Metadata), FileError> {
    if !std::fs::metadata(canonical).map_err(|e| io_error(&e))?.is_file() {
        return Err(FileError::new(400, "Not a regular file"));
    }
    let file = crate::fs_util::open_read(canonical).map_err(|e| io_error(&e))?;
    let metadata = file.metadata().map_err(|e| io_error(&e))?;
    if !metadata.is_file() {
        return Err(FileError::new(400, "Not a regular file"));
    }
    Ok((tokio::fs::File::from_std(file), metadata))
}

async fn preview_file(root: &Path, rel: &str, slots: Arc<tokio::sync::Semaphore>) -> Result<Value, FileError> {
    let file = workspace_path(root, rel, slots).await?;
    let (mut handle, metadata) = open_regular(&file.canonical).await?;
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
        // main passes the configured canonical cwd; requests are confined to it.
        if !std::fs::metadata(&root)?.is_dir() {
            return Err(io::Error::other("workspace root is not a directory"));
        }
        Ok(Self {
            root,
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
                Ok(slot) => list_files(&self.root, &file_path, Some(slot)).await,
                Err(error) => Err(error),
            }
        } else { preview_file(&self.root, &file_path, self.browsing.clone()).await };
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
        let (file, metadata) = match open_regular(&resolved.canonical).await {
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
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("rust-files-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only this test's exact exclusively-created fixture root.
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    async fn json(response: Response) -> Value {
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn tree_preview_and_traversal_boundaries() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.0.join("nested")).unwrap();
        std::fs::write(fixture.0.join("hello.txt"), "hello").unwrap();
        std::fs::write(fixture.0.join("nested/inner.txt"), "inner").unwrap();
        let files = Files::new(fixture.0.clone()).unwrap();
        let tree = files.handle("GET", "", &HeaderMap::new(), "/v1/files/tree").await;
        assert_eq!(tree.status(), StatusCode::OK);
        let tree = json(tree).await;
        let names: Vec<_> = tree["entries"].as_array().unwrap().iter().map(|e| (e["name"].as_str().unwrap().to_owned(), e["kind"].as_str().unwrap().to_owned())).collect();
        assert_eq!(names, [("nested".to_owned(), "directory".to_owned()), ("hello.txt".to_owned(), "file".to_owned())]);
        let nested = json(files.handle("GET", "path=nested", &HeaderMap::new(), "/v1/files/tree").await).await;
        assert_eq!(nested["entries"][0]["path"], "nested/inner.txt");
        let preview = files.handle("GET", "path=nested/inner.txt", &HeaderMap::new(), "/v1/files").await;
        assert_eq!(preview.status(), StatusCode::OK);
        assert_eq!(preview.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(json(preview).await["content"], "inner");
        let outside = if cfg!(windows) { "C:/hello.txt" } else { "/etc/hosts" };
        for path in ["../hello.txt", "nested/../../hello.txt", outside] {
            let response = files.handle("GET", &format!("path={path}"), &HeaderMap::new(), "/v1/files").await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        }
        assert_eq!(files.handle("GET", "path=missing", &HeaderMap::new(), "/v1/files").await.status(), StatusCode::NOT_FOUND);
        assert_eq!(files.handle("GET", "path=nested", &HeaderMap::new(), "/v1/files").await.status(), StatusCode::BAD_REQUEST);
        assert_eq!(files.handle("GET", "path=hello.txt", &HeaderMap::new(), "/v1/files/tree").await.status(), StatusCode::BAD_REQUEST);
        assert_eq!(files.handle("POST", "path=hello.txt", &HeaderMap::new(), "/v1/files").await.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn downloads_are_bounded_and_refuse_cross_site() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("source"), "hello").unwrap();
        let files = Files::new(fixture.0.clone()).unwrap();
        let none = HeaderMap::new();
        let download = || files.handle("GET", "path=source", &none, "/v1/files/download");
        let first = download().await;
        let second = download().await;
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(download().await.status(), StatusCode::TOO_MANY_REQUESTS);
        drop(first);
        drop(second);
        let body = axum::body::to_bytes(download().await.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"hello");
        let mut headers = HeaderMap::new();
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert_eq!(files.handle("GET", "path=source", &headers, "/v1/files/download").await.status(), StatusCode::FORBIDDEN);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn root_target_symlinks_are_listed_and_outside_symlinks_are_not() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let root = fixture.0.join("workspace");
        let outside = fixture.0.join("outside");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("proof"), "accepted").unwrap();
        std::fs::write(outside.join("data"), "outside-sentinel").unwrap();
        symlink(".", root.join("root-link")).unwrap();
        symlink("..", root.join("nested/back-to-root")).unwrap();
        symlink(&outside, root.join("outside-link")).unwrap();
        let files = Files::new(root).unwrap();
        for (parent, name, link) in [("", "root-link", "root-link"), ("nested", "back-to-root", "nested/back-to-root")] {
            let listing = list_files(&files.root, parent, None).await.unwrap();
            let entry = listing["entries"].as_array().unwrap().iter().find(|entry| entry["name"] == name).expect("root-target link must be visible");
            assert_eq!(entry["kind"], "directory");
            assert_eq!(entry["path"], link);
            let linked = list_files(&files.root, link, None).await.unwrap();
            assert_eq!(linked["path"], link);
            assert!(linked["entries"].as_array().unwrap().iter().any(|entry| entry["name"] == "proof"));
            assert!(!linked["entries"].as_array().unwrap().iter().any(|entry| entry["name"] == "outside-link"));
        }
        assert!(list_files(&files.root, "outside-link", None).await.is_err());
        assert!(workspace_path_blocking(&files.root, "outside-link/data").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fifo_entries_are_skipped_and_never_opened() {
        let fixture = Fixture::new();
        assert!(std::process::Command::new("mkfifo").arg(fixture.0.join("pipe")).status().unwrap().success());
        std::fs::write(fixture.0.join("file"), "x").unwrap();
        let files = Files::new(fixture.0.clone()).unwrap();
        let listing = list_files(&files.root, "", None).await.unwrap();
        assert_eq!(listing["entries"].as_array().unwrap().len(), 1);
        let preview = tokio::time::timeout(Duration::from_secs(2), files.handle("GET", "path=pipe", &HeaderMap::new(), "/v1/files")).await.unwrap();
        assert_eq!(preview.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn concurrent_tree_reads_are_independent() {
        let fixture = Fixture::new();
        for n in 0..16 { std::fs::write(fixture.0.join(format!("entry-{n}")), "accepted").unwrap(); }
        let files = Files::new(fixture.0.clone()).unwrap();
        let (first, second) = tokio::join!(list_files(&files.root, "", None), list_files(&files.root, "", None));
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first["entries"].as_array().unwrap().len(), 16);
        assert_eq!(first, second);
    }
}
