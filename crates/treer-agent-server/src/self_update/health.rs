//! Bounded health observation at the selected enrollment's literal address.
//! A successful observation is not activation proof: the engine must also verify
//! the live Controller image/OS identity and unchanged Host/workload snapshot.
use std::{net::SocketAddr, time::Duration};

use anyhow::{bail, Result};
use reqwest::{redirect::Policy, Client};

use super::{
    artifacts::Binary,
    images::digest_file,
    safety::ControllerExpectation,
    selection::{Selected, SelectionLocks},
};

const LIMIT: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(2);

pub struct Monitor {
    client: Client,
}

/// Constructible only by observing the old Controller, never from an argv
/// epoch/default or the replacement Controller's first health response.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub struct InitialController {
    job: uuid::Uuid,
    workspace: String,
    server: String,
    digest: String,
    epoch: String,
    process: treer_platform::process_image::ProcessImage,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl InitialController {
    pub(super) fn validate(&self, receipt: &super::Receipt) -> Result<&str> {
        if self.job != receipt.job_id
            || self.workspace != receipt.workspace_id
            || self.server != receipt.server_id
            || self.digest != receipt.publications.original_digest(Binary::Controller)
            || receipt.phase != super::Phase::Preparing
        {
            bail!("update_baseline_binding_mismatch");
        }
        self.process
            .verify_current()
            .map_err(|_| anyhow::anyhow!("update_controller_identity_changed"))?;
        Ok(&self.epoch)
    }
}

