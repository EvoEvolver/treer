//! HTTP/1 serving with connection-level protection (#146): a header read
//! timeout (slow-header clients are disconnected), bounded header size and
//! count, keep-alive, the peer address in every request's extensions
//! (`ConnectInfo`, exactly as axum's make-service provides it) and graceful
//! shutdown that lets in-flight requests finish.
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tower::ServiceExt;

/// Clients must finish sending request headers within this long.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest request head hyper will buffer (headers + request line).
pub const MAX_HEADER_BYTES: usize = 32 * 1024;
pub const MAX_HEADERS: usize = 100;
/// How long shutdown waits for in-flight connections before closing them.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);
/// Open connections the process accepts at once; beyond it new connections
/// are closed immediately so idle or half-open floods cannot exhaust file
/// descriptors.
pub const MAX_CONNECTIONS: usize = 8192;
/// Open connections one peer address may hold at once.
pub const MAX_CONNECTIONS_PER_PEER: usize = 256;

#[derive(Clone, Copy, Debug)]
pub struct ServerLimits {
    pub header_read_timeout: Duration,
    pub max_header_bytes: usize,
    pub max_headers: usize,
    pub shutdown_grace: Duration,
    pub max_connections: usize,
    pub max_connections_per_peer: usize,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            header_read_timeout: HEADER_READ_TIMEOUT,
            max_header_bytes: MAX_HEADER_BYTES,
            max_headers: MAX_HEADERS,
            shutdown_grace: SHUTDOWN_GRACE,
            max_connections: MAX_CONNECTIONS,
            max_connections_per_peer: MAX_CONNECTIONS_PER_PEER,
        }
    }
}

/// Counts open connections globally and per peer; a slot decrements on drop.
#[derive(Default)]
struct ConnectionBudget {
    total: usize,
    per_peer: std::collections::HashMap<std::net::IpAddr, usize>,
}

struct ConnectionSlot {
    budget: std::sync::Arc<std::sync::Mutex<ConnectionBudget>>,
    peer: std::net::IpAddr,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        let mut budget = self.budget.lock().unwrap_or_else(|p| p.into_inner());
        budget.total = budget.total.saturating_sub(1);
        if let Some(count) = budget.per_peer.get_mut(&self.peer) {
            *count -= 1;
            if *count == 0 {
                budget.per_peer.remove(&self.peer);
            }
        }
    }
}

fn take_slot(
    budget: &std::sync::Arc<std::sync::Mutex<ConnectionBudget>>,
    peer: std::net::IpAddr,
    limits: &ServerLimits,
) -> Option<ConnectionSlot> {
    let mut guard = budget.lock().unwrap_or_else(|p| p.into_inner());
    let per_peer = guard.per_peer.get(&peer).copied().unwrap_or(0);
    if guard.total >= limits.max_connections || per_peer >= limits.max_connections_per_peer {
        return None;
    }
    guard.total += 1;
    *guard.per_peer.entry(peer).or_insert(0) += 1;
    Some(ConnectionSlot {
        budget: budget.clone(),
        peer,
    })
}

