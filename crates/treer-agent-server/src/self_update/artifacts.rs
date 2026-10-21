//! Bounded, non-executable staging for the update engine. This module does not
//! activate services or choose installed destinations. A caller must authenticate
//! metadata before supplying a digest; a matching digest alone is not provenance.
use std::{fs::File, io::Write, time::Duration};

use anyhow::{bail, Result};
use ed25519_dalek::VerifyingKey;
use reqwest::{redirect::Policy, Client, Response};
use sha2::{Digest, Sha256};
use treer_platform::private_directory::PrivateDirectory;
use url::Url;

use super::hex;

const SUMS_LIMIT: usize = 8192;
const ARTIFACT_LIMIT: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    Controller,
    Cli,
}

impl Binary {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Controller => "treer-agent-server",
            Self::Cli => "treer",
        }
    }
}

/// Pins are constructed only after checking the exact checksum document digest.
/// The Host's checksum must be present, but the Host is never staged or changed.
pub struct PinnedChecksums {
    controller: String,
    cli: String,
}

impl PinnedChecksums {
    pub fn parse(bytes: &[u8], expected_document_sha256: &str) -> Result<Self> {
        if !hex(expected_document_sha256, 64)
            || bytes.is_empty()
            || bytes.len() > SUMS_LIMIT
            || format!("{:x}", Sha256::digest(bytes)) != expected_document_sha256
        {
            bail!("update_checksums_pin_mismatch");
        }
        let text =
            std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("invalid_update_checksums"))?;
        let mut entries = std::collections::BTreeMap::new();
        for line in text.lines() {
            // Match the publisher's canonical sha256sum text format. Reject
            // whitespace tricks, binary-marker aliases and path components.
            let Some((digest, name)) = line.split_once("  ") else {
                bail!("invalid_update_checksums");
            };
            if !hex(digest, 64)
                || !matches!(name, "treer" | "treer-agent-server" | "treer-agent-host")
                || entries.insert(name, digest).is_some()
            {
                bail!("invalid_update_checksums");
            }
        }
        if entries.len() != 3 {
            bail!("invalid_update_checksums");
        }
        Ok(Self {
            controller: entries["treer-agent-server"].to_owned(),
            cli: entries["treer"].to_owned(),
        })
    }

    pub fn digest(&self, binary: Binary) -> &str {
        match binary {
            Binary::Controller => &self.controller,
            Binary::Cli => &self.cli,
        }
    }
}

/// Owns its client: callers cannot accidentally enable redirects, ambient
/// proxies, cookies or request credentials by passing a differently built one.
pub struct Distributor {
    client: Client,
    base: Url,
}

impl Distributor {
    pub fn new(base: Url, enrollment: &Url, allow_artifact_origin: bool) -> Result<Self> {
        Self::validate_url(&base, cfg!(test))?;
        Self::validate_url(enrollment, cfg!(test))?;
        if base.origin() != enrollment.origin() && !allow_artifact_origin {
            bail!("update_artifact_origin_requires_ack");
        }
        if !base.path().ends_with('/') {
            bail!("invalid_update_distributor");
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|_| anyhow::anyhow!("update_http_client_failed"))?;
        Ok(Self { client, base })
    }

