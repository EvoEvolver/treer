//! Explicit, identity-bound HTTP proxy entries. No packet capture or OS DNS changes.
use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::sync::Mutex;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::sync::watch;
use treer_protocol::{
    ProtocolError, WorkloadIdentityTokenResponse, AGENT_ID_HEADER, WORKLOAD_CREDENTIAL_HEADER,
};

const IDENTITY_HEADER: &str = "x-treer-identity-token";

pub struct WorkloadProxies {
    api: String,
    entries: Mutex<HashMap<String, Entry>>,
}

struct Entry {
    port: u16,
    credential: String,
    cancel: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
        self.task.abort();
    }
}

#[derive(Clone)]
struct ProxyEntry {
    api: String,
    agent_id: String,
    credential: String,
    client: reqwest::Client,
    cancelled: watch::Receiver<bool>,
}

impl WorkloadProxies {
    pub fn new(api: String) -> Self {
        Self {
            api,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Reserve before spawn. Host metadata preserves the port across Controller replacement.
    pub fn ensure(
        &self,
        agent_id: &str,
        credential: &str,
        port: Option<u16>,
    ) -> Result<u16, ProtocolError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "proxy registry lock poisoned"))?;
        if let Some(entry) = entries.get(agent_id) {
            if entry.credential == credential && port.is_none_or(|port| port == entry.port) {
                return Ok(entry.port);
            }
            return Err(ProtocolError::new(
                "proxy_identity_conflict",
                "workload already has a different proxy entry",
            ));
        }
        let listener = (|| -> std::io::Result<_> {
            let socket = tokio::net::TcpSocket::new_v4()?;
            socket.set_reuseaddr(true)?;
            socket.bind(([127, 0, 0, 1], port.unwrap_or(0)).into())?;
            socket.listen(1024)
        })()
        .map_err(|error| {
            ProtocolError::new(
                "proxy_bind_failed",
                format!("cannot bind workload proxy: {error}"),
            )
        })?;
        let port = listener
            .local_addr()
            .map_err(|error| ProtocolError::new("proxy_bind_failed", error.to_string()))?
            .port();
        let (cancel, cancelled) = watch::channel(false);
        let entry = ProxyEntry {
            api: self.api.clone(),
            agent_id: agent_id.into(),
            credential: credential.into(),
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|error| ProtocolError::new("proxy_client_failed", error.to_string()))?,
            cancelled,
        };
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept(), if connections.len() < 256 => {
                        let Ok((socket, _)) = accepted else { break };
                        let entry = entry.clone();
                        connections.spawn(async move {
                            let mut cancelled = entry.cancelled.clone();
                            let connection = hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(socket), service_fn(move |request| {
                                    let entry = entry.clone();
                                    async move { Ok::<_, Infallible>(entry.handle(request).await) }
                                })).with_upgrades();
                            tokio::select! { _ = connection => {}, _ = cancelled.changed() => {} }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        entries.insert(
            agent_id.into(),
            Entry {
                port,
                credential: credential.into(),
                cancel,
                task,
            },
        );
        Ok(port)
    }

    pub fn remove(&self, agent_id: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(agent_id);
        }
    }
}

pub fn inject_environment(env: &mut BTreeMap<String, String>, port: u16) {
    let url = format!("http://127.0.0.1:{port}");
    for key in [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "TREER_HTTP_PROXY",
    ] {
        env.insert(key.into(), url.clone());
    }
    let inherited = std::env::var("no_proxy")
        .or_else(|_| std::env::var("NO_PROXY"))
        .unwrap_or_default();
    let bypass = format!(
        "localhost,127.0.0.1,::1{suffix}",
        suffix = if inherited.is_empty() {
            String::new()
        } else {
            format!(",{inherited}")
        }
    );
    env.insert("no_proxy".into(), bypass.clone());
    env.insert("NO_PROXY".into(), bypass);
}

type Failure = (StatusCode, String);
type Stream = Box<dyn crate::network::DestinationStream>;

