//! Bounded HTTP/1 transport, including slow headers and upgraded connections.
use crate::files::WriteTimeout;
use axum::Router;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinSet};

pub async fn serve(
    listener: TcpListener,
    router: Router,
    idle: Duration,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let slots = Arc::new(Semaphore::new(fixture_limit(
        "RUST_SPIKE_HTTP_CONNECTION_LIMIT",
        128,
    )));
    let mut connections = JoinSet::new();
    let mut inject_accept_error = fixture_accept_error();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = async {
                if std::mem::take(&mut inject_accept_error) {
                    Err(std::io::Error::from_raw_os_error(libc::EMFILE))
                } else {
                    listener.accept().await
                }
            } => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(_) => {
                        eprintln!("Rust spike: http_accept_retry");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else { drop(stream); continue; };
                // The permit belongs to the I/O, not the HTTP future: an upgraded
                // WebSocket keeps its connection slot until the socket closes.
                let io = WriteTimeout::with_permit(stream, idle, permit);
                let service = TowerToHyperService::new(router.clone());
                connections.spawn(async move {
                    let mut builder = hyper::server::conn::http1::Builder::new();
                    builder.timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_millis(fixture_limit("RUST_SPIKE_HTTP_HEADER_MS", 10_000) as u64))
                        .max_headers(64).max_buf_size(32 * 1024);
                    let _ = builder.serve_connection(TokioIo::new(io), service).with_upgrades().await;
                });
            }
        }
    }
    // Keep-alive clients cannot extend launcher stop indefinitely. Upgraded
    // sockets are terminated when main returns and its runtime is dropped.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

fn fixture_accept_error() -> bool {
    #[cfg(feature = "fixture-harness")]
    {
        std::env::var("RUST_SPIKE_HTTP_ACCEPT_ERROR_ONCE").as_deref() == Ok("1")
    }
    #[cfg(not(feature = "fixture-harness"))]
    false
}

fn fixture_limit(_name: &str, default: usize) -> usize {
    #[cfg(feature = "fixture-harness")]
    if let Ok(value) = std::env::var(_name) {
        if let Ok(value) = value.parse::<usize>() {
            return value.clamp(1, default);
        }
    }
    default
}
