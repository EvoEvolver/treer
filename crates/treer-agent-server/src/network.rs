//! Explicit service streams received over the authenticated Controller link.
//! No SOCKS listener or transparent capture; explicit HTTP entries live in workload_proxy.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch, Mutex};
use treer_protocol::{NetworkBinaryFrame, NetworkBinaryKind, NetworkConnectRequest, ProtocolError};

const STREAM_CHANNEL_CAPACITY: usize = 32;
const OUTGOING_CHANNEL_CAPACITY: usize = 128;
const MAX_STREAMS: usize = 4096;
const INITIAL_WINDOW: usize = 256 * 1024;
const MAX_CHUNK: usize = 16 * 1024;

#[derive(Clone)]
pub struct NetworkRuntime {
    inner: Arc<NetworkInner>,
    epoch: Option<u64>,
}

struct Stream {
    sender: mpsc::Sender<NetworkBinaryFrame>,
    cancel: watch::Sender<bool>,
}

struct NetworkInner {
    outgoing: mpsc::Sender<(u64, NetworkBinaryFrame)>,
    outgoing_rx: Mutex<mpsc::Receiver<(u64, NetworkBinaryFrame)>>,
    proxy_connected: AtomicBool,
    transport_epoch: AtomicU64,
    streams: Mutex<HashMap<String, Stream>>,
}

impl NetworkRuntime {
    pub fn new() -> Self {
        let (outgoing, outgoing_rx) = mpsc::channel(OUTGOING_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(NetworkInner {
                outgoing,
                outgoing_rx: Mutex::new(outgoing_rx),
                proxy_connected: AtomicBool::new(false),
                transport_epoch: AtomicU64::new(0),
                streams: Mutex::new(HashMap::new()),
            }),
            epoch: None,
        }
    }

    pub async fn next_outgoing(&self) -> Option<NetworkBinaryFrame> {
        let mut receiver = self.inner.outgoing_rx.lock().await;
        while let Some((epoch, frame)) = receiver.recv().await {
            if epoch == self.inner.transport_epoch.load(Ordering::SeqCst) {
                return Some(frame);
            }
        }
        None
    }

    pub fn set_proxy_connected(&self) {
        self.inner.proxy_connected.store(true, Ordering::SeqCst);
    }

    pub async fn handle_incoming(&self, frame: NetworkBinaryFrame) -> anyhow::Result<()> {
        if frame.kind == NetworkBinaryKind::Open {
            return self.spawn_destination(frame).await;
        }
        if matches!(
            frame.kind,
            NetworkBinaryKind::OpenDatagram
                | NetworkBinaryKind::Direct
                | NetworkBinaryKind::Usage
                | NetworkBinaryKind::UsageAck
        ) {
            return self
                .reject(
                    frame.stream_id,
                    "unsupported_transport",
                    "Only explicit registered TCP service streams are supported",
                )
                .await;
        }
        let stream_id = frame.stream_id.clone();
        let is_reset = frame.kind == NetworkBinaryKind::Reset;
        let mut streams = self.inner.streams.lock().await;
        if is_reset {
            if let Some(stream) = streams.remove(&stream_id) {
                let _ = stream.cancel.send(true);
            }
            return Ok(());
        }
        if let Some(stream) = streams.get(&stream_id) {
            if stream.sender.try_send(frame).is_ok() {
                return Ok(());
            }
            if let Some(stream) = streams.remove(&stream_id) {
                let _ = stream.cancel.send(true);
            }
            drop(streams);
            return self
                .reject(
                    stream_id,
                    "stream_backpressure",
                    "Service stream stopped accepting frames",
                )
                .await;
        }
        drop(streams);
        self.reject(
            stream_id,
            "stream_not_found",
            "Service stream does not exist",
        )
        .await
    }

    pub async fn reset_all(&self) {
        self.inner.proxy_connected.store(false, Ordering::SeqCst);
        self.inner.transport_epoch.fetch_add(1, Ordering::SeqCst);
        let mut streams = self.inner.streams.lock().await;
        for (_, stream) in streams.drain() {
            let _ = stream.cancel.send(true);
        }
    }

    pub async fn probe(
        &self,
        host: String,
        port: u16,
        target_agent_id: Option<String>,
        timeout_ms: u64,
    ) -> serde_json::Value {
        let timeout = std::time::Duration::from_millis(timeout_ms.clamp(100, 30_000));
        match tokio::time::timeout(
            timeout,
            connect_destination(&host, port, target_agent_id.as_deref()),
        )
        .await
        {
            Ok(Ok(_)) => serde_json::json!({ "healthy": true, "host": host, "port": port }),
            Ok(Err(error)) => {
                serde_json::json!({ "healthy": false, "host": host, "port": port, "error": error.to_string() })
            }
            Err(_) => {
                serde_json::json!({ "healthy": false, "host": host, "port": port, "error": "connection timed out" })
            }
        }
    }

