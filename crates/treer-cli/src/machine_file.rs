//! The Controller parses commands; this module only moves local bytes using
//! the negotiated, bounded transfer transport. No remote command execution.
use anyhow::{ensure, Context};
use base64::Engine;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use treer_protocol::{
    MachineFileTransferCall, MachineFileTransferRequest as Request, MachineTransferBegin,
    MachineTransferChunk, MachineTransferComplete, CAPABILITY_MACHINE_FILE_TRANSFER,
    MACHINE_TRANSFER_CHUNK_BYTES, MACHINE_TRANSFER_HARD_LIMIT,
};

pub(super) struct Options {
    pub path: String,
    pub local: String,
    pub remote: String,
    pub download: bool,
    pub overwrite: bool,
    pub mode: u32,
}

struct Transport {
    client: reqwest::Client,
    url: url::Url,
    headers: reqwest::header::HeaderMap,
    remote: String,
}
impl Transport {
    async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> anyhow::Result<T> {
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND
            || status == reqwest::StatusCode::NOT_IMPLEMENTED
        {
            anyhow::bail!("file_transfer_unsupported: upgrade the Proxy and target Controller");
        }
        // Bound successful and error responses before JSON parsing. Never print
        // the response body: a remote error must not disclose file contents.
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= 96 * 1024,
                "file transfer response exceeds transport limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let code = value["error"]["code"]
                .as_str()
                .or_else(|| value["code"].as_str())
                .unwrap_or("machine_file_request_failed");
            // Only bounded protocol identifiers, never arbitrary server text.
            let code = if code.len() <= 80
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            {
                code
            } else {
                "machine_file_request_failed"
            };
            anyhow::bail!("{code}: file transfer request rejected (HTTP {status})");
        }
        serde_json::from_slice(&bytes).context("invalid file transfer response")
    }
    async fn call<T: DeserializeOwned>(&self, request: Request) -> anyhow::Result<T> {
        Self::decode(
            self.client
                .post(self.url.clone())
                .headers(self.headers.clone())
                .json(&MachineFileTransferCall {
                    path: self.remote.clone(),
                    request,
                })
                .send()
                .await?,
        )
        .await
    }
    async fn abort(&self, id: &str, read: bool) {
        let _: anyhow::Result<serde_json::Value> = self
            .call(Request::Abort {
                id: id.into(),
                read,
            })
            .await;
    }
}

