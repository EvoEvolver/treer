//! Windows workspace reads use the retained, reparse-refusing platform root.
//! Directory enumeration is explicitly unavailable until that backend exposes a
//! retained-directory iterator; path-based read_dir is not a safe substitute.
use axum::{
    body::Body,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::Stream;
use serde_json::{json, Value};
use std::{
    io,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};

const PREVIEW_LIMIT: usize = 64 * 1024;
const DOWNLOAD_LIMIT_BYTES: u64 = 1 << 30;

#[derive(Clone)]
pub struct Files {
    root: Arc<treer_platform::machine_file::Root>,
    slots: Arc<tokio::sync::Semaphore>,
    max: Duration,
    pub idle: Duration,
}
fn millis(name: &str, default: u64) -> Duration {
    Duration::from_millis(
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default)
            .max(100),
    )
}
fn response(status: StatusCode, value: Value) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        value.to_string(),
    )
        .into_response()
}
fn error(error: io::Error) -> Response {
    let status = match error.kind() {
        io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    response(
        status,
        json!({"error":{"code":"file_access_failed","message":"Unable to access workspace file"}}),
    )
}
impl Files {
    pub fn new(root: PathBuf) -> io::Result<Self> {
        Ok(Self {
            root: Arc::new(treer_platform::machine_file::Root::open(&root, &[])?),
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
            max: millis("AGENT_UI_DOWNLOAD_MAX_MS", 60 * 60_000),
            idle: millis("AGENT_UI_DOWNLOAD_IDLE_MS", 60_000),
        })
    }
    pub fn max_duration(&self) -> Duration {
        self.max
    }
    pub async fn handle(
        &self,
        method: &str,
        query: &str,
        headers: &HeaderMap,
        path: &str,
    ) -> Response {
        if method != "GET" {
            let mut r = response(
                StatusCode::METHOD_NOT_ALLOWED,
                json!({"error":{"code":"method_not_allowed","message":"File access is read-only; use GET"}}),
            );
            r.headers_mut()
                .insert(header::ALLOW, "GET".parse().unwrap());
            return r;
        }
        if path == "/v1/files/tree" {
            return response(
                StatusCode::NOT_IMPLEMENTED,
                json!({"error":{"code":"workspace_tree_unsupported","message":"Windows workspace directory enumeration is unavailable"}}),
            );
        }
        let download = path == "/v1/files/download";
        if download
            && headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site")
        {
            return response(
                StatusCode::FORBIDDEN,
                json!({"error":{"code":"cross_site_download","message":"Downloads cannot be started from another site"}}),
            );
        }
        let rel = query
            .split('&')
            .find_map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (key == "path").then(|| {
                    percent_encoding::percent_decode_str(&value.replace('+', " "))
                        .decode_utf8_lossy()
                        .into_owned()
                })
            })
            .unwrap_or_default();
        if rel.is_empty() {
            return response(
                StatusCode::BAD_REQUEST,
                json!({"error":{"code":"file_path_required","message":"A file path is required"}}),
            );
        }
        let permit = match self.slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return response(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({"error":{"code":"too_many_downloads","message":"Workspace filesystem workers are busy; try again"}}),
                )
            }
        };
        let root = self.root.clone();
        let opened = tokio::task::spawn_blocking(move || {
            // Keep the slot even if the HTTP request is cancelled during open.
            root.read(&rel).map(|file| (file, rel, permit))
        })
        .await;
        let (file, rel, permit) = match opened {
            Ok(Ok(opened)) => opened,
            Ok(Err(e)) => return error(e),
            Err(_) => return error(io::Error::other("filesystem worker failed")),
        };
        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(e) => return error(e),
        };
        let size = metadata.len();
        let name = rel.rsplit('/').next().unwrap_or_default();
        let mut file = tokio::fs::File::from_std(file);
        if !download {
            let mut bytes = Vec::with_capacity(PREVIEW_LIMIT + 1);
            if let Err(e) = (&mut file)
                .take(PREVIEW_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .await
            {
                return error(e);
            }
            let truncated = bytes.len() > PREVIEW_LIMIT;
            bytes.truncate(PREVIEW_LIMIT);
            return response(
                StatusCode::OK,
                json!({"path":rel,"name":name,"content":String::from_utf8_lossy(&bytes),"size":size,"truncated":truncated}),
            );
        }
        if size > DOWNLOAD_LIMIT_BYTES {
            return response(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({"error":{"code":"file_too_large","limit_bytes":DOWNLOAD_LIMIT_BYTES,"size":size}}),
            );
        }
        let body = DownloadBody {
            stream: tokio_util::io::ReaderStream::with_capacity(file.take(size), PREVIEW_LIMIT),
            deadline: Box::pin(tokio::time::sleep(self.max)),
            _permit: permit,
        };
        (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
                (header::CONTENT_LENGTH, size.to_string()),
                (header::CONTENT_DISPOSITION, content_disposition(name)),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
                (header::CACHE_CONTROL, "no-store".to_owned()),
            ],
            Body::from_stream(body),
        )
            .into_response()
    }
}
struct DownloadBody {
    stream: tokio_util::io::ReaderStream<tokio::io::Take<tokio::fs::File>>,
    deadline: Pin<Box<tokio::time::Sleep>>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl Stream for DownloadBody {
    type Item = io::Result<axum::body::Bytes>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if std::future::Future::poll(self.deadline.as_mut(), cx).is_ready() {
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "download duration exceeded",
            ))));
        }
        Pin::new(&mut self.stream).poll_next(cx)
    }
}
fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if (' '..='~').contains(&c) && !matches!(c, '"' | '\\' | '%' | ';') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded = percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// Bound writes without timing out a quiet connection that has no pending write.
pub struct WriteTimeout<T> {
    inner: T,
    idle: Duration,
    sleep: Pin<Box<tokio::time::Sleep>>,
    armed: bool,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl<T> WriteTimeout<T> {
    pub fn new(inner: T, idle: Duration) -> Self {
        Self {
            inner,
            idle,
            sleep: Box::pin(tokio::time::sleep(idle)),
            armed: false,
            _permit: None,
        }
    }
    pub fn with_permit(
        inner: T,
        idle: Duration,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Self {
        let mut result = Self::new(inner, idle);
        result._permit = Some(permit);
        result
    }
    fn stalled(&mut self, cx: &mut Context<'_>) -> bool {
        if !self.armed {
            self.sleep
                .as_mut()
                .reset(tokio::time::Instant::now() + self.idle);
            self.armed = true;
        }
        std::future::Future::poll(self.sleep.as_mut(), cx).is_ready()
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for WriteTimeout<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for WriteTimeout<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Pending if self.stalled(cx) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "write idle timeout",
            ))),
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
            Poll::Pending if self.stalled(cx) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "write idle timeout",
            ))),
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
            let path =
                std::env::temp_dir().join(format!("treer-windows-files-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
        fn files(&self) -> Files {
            Files::new(self.0.clone()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only the exact test-owned exclusively-created temporary directory.
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test]
    async fn windows_files_preview_and_boundaries() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("hello.txt"), "hello").unwrap();
        std::fs::create_dir(fixture.0.join(".treer")).unwrap();
        std::fs::write(fixture.0.join(".treer/secret"), "private").unwrap();
        let files = fixture.files();
        let good = files
            .handle("GET", "path=hello.txt", &HeaderMap::new(), "/v1/files")
            .await;
        assert_eq!(good.status(), StatusCode::OK);
        assert_eq!(good.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = axum::body::to_bytes(good.into_body(), 1024).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["content"], "hello");
        for path in [
            "../hello.txt",
            ".treer/secret",
            "C:/hello.txt",
            "hello.txt:alternate",
        ] {
            let response = files
                .handle(
                    "GET",
                    &format!("path={path}"),
                    &HeaderMap::new(),
                    "/v1/files",
                )
                .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        }
        let tree = files
            .handle("GET", "", &HeaderMap::new(), "/v1/files/tree")
            .await;
        assert_eq!(tree.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(
            files
                .handle("POST", "path=hello.txt", &HeaderMap::new(), "/v1/files")
                .await
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
    #[tokio::test]
    async fn windows_files_download_quota_and_hardlink_refusal() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("source"), "hello").unwrap();
        let files = fixture.files();
        let first = files
            .handle(
                "GET",
                "path=source",
                &HeaderMap::new(),
                "/v1/files/download",
            )
            .await;
        let second = files
            .handle(
                "GET",
                "path=source",
                &HeaderMap::new(),
                "/v1/files/download",
            )
            .await;
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(
            files
                .handle(
                    "GET",
                    "path=source",
                    &HeaderMap::new(),
                    "/v1/files/download"
                )
                .await
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        drop(first);
        drop(second);
        let mut headers = HeaderMap::new();
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert_eq!(
            files
                .handle("GET", "path=source", &headers, "/v1/files/download")
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        std::fs::hard_link(fixture.0.join("source"), fixture.0.join("alias")).unwrap();
        assert_eq!(
            files
                .handle("GET", "path=alias", &HeaderMap::new(), "/v1/files")
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}