/// Accepts connections until `shutdown` resolves, then drains gracefully.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    limits: ServerLimits,
    shutdown: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    let graceful = GracefulShutdown::new();
    let mut builder = Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_read_timeout)
        .max_buf_size(limits.max_header_bytes.max(8192))
        .max_headers(limits.max_headers)
        .keep_alive(true);
    // HTTP/1 only, as before this module (axum::serve without the http2
    // feature never negotiated h2c either); TLS fronts speak HTTP/1 upstream.
    let builder = builder.http1_only();
    let budget = std::sync::Arc::new(std::sync::Mutex::new(ConnectionBudget::default()));
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    // Transient accept errors (EMFILE, aborted handshakes)
                    // must not stop the server.
                    tracing::warn!(%error, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let Some(slot) = take_slot(&budget, peer.ip(), &limits) else {
            // Over budget: close at once, no handshake, no handler.
            drop(stream);
            tracing::debug!(%peer, "connection refused: connection budget exhausted");
            continue;
        };
        let service = router.clone().map_request(
            move |mut request: axum::http::Request<hyper::body::Incoming>| {
                request.extensions_mut().insert(ConnectInfo(peer));
                request
            },
        );
        let connection = builder.serve_connection_with_upgrades(
            TokioIo::new(stream),
            TowerToHyperService::new(service),
        );
        let watched = graceful.watch(connection.into_owned());
        tokio::spawn(async move {
            let _slot = slot;
            if let Err(error) = watched.await {
                tracing::debug!(%error, %peer, "connection closed with error");
            }
        });
    }
    drop(listener);
    tokio::select! {
        () = graceful.shutdown() => {}
        () = tokio::time::sleep(limits.shutdown_grace) => {
            tracing::warn!("shutdown grace period elapsed; closing remaining connections");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn peer_echo(ConnectInfo(peer): ConnectInfo<SocketAddr>) -> String {
        peer.ip().to_string()
    }

    async fn slow() -> &'static str {
        tokio::time::sleep(Duration::from_millis(300)).await;
        "done"
    }

    fn router() -> Router {
        Router::new()
            .route("/peer", get(peer_echo))
            .route("/slow", get(slow))
    }

    async fn start(
        limits: ServerLimits,
    ) -> (
        SocketAddr,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            serve(listener, router(), limits, async move {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        (address, tx, handle)
    }

    async fn raw(address: SocketAddr, request: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            response.push_str(&String::from_utf8_lossy(&buffer[..read]));
            if response.contains("\r\n\r\n") && response.ends_with("done")
                || response.contains("\r\n\r\n") && response.contains("127.0.0.1")
            {
                break;
            }
        }
        response
    }

    #[tokio::test]
    async fn requests_carry_the_peer_address_like_axum_serve() {
        let (address, stop, handle) = start(ServerLimits::default()).await;
        let response = raw(
            address,
            "GET /peer HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("127.0.0.1"), "{response}");
        let _ = stop.send(());
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn slow_headers_are_disconnected_at_the_timeout() {
        let limits = ServerLimits {
            header_read_timeout: Duration::from_millis(300),
            ..ServerLimits::default()
        };
        let (address, stop, handle) = start(limits).await;
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /peer HTTP/1.1\r\nHost: x\r\nX-Slow: ")
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let mut buffer = [0_u8; 1024];
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
            .await
            .expect("the server closes a slow-header connection")
            .unwrap();
        let closed_quickly = started.elapsed() < Duration::from_secs(3);
        assert!(closed_quickly, "took {:?}", started.elapsed());
        // hyper answers 408 or simply closes; either ends the attack.
        let text = String::from_utf8_lossy(&buffer[..read]);
        assert!(read == 0 || text.starts_with("HTTP/1.1 408"), "{text}");
        let _ = stop.send(());
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_headers_are_refused() {
        let limits = ServerLimits {
            max_header_bytes: 8192,
            ..ServerLimits::default()
        };
        let (address, stop, handle) = start(limits).await;
        let huge = "x".repeat(16 * 1024);
        let request =
            format!("GET /peer HTTP/1.1\r\nHost: x\r\nX-Big: {huge}\r\nConnection: close\r\n\r\n");
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        // The server may close before the oversized head is fully written.
        let _ = stream.write_all(request.as_bytes()).await;
        let mut response = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response)).await;
        let text = String::from_utf8_lossy(&response);
        assert!(
            text.is_empty() || text.starts_with("HTTP/1.1 431"),
            "an oversized head is refused or dropped, never served: {text}"
        );
        let _ = stop.send(());
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn connections_beyond_the_peer_budget_are_closed_at_once() {
        let limits = ServerLimits {
            max_connections_per_peer: 2,
            ..ServerLimits::default()
        };
        let (address, stop, handle) = start(limits).await;
        let first = tokio::net::TcpStream::connect(address).await.unwrap();
        let second = tokio::net::TcpStream::connect(address).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut third = tokio::net::TcpStream::connect(address).await.unwrap();
        let _ = third
            .write_all(b"GET /peer HTTP/1.1\r\nHost: x\r\n\r\n")
            .await;
        let mut buffer = [0_u8; 64];
        let read = tokio::time::timeout(Duration::from_secs(5), third.read(&mut buffer))
            .await
            .expect("the third connection is closed, not served")
            .unwrap_or(0);
        assert_eq!(read, 0, "no response on a refused connection");
        // Releasing one slot admits a new connection.
        drop(first);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let response = raw(
            address,
            "GET /peer HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        drop(second);
        let _ = stop.send(());
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_waits_for_in_flight_requests() {
        let (address, stop, handle) = start(ServerLimits::default()).await;
        let in_flight = tokio::spawn(raw(
            address,
            "GET /slow HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = stop.send(());
        let response = in_flight.await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("done"), "{response}");
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::net::TcpStream::connect(address).await.is_err(),
            "listener is closed after shutdown"
        );
    }
}