impl Monitor {
    /// Before publication, bind the HTTP epoch to the selected original image
    /// executing as this verified Host's direct child. No mutation is performed.
    /// Binding is indirect: loopback TCP does not attest the listening process.
    /// Another process in the same OS-account trust domain could occupy this
    /// port and forge the epoch; the independent image/Host checks still apply.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub async fn observe_initial(
        &self,
        selected: &Selected,
        locks: &SelectionLocks,
        journal: &super::Journal,
        host: &super::host::Observation,
    ) -> Result<InitialController> {
        let receipt = journal.read()?;
        if receipt.phase != super::Phase::Preparing
            || receipt.workspace_id != selected.workspace()
            || receipt.server_id != selected.server_id()
        {
            bail!("update_baseline_binding_mismatch");
        }
        let digest = receipt
            .publications
            .original_digest(Binary::Controller)
            .to_owned();
        let process = host.controller_image(selected, locks, &digest)?;
        let document = self.document(selected.health_address()?).await?;
        let epoch = super::safety::initial_controller_epoch(
            &document,
            selected.workspace(),
            selected.server_id(),
        )?;
        host.verify().await?;
        let initial = InitialController {
            job: receipt.job_id,
            workspace: receipt.workspace_id.clone(),
            server: receipt.server_id.clone(),
            digest,
            epoch,
            process,
        };
        initial.validate(&journal.read()?)?;
        let image = locks
            .directory(selected, Binary::Controller)?
            .read_file(Binary::Controller.name())
            .map_err(|_| anyhow::anyhow!("unsafe_update_health_image"))?;
        if digest_file(&image)? != initial.digest {
            bail!("update_controller_health_mismatch");
        }
        Ok(initial)
    }

    pub fn new() -> Result<Self> {
        Self::with_timeout(TIMEOUT)
    }

    fn with_timeout(timeout: Duration) -> Result<Self> {
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .map_err(|_| anyhow::anyhow!("update_health_client_failed"))?;
        Ok(Self { client })
    }

    async fn document(&self, address: SocketAddr) -> Result<Vec<u8>> {
        // Literal SocketAddr prevents DNS rebinding and URL userinfo/query tricks.
        // The request is unauthenticated; no headers or credentials are inherited.
        let mut response = self
            .client
            .get(format!("http://{address}/api/health"))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("update_health_unavailable"))?;
        if response.status() != reqwest::StatusCode::OK {
            bail!("update_health_unavailable");
        }
        if response
            .content_length()
            .is_some_and(|size| size > LIMIT as u64)
        {
            bail!("update_health_too_large");
        }
        let mut document = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("update_health_unavailable"))?
        {
            if chunk.len() > LIMIT - document.len() {
                bail!("update_health_too_large");
            }
            document.extend_from_slice(&chunk);
        }
        Ok(document)
    }

    /// One bounded observation, without resubmitting any mutation or restart.
    /// Digest the installed image independently before and after HTTP; these
    /// checks alone still do not attest which image the live process executes.
    pub async fn observe(
        &self,
        selected: &Selected,
        locks: &SelectionLocks,
        expected: &ControllerExpectation,
    ) -> Result<String> {
        if expected.workspace != selected.workspace() || expected.server != selected.server_id() {
            bail!("update_health_binding_mismatch");
        }
        let directory = locks.directory(selected, Binary::Controller)?;
        let image = directory
            .read_file(Binary::Controller.name())
            .map_err(|_| anyhow::anyhow!("unsafe_update_health_image"))?;
        if digest_file(&image)? != expected.executable_sha256 {
            bail!("update_controller_health_mismatch");
        }
        let document = self.document(selected.health_address()?).await?;
        let image = directory
            .read_file(Binary::Controller.name())
            .map_err(|_| anyhow::anyhow!("unsafe_update_health_image"))?;
        expected.verify(&document, &digest_file(&image)?)
    }

    /// Compose HTTP and installed-file checks with the Host-owned live image
    /// and unchanged Host/workload observation. Still read-only: the transaction
    /// engine owns journal changes, restart requests, rollback and launcher work.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub async fn observe_live(
        &self,
        selected: &Selected,
        locks: &SelectionLocks,
        expected: &ControllerExpectation,
        host: &super::host::Observation,
    ) -> Result<String> {
        let process = host.controller_image(selected, locks, &expected.executable_sha256)?;
        let epoch = self.observe(selected, locks, expected).await?;
        process
            .verify_current()
            .map_err(|_| anyhow::anyhow!("update_controller_identity_changed"))?;
        host.verify().await?;
        process
            .verify_current()
            .map_err(|_| anyhow::anyhow!("update_controller_identity_changed"))?;
        Ok(epoch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn server(reply: Vec<u8>) -> (SocketAddr, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let byte = socket.read_u8().await.unwrap();
                request.push(byte);
                assert!(request.len() < 8192);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let _ = socket.write_all(&reply).await;
            request
        });
        (address, handle)
    }

    #[tokio::test]
    async fn request_is_fixed_and_has_no_authorization_or_identity_headers() {
        let (address, server) =
            server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec()).await;
        assert_eq!(
            Monitor::new().unwrap().document(address).await.unwrap(),
            b"{}"
        );
        let request = String::from_utf8(server.await.unwrap())
            .unwrap()
            .to_ascii_lowercase();
        assert!(request.starts_with("get /api/health http/1.1\r\n"));
        for forbidden in [
            "authorization:",
            "cookie:",
            "x-treer-",
            "proxy-authorization:",
        ] {
            assert!(!request.contains(forbidden));
        }
    }

    #[tokio::test]
    async fn redirects_and_errors_do_not_return_response_content_or_destination() {
        let target = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        for status in [302, 503] {
            let (address, server) = server(format!("HTTP/1.1 {status} Test\r\nLocation: http://{}/private?credential=synthetic-secret\r\nContent-Length: 16\r\n\r\nsynthetic-secret", target.local_addr().unwrap()).into_bytes()).await;
            assert_eq!(
                Monitor::new()
                    .unwrap()
                    .document(address)
                    .await
                    .err()
                    .unwrap()
                    .to_string(),
                "update_health_unavailable"
            );
            server.await.unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(30), target.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn body_limits_apply_with_and_without_content_length() {
        for chunked in [false, true] {
            let body = "x".repeat(LIMIT + 1);
            let reply = if chunked {
                format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n", body.len())
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
            };
            let (address, server) = server(reply.into_bytes()).await;
            assert_eq!(
                Monitor::new()
                    .unwrap()
                    .document(address)
                    .await
                    .err()
                    .unwrap()
                    .to_string(),
                "update_health_too_large"
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn stalled_body_has_one_total_deadline_and_fixed_diagnostic() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{")
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let monitor = Monitor::with_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(
            monitor.document(address).await.err().unwrap().to_string(),
            "update_health_unavailable"
        );
        server.abort();
        let _ = server.await;
    }
}