    async fn spawn_destination(&self, frame: NetworkBinaryFrame) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.inner.proxy_connected.load(Ordering::SeqCst),
            "service transport offline"
        );
        let request: NetworkConnectRequest =
            serde_json::from_slice(&frame.payload).context("invalid service connect request")?;
        let stream_id = frame.stream_id;
        let mut streams = self.inner.streams.lock().await;
        if streams.contains_key(&stream_id) || streams.len() >= MAX_STREAMS {
            drop(streams);
            return self
                .reject(
                    stream_id,
                    "stream_unavailable",
                    "Service stream already exists or limit reached",
                )
                .await;
        }
        let (sender, incoming) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        let (cancel, mut cancelled) = watch::channel(false);
        streams.insert(
            stream_id.clone(),
            Stream {
                sender: sender.clone(),
                cancel,
            },
        );
        let runtime = Self {
            inner: self.inner.clone(),
            epoch: Some(self.inner.transport_epoch.load(Ordering::SeqCst)),
        };
        drop(streams);
        tokio::spawn(async move {
            let transfer = async {
                let socket = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    connect_destination(
                        &request.host,
                        request.port,
                        request.destination_agent_id.as_deref(),
                    ),
                )
                .await
                .context("service connection timed out")??;
                runtime
                    .send(NetworkBinaryFrame {
                        kind: NetworkBinaryKind::Opened,
                        stream_id: stream_id.clone(),
                        payload: Vec::new(),
                    })
                    .await?;
                bridge_stream(socket, incoming, &runtime, &stream_id).await
            };
            let result = tokio::select! {
                result = transfer => result,
                _ = cancelled.changed() => return,
            };
            // A previous connection must never remove a new connection's stream.
            if runtime.epoch == Some(runtime.inner.transport_epoch.load(Ordering::SeqCst)) {
                let mut streams = runtime.inner.streams.lock().await;
                if !streams
                    .get(&stream_id)
                    .is_some_and(|stream| stream.sender.same_channel(&sender))
                {
                    return;
                }
                streams.remove(&stream_id);
                drop(streams);
                let (code, message) = match result {
                    Ok(()) => ("stream_closed", "Service stream completed".to_string()),
                    Err(error) => ("stream_failed", error.to_string()),
                };
                let _ = runtime.reject(stream_id, code, &message).await;
            }
        });
        Ok(())
    }

    async fn reject(&self, stream_id: String, code: &str, message: &str) -> anyhow::Result<()> {
        self.send(NetworkBinaryFrame {
            kind: NetworkBinaryKind::Reset,
            stream_id,
            payload: serde_json::to_vec(&ProtocolError::new(code, message))?,
        })
        .await
    }

    async fn send(&self, frame: NetworkBinaryFrame) -> anyhow::Result<()> {
        let epoch = self
            .epoch
            .unwrap_or_else(|| self.inner.transport_epoch.load(Ordering::SeqCst));
        anyhow::ensure!(
            self.inner.proxy_connected.load(Ordering::SeqCst)
                && epoch == self.inner.transport_epoch.load(Ordering::SeqCst),
            "service transport offline or superseded"
        );
        self.inner
            .outgoing
            .send((epoch, frame))
            .await
            .map_err(|_| anyhow!("service transport closed"))
    }
}

pub(crate) trait DestinationStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> DestinationStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

async fn connect_destination(
    host: &str,
    port: u16,
    _target_agent_id: Option<&str>,
) -> anyhow::Result<Box<dyn DestinationStream>> {
    // Agent-owned endpoint registration denotes ownership, not an OS namespace.
    let socket = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("failed to connect to service {host}:{port}"))?;
    Ok(Box::new(socket))
}

pub(crate) async fn connect_agent_service(
    _agent_id: &str,
    port: u16,
) -> anyhow::Result<Box<dyn DestinationStream>> {
    connect_destination("127.0.0.1", port, None).await
}

async fn bridge_stream<S>(
    socket: S,
    mut incoming: mpsc::Receiver<NetworkBinaryFrame>,
    runtime: &NetworkRuntime,
    stream_id: &str,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(socket);
    let mut buffer = vec![0_u8; MAX_CHUNK];
    let mut send_window = INITIAL_WINDOW;
    let mut local_closed = false;
    let mut remote_closed = false;
    while !local_closed || !remote_closed {
        tokio::select! {
            read = reader.read(&mut buffer[..send_window.min(MAX_CHUNK)]), if !local_closed && send_window > 0 => {
                let count = read?;
                let (kind, payload) = if count == 0 {
                    local_closed = true;
                    (NetworkBinaryKind::HalfClose, Vec::new())
                } else {
                    send_window -= count;
                    (NetworkBinaryKind::Data, buffer[..count].to_vec())
                };
                runtime.send(NetworkBinaryFrame { kind, stream_id: stream_id.to_string(), payload }).await?;
            }
            frame = incoming.recv() => {
                let frame = frame.ok_or_else(|| anyhow!("service stream receiver closed"))?;
                match frame.kind {
                    NetworkBinaryKind::Data => {
                        anyhow::ensure!(!remote_closed && frame.payload.len() <= MAX_CHUNK, "invalid service data frame");
                        writer.write_all(&frame.payload).await?;
                        runtime.send(NetworkBinaryFrame {
                            kind: NetworkBinaryKind::WindowUpdate, stream_id: stream_id.to_string(),
                            payload: (frame.payload.len() as u32).to_be_bytes().to_vec(),
                        }).await?;
                    }
                    NetworkBinaryKind::WindowUpdate => {
                        let bytes: [u8; 4] = frame.payload.as_slice().try_into().map_err(|_| anyhow!("invalid service window update"))?;
                        let credit = u32::from_be_bytes(bytes) as usize;
                        anyhow::ensure!(credit > 0 && credit <= INITIAL_WINDOW - send_window, "invalid service window credit");
                        send_window += credit;
                    }
                    NetworkBinaryKind::HalfClose => {
                        if !remote_closed { writer.shutdown().await?; remote_closed = true; }
                    }
                    NetworkBinaryKind::Reset => return Err(anyhow!("service stream reset")),
                    _ => return Err(anyhow!("unexpected service stream frame {:?}", frame.kind)),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