impl ProxyEntry {
    fn control(&self, method: Method, suffix: &str) -> reqwest::RequestBuilder {
        self.client
            .request(
                method,
                format!("{}/{suffix}", self.api.trim_end_matches('/')),
            )
            .header(AGENT_ID_HEADER, &self.agent_id)
            .header(WORKLOAD_CREDENTIAL_HEADER, &self.credential)
    }

    async fn connect(
        &self,
        host: &str,
        port: u16,
        service: Option<&str>,
    ) -> Result<Stream, Failure> {
        if let Some(service) = service {
            let response = self
                .control(Method::POST, &format!("api/services/{service}/connect"))
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, "treer-tcp")
                .send()
                .await
                .map_err(gateway)?;
            if response.status() != StatusCode::SWITCHING_PROTOCOLS {
                return Err((
                    if response.status().is_client_error() || response.status().is_server_error() {
                        response.status()
                    } else {
                        StatusCode::BAD_GATEWAY
                    },
                    "Treer service connection denied or unavailable".into(),
                ));
            }
            Ok(Box::new(response.upgrade().await.map_err(gateway)?))
        } else {
            let socket =
                tokio::time::timeout(Duration::from_secs(30), TcpStream::connect((host, port)))
                    .await
                    .map_err(gateway)?
                    .map_err(gateway)?;
            Ok(Box::new(socket))
        }
    }

    async fn handle(&self, request: Request<hyper::body::Incoming>) -> Response<Body> {
        match self.forward(request).await {
            Ok(response) => response,
            Err((status, message)) => Response::builder()
                .status(status)
                .body(Body::from(message))
                .expect("valid proxy error response"),
        }
    }

    async fn forward(
        &self,
        mut request: Request<hyper::body::Incoming>,
    ) -> Result<Response<Body>, Failure> {
        let connect = request.method() == Method::CONNECT;
        let uri = request.uri().clone();
        let authority = uri.authority().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "absolute proxy URL or CONNECT authority required".into(),
            )
        })?;
        if !connect && uri.scheme_str() != Some("http") {
            return Err((StatusCode::BAD_REQUEST, "use CONNECT for HTTPS".into()));
        }
        let host = authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']');
        let port = authority
            .port_u16()
            .unwrap_or(if connect { 443 } else { 80 });
        let service = service_target(host)?;
        // Mint for each request: no stale token cache and no refresh burden on callers.
        let identity = if !connect {
            if let Some(service) = &service {
                let response = self
                    .control(Method::POST, "api/identity/token")
                    .json(&serde_json::json!({"audience":service}))
                    .send()
                    .await
                    .map_err(gateway)?;
                if !response.status().is_success() {
                    return Err((
                        response.status(),
                        "Treer service identity denied or unavailable".into(),
                    ));
                }
                Some(
                    response
                        .json::<WorkloadIdentityTokenResponse>()
                        .await
                        .map_err(gateway)?,
                )
            } else {
                None
            }
        } else {
            None
        };
        // Pin HTTP to the canonical audience returned by the same lookup that signed
        // the token. A concurrent rename/reuse must not authenticate to a different service.
        let target = identity
            .as_ref()
            .map(|identity| identity.audience.as_str())
            .or(service.as_deref());
        let remote = self.connect(host, port, target).await?;
        if connect {
            let local = hyper::upgrade::on(&mut request);
            let mut cancelled = self.cancelled.clone();
            tokio::spawn(async move {
                let transfer = async {
                    let Ok(local) = local.await else { return };
                    let mut local = TokioIo::new(local);
                    let mut remote = remote;
                    let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
                };
                tokio::select! { _ = transfer => {}, _ = cancelled.changed() => {} }
            });
            return Ok(Response::new(Body::empty()));
        }
        let upgraded = request.headers().contains_key(header::UPGRADE);
        let local_upgrade = upgraded.then(|| hyper::upgrade::on(&mut request));
        sanitize_headers(request.headers_mut(), upgraded);
        request.headers_mut().insert(
            header::HOST,
            HeaderValue::from_str(authority.as_str()).map_err(gateway)?,
        );
        if let Some(identity) = identity {
            let token = identity.access_token;
            request.headers_mut().insert(
                IDENTITY_HEADER,
                HeaderValue::from_str(&token).map_err(gateway)?,
            );
            if !request.headers().contains_key(header::AUTHORIZATION) {
                request.headers_mut().insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}")).map_err(gateway)?,
                );
            }
        }
        *request.uri_mut() = uri
            .path_and_query()
            .map_or("/", |path| path.as_str())
            .parse::<Uri>()
            .map_err(gateway)?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(remote))
            .await
            .map_err(gateway)?;
        let mut cancelled = self.cancelled.clone();
        tokio::spawn(async move {
            tokio::select! { _ = connection.with_upgrades() => {}, _ = cancelled.changed() => {} }
        });
        let mut response = sender.send_request(request).await.map_err(gateway)?;
        let remote_upgrade = (response.status() == StatusCode::SWITCHING_PROTOCOLS)
            .then(|| hyper::upgrade::on(&mut response));
        if let (Some(local), Some(remote)) = (local_upgrade, remote_upgrade) {
            let mut cancelled = self.cancelled.clone();
            tokio::spawn(async move {
                let transfer = async {
                    let (Ok(local), Ok(remote)) = tokio::join!(local, remote) else {
                        return;
                    };
                    let mut local = TokioIo::new(local);
                    let mut remote = TokioIo::new(remote);
                    let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
                };
                tokio::select! { _ = transfer => {}, _ = cancelled.changed() => {} }
            });
        }
        let upgraded = response.status() == StatusCode::SWITCHING_PROTOCOLS;
        sanitize_headers(response.headers_mut(), upgraded);
        let (parts, body) = response.into_parts();
        Ok(Response::from_parts(parts, Body::new(body)))
    }
}