pub(super) async fn transfer(
    base: &url::Url,
    headers: &[(&'static str, String)],
    options: Options,
) -> anyhow::Result<i32> {
    treer_platform::machine_file::validate_relative_path(&options.remote)?;
    treer_platform::machine_file::validate_mode(options.mode)?;
    let mut map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        map.insert(*name, reqwest::header::HeaderValue::from_str(value)?);
    }
    let transport = Transport {
        client: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?,
        url: super::transport_url(base, &options.path)?,
        headers: map,
        remote: options.remote.clone(),
    };
    let info: serde_json::Value = Transport::decode(
        transport
            .client
            .get(transport.url.clone())
            .headers(transport.headers.clone())
            .send()
            .await?,
    )
    .await?;
    ensure!(info["capabilities"].as_array().is_some_and(|caps| caps.iter().any(|cap| cap == CAPABILITY_MACHINE_FILE_TRANSFER)),
        "file_transfer_unsupported: target Controller lacks the negotiated file transfer capability");
    let limit = info["max_bytes"]
        .as_u64()
        .filter(|limit| *limit > 0 && *limit <= MACHINE_TRANSFER_HARD_LIMIT)
        .context("invalid transfer size limit")?;
    tokio::time::timeout(
        Duration::from_secs(treer_protocol::MACHINE_TRANSFER_TOTAL_SECONDS),
        async {
            if options.download {
                download(&transport, &options, limit).await
            } else {
                upload(&transport, &options, limit).await
            }
        },
    )
    .await
    .context("file transfer exceeded total deadline; temporary state will expire")??;
    Ok(0)
}

async fn upload(transport: &Transport, options: &Options, limit: u64) -> anyhow::Result<()> {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (transport, options, limit);
        anyhow::bail!(
            "file_transfer_unsupported: safe local file operations unavailable on this platform"
        );
    }
    #[cfg(any(unix, windows))]
    {
        let (mut input, size): (Box<dyn AsyncRead + Unpin + Send>, Option<u64>) =
            if options.local == "-" {
                (Box::new(tokio::io::stdin()), None)
            } else {
                let (root, leaf) = local_root(&options.local)?;
                let file = root
                    .read(&leaf)
                    .context("cannot open local regular file without following links")?;
                let size = file.metadata()?.len();
                ensure!(
                    size <= limit,
                    "machine_file_too_large: local file exceeds machine transfer limit"
                );
                (Box::new(tokio::fs::File::from_std(file)), Some(size))
            };
        let begin: MachineTransferBegin = transport
            .call(Request::PutBegin {
                path: options.remote.clone(),
                overwrite: options.overwrite,
                mode: options.mode,
                size,
                limit,
            })
            .await?;
        validate_begin(&begin, &options.remote, limit)?;
        let result = async {
            let mut offset = 0_u64;
            let mut hash = Sha256::new();
            let mut buffer = vec![0_u8; MACHINE_TRANSFER_CHUNK_BYTES];
            loop {
                let count = input.read(&mut buffer).await?;
                if count == 0 {
                    break;
                }
                ensure!(
                    offset + count as u64 <= begin.limit,
                    "machine_file_too_large: input exceeds machine transfer limit"
                );
                let response: serde_json::Value = transport
                    .call(Request::PutChunk {
                        id: begin.id.clone(),
                        offset,
                        content_base64: base64::engine::general_purpose::STANDARD
                            .encode(&buffer[..count]),
                    })
                    .await?;
                ensure!(
                    response["offset"].as_u64() == Some(offset + count as u64),
                    "invalid upload acknowledgement"
                );
                hash.update(&buffer[..count]);
                offset += count as u64;
            }
            let digest = format!("{:x}", hash.finalize());
            let complete: MachineTransferComplete = transport
                .call(Request::PutFinish {
                    id: begin.id.clone(),
                    size: offset,
                    sha256: digest.clone(),
                })
                .await?;
            ensure!(
                complete.path == options.remote
                    && complete.size == offset
                    && complete.sha256 == digest,
                "machine_file_checksum_mismatch: upload completion digest differs"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if result.is_err() {
            transport.abort(&begin.id, false).await;
        }
        result
    }
}

fn validate_begin(begin: &MachineTransferBegin, remote: &str, limit: u64) -> anyhow::Result<()> {
    ensure!(
        !begin.id.is_empty()
            && begin.id.len() <= 128
            && begin.path == remote
            && begin.limit > 0
            && begin.limit <= limit
            && begin.chunk_bytes == MACHINE_TRANSFER_CHUNK_BYTES
            && begin.size.is_none_or(|size| size <= begin.limit),
        "invalid transfer negotiation"
    );
    Ok(())
}

#[cfg(any(unix, windows))]
fn local_root(path: &str) -> anyhow::Result<(treer_platform::machine_file::Root, String)> {
    // Resolve lexical components only. Each existing directory is subsequently
    // opened relative to retained handles with NOFOLLOW, including cwd parents.
    let absolute = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut components = Vec::new();
    #[cfg(windows)]
    let mut anchor = PathBuf::new();
    for part in absolute.components() {
        match part {
            #[cfg(windows)]
            std::path::Component::Prefix(prefix)
                if matches!(
                    prefix.kind(),
                    std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
                ) =>
            {
                anchor.push(prefix.as_os_str());
                anchor.push("\\");
            }
            std::path::Component::RootDir => {}
            std::path::Component::Normal(part) => components.push(
                part.to_str()
                    .context("local path must be UTF-8")?
                    .to_owned(),
            ),
            _ => anyhow::bail!("local file path must not contain parent traversal"),
        }
    }
    let path = components.join("/");
    treer_platform::machine_file::validate_relative_path(&path)?;
    #[cfg(unix)]
    let anchor = PathBuf::from("/");
    Ok((
        treer_platform::machine_file::Root::open_local(&anchor)?,
        path,
    ))
}

async fn download(transport: &Transport, options: &Options, limit: u64) -> anyhow::Result<()> {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (transport, options, limit);
        anyhow::bail!(
            "file_transfer_unsupported: safe local file operations unavailable on this platform"
        );
    }
    #[cfg(any(unix, windows))]
    {
        use std::io::{Read, Write};
        // stdout is withheld until the complete digest has been verified.
        let spool = if options.local == "-" {
            Some(Spool::new()?)
        } else {
            None
        };
        let (root, local) = if let Some(spool) = &spool {
            (
                treer_platform::machine_file::Root::open(&spool.0, &[])?,
                "verified".to_owned(),
            )
        } else {
            local_root(&options.local)?
        };
        let mut staged = root
            .stage(
                &local,
                &format!(".treer-transfer-{}", uuid::Uuid::new_v4()),
                options.mode,
            )
            .context("cannot create private local staging file")?;
        let begin: MachineTransferBegin = transport
            .call(Request::GetBegin {
                path: options.remote.clone(),
                limit,
            })
            .await?;
        validate_begin(&begin, &options.remote, limit)?;
        let result = async {
            let mut offset = 0_u64;
            let mut hash = Sha256::new();
            loop {
                let chunk: MachineTransferChunk = transport
                    .call(Request::GetChunk {
                        id: begin.id.clone(),
                        offset,
                    })
                    .await?;
                ensure!(
                    chunk.offset == offset
                        && chunk.content_base64.len()
                            <= MACHINE_TRANSFER_CHUNK_BYTES.div_ceil(3) * 4,
                    "invalid download chunk"
                );
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&chunk.content_base64)
                    .context("invalid chunk encoding")?;
                ensure!(
                    bytes.len() <= MACHINE_TRANSFER_CHUNK_BYTES
                        && offset + bytes.len() as u64 <= begin.limit,
                    "machine_file_too_large: response exceeds transfer limit"
                );
                ensure!(
                    chunk.eof == bytes.is_empty(),
                    "invalid download termination"
                );
                if chunk.eof {
                    break;
                }
                staged.file_mut().write_all(&bytes)?;
                hash.update(&bytes);
                offset += bytes.len() as u64;
            }
            let complete: MachineTransferComplete = transport
                .call(Request::GetFinish {
                    id: begin.id.clone(),
                })
                .await?;
            ensure!(
                complete.path == options.remote
                    && complete.size == offset
                    && begin.size.is_none_or(|size| size == offset)
                    && complete.sha256 == format!("{:x}", hash.finalize()),
                "machine_file_checksum_mismatch: download discarded"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if result.is_err() {
            transport.abort(&begin.id, true).await;
        }
        result?;
        staged.commit(options.overwrite)?;
        if spool.is_some() {
            let mut file = root.read(&local)?;
            let mut stdout = tokio::io::stdout();
            let mut buffer = vec![0; MACHINE_TRANSFER_CHUNK_BYTES];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                stdout.write_all(&buffer[..count]).await?;
            }
            stdout.flush().await?;
        }
        Ok(())
    }
}

#[cfg(any(unix, windows))]
struct Spool(PathBuf);
#[cfg(any(unix, windows))]
impl Spool {
    fn new() -> anyhow::Result<Self> {
        let path = std::env::temp_dir().join(format!("treer-file-spool-{}", uuid::Uuid::new_v4()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        }
        #[cfg(windows)]
        {
            std::fs::create_dir(&path)?;
            treer_platform::secure_directory(&path)?;
        }
        Ok(Self(std::fs::canonicalize(path)?))
    }
}
#[cfg(any(unix, windows))]
impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Fake {
        bytes: Vec<u8>,
        chunks: usize,
        bad_digest: bool,
        aborted: bool,
    }
    async fn info() -> Json<Value> {
        Json(json!({"capabilities":[CAPABILITY_MACHINE_FILE_TRANSFER],"max_bytes":262144}))
    }
    async fn request(
        State(state): State<Arc<Mutex<Fake>>>,
        headers: axum::http::HeaderMap,
        Json(call): Json<MachineFileTransferCall>,
    ) -> Result<Json<Value>, StatusCode> {
        if headers
            .get("x-treer-workload-credential")
            .and_then(|value| value.to_str().ok())
            != Some("test-workload")
        {
            return Err(StatusCode::FORBIDDEN);
        }
        assert_eq!(call.path, "remote.bin");
        let mut state = state.lock().unwrap();
        let response = match call.request {
            Request::PutBegin { path, limit, .. } => {
                state.bytes.clear();
                json!({"id":"fixture","path":path,"limit":limit,"chunk_bytes":MACHINE_TRANSFER_CHUNK_BYTES,"size":null})
            }
            Request::PutChunk {
                offset,
                content_base64,
                ..
            } => {
                assert_eq!(offset, state.bytes.len() as u64);
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(content_base64)
                    .unwrap();
                assert!(bytes.len() <= MACHINE_TRANSFER_CHUNK_BYTES);
                state.bytes.extend(bytes);
                state.chunks += 1;
                json!({"offset":state.bytes.len()})
            }
            Request::PutFinish { size, sha256, .. } => {
                assert_eq!(size, state.bytes.len() as u64);
                assert_eq!(sha256, format!("{:x}", Sha256::digest(&state.bytes)));
                json!({"path":call.path,"size":size,"sha256":sha256})
            }
            Request::GetBegin { path, limit } => {
                json!({"id":"fixture","path":path,"limit":limit,"chunk_bytes":MACHINE_TRANSFER_CHUNK_BYTES,"size":state.bytes.len()})
            }
            Request::GetChunk { offset, .. } => {
                let offset = offset as usize;
                let end = (offset + MACHINE_TRANSFER_CHUNK_BYTES).min(state.bytes.len());
                json!({"offset":offset,"content_base64":base64::engine::general_purpose::STANDARD.encode(&state.bytes[offset..end]),"eof":offset==end})
            }
            Request::GetFinish { .. } => {
                json!({"path":call.path,"size":state.bytes.len(),"sha256":if state.bad_digest {"0".repeat(64)} else {format!("{:x}", Sha256::digest(&state.bytes))}})
            }
            Request::Abort { .. } => {
                state.aborted = true;
                json!({"aborted":true})
            }
        };
        Ok(Json(response))
    }
    fn options(local: &Path, download: bool, overwrite: bool) -> Options {
        Options {
            path: "/api/machines/fixture/file-transfer".into(),
            local: local.to_str().unwrap().into(),
            remote: "remote.bin".into(),
            download,
            overwrite,
            mode: 0o600,
        }
    }
    #[tokio::test]
    async fn bounded_transport_round_trip_and_corrupt_download_never_publish() {
        let directory = Spool::new().unwrap();
        let state = Arc::new(Mutex::new(Fake::default()));
        let app = Router::new()
            .route(
                "/api/machines/fixture/file-transfer",
                get(info).post(request),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = url::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let headers = [("x-treer-workload-credential", "test-workload".to_owned())];
        let source = directory.0.join("source.bin");
        let destination = directory.0.join("destination.bin");
        let bytes: Vec<u8> = (0..170000).map(|index| (index % 251) as u8).collect();
        std::fs::write(&source, &bytes).unwrap();
        transfer(&base, &headers, options(&source, false, false))
            .await
            .unwrap();
        assert_eq!(state.lock().unwrap().chunks, 3);
        transfer(&base, &headers, options(&destination, true, false))
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
        std::fs::write(&destination, b"old").unwrap();
        assert!(
            transfer(&base, &headers, options(&destination, true, false))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"old");
        transfer(&base, &headers, options(&destination, true, true))
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
        state.lock().unwrap().bad_digest = true;
        std::fs::write(&destination, b"preserve").unwrap();
        assert!(transfer(&base, &headers, options(&destination, true, true))
            .await
            .unwrap_err()
            .to_string()
            .contains("checksum_mismatch"));
        assert_eq!(std::fs::read(&destination).unwrap(), b"preserve");
        assert!(state.lock().unwrap().aborted);
        assert!(!std::fs::read_dir(&directory.0).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".treer-transfer-")));
        server.abort();
    }
    #[tokio::test]
    async fn old_proxy_is_rejected_before_local_io_and_credentials_are_not_redirected() {
        let app = Router::new().route(
            "/api/machines/fixture/file-transfer",
            get(|| async { StatusCode::NOT_FOUND }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = url::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let error = transfer(
            &base,
            &[],
            options(Path::new("missing-local-file"), false, false),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("file_transfer_unsupported"));
        server.abort();
        assert!(super::super::ensure_file_transfer_capability(
            &json!({}),
            &["machine".into(), "put".into()]
        )
        .is_err());
    }
}