    fn validate_url(url: &Url, allow_test_http: bool) -> Result<()> {
        // Production never accepts HTTP; only local unit-test distributors do.
        let fixture_http = allow_test_http
            && url.scheme() == "http"
            && match url.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                _ => false,
            };
        if (url.scheme() != "https" && !fixture_http)
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("invalid_update_distributor");
        }
        Ok(())
    }

    async fn get(&self, name: &str, limit: u64) -> Result<Response> {
        // The name is internal: binary enum, fixed SHA256SUMS, or a path already
        // authenticated and structurally validated by the shared verifier.
        // Never format reqwest errors with URLs.
        let url = self
            .base
            .join(name)
            .map_err(|_| anyhow::anyhow!("invalid_update_distributor"))?;
        let response = self
            .client
            .get(url)
            .header("cache-control", "no-cache")
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("update_download_failed"))?;
        if !response.status().is_success() {
            bail!("update_download_http_status");
        }
        if response.content_length().is_some_and(|size| size > limit) {
            bail!("update_download_too_large");
        }
        Ok(response)
    }

    pub async fn pinned_checksums(&self, document_sha256: &str) -> Result<PinnedChecksums> {
        if !hex(document_sha256, 64) {
            bail!("update_checksums_pin_mismatch");
        }
        let mut response = self.get("SHA256SUMS", SUMS_LIMIT as u64).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("update_download_failed"))?
        {
            if chunk.len() > SUMS_LIMIT.saturating_sub(bytes.len()) {
                bail!("update_download_too_large");
            }
            bytes.extend_from_slice(&chunk);
        }
        PinnedChecksums::parse(&bytes, document_sha256)
    }

    /// Reuse the existing release signature verifier; never fetch a trust key
    /// from this distributor. Errors are deliberately collapsed so metadata,
    /// URLs and response bodies cannot become a public receipt diagnostic.
    pub async fn signed_release(
        &self,
        channel: &str,
        key: &VerifyingKey,
        platform: &str,
    ) -> Result<SignedRelease<'_>> {
        if channel.is_empty()
            || channel.len() > 64
            || !channel.starts_with(|c: char| c.is_ascii_lowercase())
            || !channel
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || !matches!(
                platform,
                "darwin-aarch64" | "darwin-x86_64" | "linux-aarch64" | "linux-x86_64"
            )
        {
            bail!("invalid_update_release_selector");
        }
        let release = crate::release_update::authenticate(
            &self.client,
            self.base.clone(),
            channel,
            key,
            platform,
        )
        .await
        .map_err(|_| anyhow::anyhow!("update_release_authentication_failed"))?;
        Ok(SignedRelease {
            distributor: self,
            release,
        })
    }

    /// The file remains 0600 and cannot be executed. Its returned handle is
    /// read-only, and every writer has been closed before this method returns.
    /// Cancellation/failure removes only the exact file this call created;
    /// an existing stage is never overwritten or removed.
    pub async fn stage(
        &self,
        directory: &PrivateDirectory,
        binary: Binary,
        digest: &str,
        expected_size: Option<u64>,
    ) -> Result<VerifiedArtifact> {
        self.stage_path(directory, binary, binary.name(), digest, expected_size)
            .await
    }

    async fn stage_path(
        &self,
        directory: &PrivateDirectory,
        binary: Binary,
        path: &str,
        digest: &str,
        expected_size: Option<u64>,
    ) -> Result<VerifiedArtifact> {
        if !hex(digest, 64) || expected_size.is_some_and(|size| size == 0 || size > ARTIFACT_LIMIT)
        {
            bail!("invalid_update_artifact_metadata");
        }
        let limit = expected_size.unwrap_or(ARTIFACT_LIMIT);
        let mut response = self.get(path, limit).await?;
        let mut file = directory.create_file(binary.name())?;
        let mut cleanup = PendingFile {
            directory,
            name: binary.name(),
            retained: false,
        };
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("update_download_failed"))?
        {
            if chunk.len() as u64 > limit.saturating_sub(size) {
                bail!("update_download_too_large");
            }
            file.write_all(&chunk)?;
            hasher.update(&chunk);
            size += chunk.len() as u64;
        }
        if size == 0
            || expected_size.is_some_and(|expected| size != expected)
            || format!("{:x}", hasher.finalize()) != digest
        {
            bail!("update_artifact_verification_failed");
        }
        file.sync_all()?;
        drop(file);
        let file = directory.read_file(binary.name())?;
        directory.sync()?;
        cleanup.retained = true;
        Ok(VerifiedArtifact { file, binary, size })
    }
}

/// A verified signed metadata plan is bound to the client/origin that fetched
/// it. It does not imply freshness/replay protection (P4), and its version is
/// not yet a source-commit assertion for reused native artifacts.
pub struct SignedRelease<'a> {
    distributor: &'a Distributor,
    release: crate::release_update::AuthenticatedRelease,
}

impl SignedRelease<'_> {
    pub fn version(&self) -> &str {
        &self.release.version
    }

    pub async fn stage(
        &self,
        directory: &PrivateDirectory,
        binary: Binary,
    ) -> Result<VerifiedArtifact> {
        let artifact = match binary {
            Binary::Controller => &self.release.controller,
            Binary::Cli => &self.release.cli,
        };
        self.distributor
            .stage_path(
                directory,
                binary,
                &artifact.path,
                &artifact.sha256,
                Some(artifact.size as u64),
            )
            .await
    }
}