// Names resolve within the authenticated workspace, never through public DNS.
fn service_target(host: &str) -> Result<Option<String>, Failure> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let Some(target) = host.strip_suffix(".treer") else {
        return Ok(None);
    };
    let stable_id = target.starts_with("svc_")
        && target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    let name = !target.is_empty()
        && target.len() <= 63
        && !target.starts_with('-')
        && !target.ends_with('-')
        && target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if !stable_id && !name {
        return Err((
            StatusCode::BAD_REQUEST,
            "use <service-id>.treer or a service name with 1-63 ASCII letters, digits or internal hyphens".into(),
        ));
    }
    Ok(Some(target.to_owned()))
}

fn gateway(error: impl std::fmt::Display) -> Failure {
    (
        StatusCode::BAD_GATEWAY,
        format!("proxy upstream failed: {error}"),
    )
}

fn sanitize_headers(headers: &mut HeaderMap, upgraded: bool) {
    // Never pass local machine/operator/workload credentials or spoofed Treer identity.
    for name in headers
        .keys()
        .filter(|name| treer_protocol::is_treer_header_name(name.as_str()))
        .cloned()
        .collect::<Vec<_>>()
    {
        headers.remove(name);
    }
    headers.remove("treer-authorization");
    headers.remove(header::PROXY_AUTHORIZATION);
    headers.remove("proxy-connection");
    let nominated = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(',').map(|value| value.trim().to_owned()))
        .collect::<Vec<_>>();
    for name in nominated {
        if !(upgraded && name.eq_ignore_ascii_case("upgrade")) {
            headers.remove(name);
        }
    }
    for name in [
        "keep-alive",
        "te",
        "trailer",
        "transfer-encoding",
        "proxy-authenticate",
    ] {
        headers.remove(name);
    }
    if !upgraded {
        headers.remove(header::CONNECTION);
        headers.remove(header::UPGRADE);
    }
}

#[cfg(test)]
#[path = "workload_proxy_tests.rs"]
mod tests;