struct PendingFile<'a> {
    directory: &'a PrivateDirectory,
    name: &'static str,
    retained: bool,
}

impl Drop for PendingFile<'_> {
    fn drop(&mut self) {
        if !self.retained {
            let _ = self.directory.remove_file(self.name);
        }
    }
}

pub struct VerifiedArtifact {
    file: File,
    binary: Binary,
    size: u64,
}

impl VerifiedArtifact {
    pub fn binary(&self) -> Binary {
        self.binary
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn file(&self) -> &File {
        &self.file
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Response, routing::get, Router};
    use std::{
        io::Read,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        path::PathBuf,
        sync::{Arc, Mutex},
    };
    use uuid::Uuid;

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn sums() -> Vec<u8> {
        ["treer", "treer-agent-server", "treer-agent-host"]
            .map(|name| format!("{}  {name}\n", digest(b"verified fixture")))
            .concat()
            .into_bytes()
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> (Self, PrivateDirectory) {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("treer-update-artifact-{}", Uuid::new_v4()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            let directory = PrivateDirectory::open(&path).unwrap();
            (Self(path), directory)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    struct Server {
        url: Url,
        task: tokio::task::JoinHandle<()>,
        requests: Arc<Mutex<Vec<String>>>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn serve(app: Router) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let app = app.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let seen = seen.clone();
                async move {
                    assert!(request.headers().get("authorization").is_none());
                    assert!(request.headers().get("cookie").is_none());
                    seen.lock().unwrap().push(request.uri().path().into());
                    next.run(request).await
                }
            },
        ));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Server {
            url,
            task,
            requests,
        }
    }

    #[test]
    fn checksum_document_is_pinned_before_strict_entry_parsing() {
        let bytes = sums();
        let checked = PinnedChecksums::parse(&bytes, &digest(&bytes)).unwrap();
        assert_eq!(
            checked.digest(Binary::Controller),
            digest(b"verified fixture")
        );
        assert!(PinnedChecksums::parse(&bytes, &"0".repeat(64)).is_err());
        let original = String::from_utf8(bytes).unwrap();
        for malformed in [
            original.replace("treer-agent-host\n", "../treer-agent-host\n"),
            original.replace("treer-agent-host\n", "treer\n"),
            original.replace("  treer-agent-host\n", " *treer-agent-host\n"),
            original.replace("  treer-agent-host\n", "  treer-agent-host \n"),
            original.lines().take(2).collect::<Vec<_>>().join("\n"),
            format!("{original}{}  unknown\n", "a".repeat(64)),
            "x".repeat(SUMS_LIMIT + 1),
        ] {
            assert!(
                PinnedChecksums::parse(malformed.as_bytes(), &digest(malformed.as_bytes()))
                    .is_err()
            );
        }
    }

    #[test]
    fn urls_require_explicit_origin_and_cannot_embed_secrets_or_redirect_paths() {
        let enrollment = Url::parse("https://proxy.example/").unwrap();
        assert!(Distributor::new(
            Url::parse("https://other.example/artifacts/").unwrap(),
            &enrollment,
            false,
        )
        .is_err());
        assert!(Distributor::new(
            Url::parse("https://other.example/artifacts/").unwrap(),
            &enrollment,
            true,
        )
        .is_ok());
        for url in [
            "https://user:synthetic-secret@proxy.example/artifacts/",
            "https://proxy.example/artifacts/?token=synthetic-secret",
            "https://proxy.example/artifacts/#synthetic-secret",
            "http://169.254.169.254/",
            "file:///synthetic-secret/",
            "https://proxy.example/artifacts",
        ] {
            let error = Distributor::new(Url::parse(url).unwrap(), &enrollment, true)
                .err()
                .unwrap()
                .to_string();
            assert_eq!(error, "invalid_update_distributor");
        }
        assert!(
            Distributor::validate_url(&Url::parse("http://127.0.0.1/").unwrap(), false).is_err()
        );
    }

    #[tokio::test]
    async fn downloaded_stages_are_private_readonly_and_never_overwrite_existing_files() {
        let server = serve(
            Router::new()
                .route("/SHA256SUMS", get(|| async { sums() }))
                .route("/treer-agent-server", get(|| async { b"verified fixture" })),
        )
        .await;
        let client = Distributor::new(server.url.clone(), &server.url, false).unwrap();
        let checksums = client.pinned_checksums(&digest(&sums())).await.unwrap();
        let (fixture, directory) = Fixture::new();
        let staged = client
            .stage(
                &directory,
                Binary::Controller,
                checksums.digest(Binary::Controller),
                Some(16),
            )
            .await
            .unwrap();
        assert_eq!(staged.size, 16);
        let mut bytes = Vec::new();
        staged.file().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"verified fixture");
        assert!(staged.file().write_all(b"mutation").is_err());
        assert_eq!(
            std::fs::metadata(fixture.0.join("treer-agent-server"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(client
            .stage(
                &directory,
                Binary::Controller,
                &digest(b"verified fixture"),
                None
            )
            .await
            .is_err());
        assert_eq!(
            std::fs::read(fixture.0.join("treer-agent-server")).unwrap(),
            b"verified fixture"
        );
        assert!(!fixture.0.join("treer-agent-host").exists());
    }

    #[tokio::test]
    async fn chunked_oversize_wrong_hash_and_short_body_leave_no_stage() {
        let server = serve(Router::new().route(
            "/treer",
            get(|| async {
                // Deliberately omit Content-Length to exercise the streaming
                // limit instead of relying on a distributor's claimed size.
                Body::from_stream(futures_util::stream::iter([
                    Ok::<_, std::io::Error>(b"abc".to_vec()),
                    Ok(b"def".to_vec()),
                ]))
            }),
        ))
        .await;
        let client = Distributor::new(server.url.clone(), &server.url, false).unwrap();
        let (fixture, directory) = Fixture::new();
        for (expected_size, hash, error) in [
            (Some(5), digest(b"abcde"), "update_download_too_large"),
            (
                Some(7),
                digest(b"abcdefg"),
                "update_artifact_verification_failed",
            ),
            (
                Some(6),
                digest(b"wrong!"),
                "update_artifact_verification_failed",
            ),
        ] {
            let result = client
                .stage(&directory, Binary::Cli, &hash, expected_size)
                .await;
            assert_eq!(result.err().unwrap().to_string(), error);
            assert!(!fixture.0.join("treer").exists());
        }
    }

    #[tokio::test]
    async fn cancelling_an_incomplete_download_removes_only_its_owned_file() {
        use std::future::Future;
        let server = serve(Router::new().route(
            "/treer",
            get(|| async {
                Body::from_stream(futures_util::stream::pending::<
                    Result<Vec<u8>, std::io::Error>,
                >())
            }),
        ))
        .await;
        let client = Distributor::new(server.url.clone(), &server.url, false).unwrap();
        let (fixture, directory) = Fixture::new();
        directory
            .create_file("unrelated")
            .unwrap()
            .write_all(b"keep")
            .unwrap();
        let hash = digest(b"unfinished");
        let mut download = Box::pin(client.stage(&directory, Binary::Cli, &hash, None));
        // Poll the actual production future to its suspended body-read point.
        // No elapsed-time sleep determines when to cancel it.
        tokio::time::timeout(
            Duration::from_secs(5),
            futures_util::future::poll_fn(|cx| {
                assert!(download.as_mut().poll(cx).is_pending());
                if fixture.0.join("treer").exists() {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            }),
        )
        .await
        .unwrap();
        drop(download);
        assert!(!fixture.0.join("treer").exists());
        assert_eq!(std::fs::read(fixture.0.join("unrelated")).unwrap(), b"keep");
    }

    #[tokio::test]
    async fn signed_metadata_uses_shared_verifier_before_fetching_executable_bytes() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use ed25519_dalek::{pkcs8::EncodePublicKey, Signer, SigningKey};
        use serde_json::json;
        let key = SigningKey::from_bytes(&[42; 32]);
        let key_id =
            digest(key.verifying_key().to_public_key_der().unwrap().as_bytes())[..16].to_owned();
        let version = "v0.1.14";
        let artifact_path = format!("releases/{version}/linux-x86_64/treer");
        let controller_path = format!("releases/{version}/linux-x86_64/treer-agent-server");
        let manifest_path = format!("releases/{version}/manifest.json");
        let manifest = serde_json::to_vec(&json!({
            "schema_version":2, "version":version, "signing_key_id":key_id,
            "artifacts":[
                {"binary":"treer", "platform":"linux-x86_64", "path":artifact_path,
                 "sha256":digest(b"signed fixture"), "size":14},
                {"binary":"treer-agent-server", "platform":"linux-x86_64", "path":controller_path,
                 "sha256":digest(b"signed fixture"), "size":14}
            ]
        }))
        .unwrap();
        let channel = serde_json::to_vec(&json!({
            "schema_version":1, "channel":"canary", "version":version,
            "manifest_path":manifest_path, "manifest_sha256":digest(&manifest)
        }))
        .unwrap();
        let signature = |bytes: &[u8]| {
            serde_json::to_vec(&json!({
                "schema_version":1, "algorithm":"ed25519", "key_id":key_id,
                "signature":STANDARD.encode(key.sign(bytes).to_bytes())
            }))
            .unwrap()
        };
        let objects = Arc::new(std::collections::HashMap::from([
            ("channels/canary.json".into(), channel.clone()),
            ("channels/canary.json.sig".into(), signature(&channel)),
            (manifest_path.clone(), manifest.clone()),
            (format!("{manifest_path}.sig"), signature(&manifest)),
            (artifact_path.clone(), b"signed fixture".to_vec()),
        ]));
        let server = serve(Router::new().fallback(move |uri: axum::http::Uri| {
            let objects = objects.clone();
            async move {
                match objects.get(uri.path().trim_start_matches('/')) {
                    Some(bytes) => Response::builder().body(Body::from(bytes.clone())).unwrap(),
                    None => Response::builder().status(404).body(Body::empty()).unwrap(),
                }
            }
        }))
        .await;
        let client = Distributor::new(server.url.clone(), &server.url, false).unwrap();
        let wrong_key = SigningKey::from_bytes(&[43; 32]).verifying_key();
        assert_eq!(
            client
                .signed_release("canary", &wrong_key, "linux-x86_64")
                .await
                .err()
                .unwrap()
                .to_string(),
            "update_release_authentication_failed"
        );
        assert!(!server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|path| path == &format!("/{artifact_path}")));
        let plan = client
            .signed_release("canary", &key.verifying_key(), "linux-x86_64")
            .await
            .unwrap();
        assert_eq!(plan.version(), version);
        let (_fixture, directory) = Fixture::new();
        let artifact = plan.stage(&directory, Binary::Cli).await.unwrap();
        assert_eq!(artifact.size, 14);
        let mut bytes = Vec::new();
        artifact.file().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"signed fixture");
        assert!(!server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|path| path == &format!("/{controller_path}")));
    }

    #[tokio::test]
    async fn redirects_and_oversized_headers_fail_without_following_or_writing() {
        let target = serve(Router::new().route("/treer", get(|| async { "not fetched" }))).await;
        let location = target.url.join("treer").unwrap().to_string();
        let server = serve(
            Router::new()
                .route(
                    "/treer",
                    get(move || async move {
                        Response::builder()
                            .status(302)
                            .header("location", location)
                            .body(Body::empty())
                            .unwrap()
                    }),
                )
                .route(
                    "/treer-agent-server",
                    get(|| async {
                        Response::builder()
                            .header("content-length", ARTIFACT_LIMIT + 1)
                            .body(Body::empty())
                            .unwrap()
                    }),
                ),
        )
        .await;
        let client = Distributor::new(server.url.clone(), &server.url, false).unwrap();
        let (fixture, directory) = Fixture::new();
        assert_eq!(
            client
                .stage(&directory, Binary::Cli, &digest(b"x"), None)
                .await
                .err()
                .unwrap()
                .to_string(),
            "update_download_http_status"
        );
        assert!(target.requests.lock().unwrap().is_empty());
        assert!(client
            .stage(&directory, Binary::Controller, &digest(b"x"), None)
            .await
            .is_err());
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 0);
    }
}
