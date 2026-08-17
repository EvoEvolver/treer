use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};
use treer_protocol::{
    NetworkBinaryFrame, NetworkBinaryKind, NetworkConnectRequest, NetworkOpenRequest, ProtocolError,
};
use uuid::Uuid;

const STREAM_CHANNEL_CAPACITY: usize = 32;
const OUTGOING_CHANNEL_CAPACITY: usize = 128;
const INITIAL_WINDOW: usize = 256 * 1024;
const MAX_CHUNK: usize = 16 * 1024;

#[derive(Clone)]
pub struct NetworkRuntime {
    inner: Arc<NetworkInner>,
}

struct NetworkInner {
    listen_address: SocketAddr,
    outgoing: mpsc::Sender<NetworkBinaryFrame>,
    outgoing_rx: Mutex<mpsc::Receiver<NetworkBinaryFrame>>,
    streams: Mutex<HashMap<String, mpsc::Sender<NetworkBinaryFrame>>>,
}

impl NetworkRuntime {
    pub async fn bind_near(api_address: SocketAddr) -> anyhow::Result<Self> {
        let listener = bind_near(api_address).await?;
        let listen_address = listener.local_addr()?;
        let (outgoing, outgoing_rx) = mpsc::channel(OUTGOING_CHANNEL_CAPACITY);
        let runtime = Self {
            inner: Arc::new(NetworkInner {
                listen_address,
                outgoing,
                outgoing_rx: Mutex::new(outgoing_rx),
                streams: Mutex::new(HashMap::new()),
            }),
        };
        let accept_runtime = runtime.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => accept_runtime.spawn_source(stream).await,
                    Err(error) => {
                        warn!(%error, "network proxy accept failed");
                        break;
                    }
                }
            }
        });
        Ok(runtime)
    }

    pub fn listen_address(&self) -> SocketAddr {
        self.inner.listen_address
    }

    pub fn proxy_url(&self) -> String {
        format!("socks5h://{}", self.inner.listen_address)
    }

    pub async fn next_outgoing(&self) -> Option<NetworkBinaryFrame> {
        self.inner.outgoing_rx.lock().await.recv().await
    }

    pub async fn handle_incoming(&self, frame: NetworkBinaryFrame) -> anyhow::Result<()> {
        if frame.kind == NetworkBinaryKind::Open {
            return self.spawn_destination(frame).await;
        }
        let sender = self
            .inner
            .streams
            .lock()
            .await
            .get(&frame.stream_id)
            .cloned();
        if let Some(sender) = sender {
            sender
                .send(frame)
                .await
                .map_err(|_| anyhow!("network stream closed"))?;
        } else if frame.kind != NetworkBinaryKind::Reset {
            self.send(NetworkBinaryFrame {
                kind: NetworkBinaryKind::Reset,
                stream_id: frame.stream_id,
                payload: encode_error("stream_not_found", "network stream does not exist"),
            })
            .await?;
        }
        Ok(())
    }

    pub async fn reset_all(&self) {
        let streams = self
            .inner
            .streams
            .lock()
            .await
            .drain()
            .map(|(_, sender)| sender)
            .collect::<Vec<_>>();
        for sender in streams {
            let _ = sender
                .send(NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Reset,
                    stream_id: "disconnected".to_string(),
                    payload: encode_error("proxy_disconnected", "proxy connection was lost"),
                })
                .await;
        }
    }

    async fn spawn_source(&self, mut socket: TcpStream) {
        let route = match read_socks_request(&mut socket).await {
            Ok(route) => route,
            Err(error) => {
                debug!(%error, "rejected local SOCKS request");
                let _ = write_socks_reply(&mut socket, 0x01).await;
                return;
            }
        };
        let stream_id = format!("net_{}", Uuid::new_v4().simple());
        let (incoming, incoming_rx) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        self.inner
            .streams
            .lock()
            .await
            .insert(stream_id.clone(), incoming);
        let runtime = self.clone();
        tokio::spawn(async move {
            let request = NetworkOpenRequest {
                destination: route.destination,
                host: route.host,
                port: route.port,
                source_agent_id: None,
            };
            let result = async {
                runtime
                    .send(NetworkBinaryFrame {
                        kind: NetworkBinaryKind::Open,
                        stream_id: stream_id.clone(),
                        payload: serde_json::to_vec(&request)?,
                    })
                    .await?;
                source_stream(socket, incoming_rx, &runtime, &stream_id).await
            }
            .await;
            runtime.inner.streams.lock().await.remove(&stream_id);
            if let Err(error) = result {
                debug!(stream_id, %error, "source network stream closed");
                let _ = runtime
                    .send(NetworkBinaryFrame {
                        kind: NetworkBinaryKind::Reset,
                        stream_id,
                        payload: encode_error("stream_error", &error.to_string()),
                    })
                    .await;
            }
        });
    }

    async fn spawn_destination(&self, frame: NetworkBinaryFrame) -> anyhow::Result<()> {
        let request: NetworkConnectRequest =
            serde_json::from_slice(&frame.payload).context("invalid network connect request")?;
        let stream_id = frame.stream_id;
        if self.inner.streams.lock().await.contains_key(&stream_id) {
            self.send(NetworkBinaryFrame {
                kind: NetworkBinaryKind::Reset,
                stream_id,
                payload: encode_error("stream_exists", "network stream already exists"),
            })
            .await?;
            return Ok(());
        }
        let (incoming, incoming_rx) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        self.inner
            .streams
            .lock()
            .await
            .insert(stream_id.clone(), incoming);
        let runtime = self.clone();
        tokio::spawn(async move {
            let result = async {
                let socket = TcpStream::connect((request.host.as_str(), request.port))
                    .await
                    .with_context(|| {
                        format!("failed to connect to {}:{}", request.host, request.port)
                    })?;
                runtime
                    .send(NetworkBinaryFrame {
                        kind: NetworkBinaryKind::Opened,
                        stream_id: stream_id.clone(),
                        payload: Vec::new(),
                    })
                    .await?;
                bridge_stream(socket, incoming_rx, &runtime, &stream_id, INITIAL_WINDOW).await
            }
            .await;
            runtime.inner.streams.lock().await.remove(&stream_id);
            if let Err(error) = result {
                debug!(stream_id, %error, "destination network stream closed");
                let _ = runtime
                    .send(NetworkBinaryFrame {
                        kind: NetworkBinaryKind::Reset,
                        stream_id,
                        payload: encode_error("connect_failed", &error.to_string()),
                    })
                    .await;
            }
        });
        Ok(())
    }

    async fn send(&self, frame: NetworkBinaryFrame) -> anyhow::Result<()> {
        self.inner
            .outgoing
            .send(frame)
            .await
            .map_err(|_| anyhow!("network transport closed"))
    }
}

struct SocksRoute {
    destination: String,
    host: String,
    port: u16,
}

async fn bind_near(api_address: SocketAddr) -> io::Result<TcpListener> {
    let ip = if api_address.ip().is_loopback() {
        api_address.ip()
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    let first = api_address.port().saturating_add(1);
    if first > 0 {
        for port in first..=first.saturating_add(20) {
            match TcpListener::bind(SocketAddr::new(ip, port)).await {
                Ok(listener) => return Ok(listener),
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
                Err(error) => return Err(error),
            }
        }
    }
    TcpListener::bind(SocketAddr::new(ip, 0)).await
}

async fn read_socks_request(socket: &mut TcpStream) -> anyhow::Result<SocksRoute> {
    let mut greeting = [0_u8; 2];
    socket.read_exact(&mut greeting).await?;
    if greeting[0] != 5 {
        return Err(anyhow!("only SOCKS5 is supported"));
    }
    let mut methods = vec![0_u8; usize::from(greeting[1])];
    socket.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        socket.write_all(&[5, 0xff]).await?;
        return Err(anyhow!(
            "SOCKS client does not allow unauthenticated access"
        ));
    }
    socket.write_all(&[5, 0]).await?;

    let mut request = [0_u8; 4];
    socket.read_exact(&mut request).await?;
    if request[0] != 5 || request[1] != 1 {
        return Err(anyhow!("only SOCKS5 CONNECT is supported"));
    }
    let domain = match request[3] {
        3 => {
            let length = socket.read_u8().await?;
            let mut value = vec![0_u8; usize::from(length)];
            socket.read_exact(&mut value).await?;
            String::from_utf8(value).context("SOCKS destination is not UTF-8")?
        }
        _ => return Err(anyhow!("Treer destinations must use a hostname")),
    };
    let port = socket.read_u16().await?;
    parse_route(&domain, port)
}

fn parse_route(domain: &str, port: u16) -> anyhow::Result<SocksRoute> {
    let route = domain.trim_end_matches('.').to_ascii_lowercase();
    if route.is_empty() || route.len() > 253 {
        return Err(anyhow!("Treer destination hostname is invalid"));
    }
    let (host, destination) = match route
        .strip_suffix(".treer")
        .and_then(|route| route.rsplit_once(".via."))
    {
        Some((host, destination)) if !host.is_empty() && !destination.is_empty() => {
            (host.to_string(), destination.to_string())
        }
        Some(_) => return Err(anyhow!("invalid Treer via hostname")),
        None => ("127.0.0.1".to_string(), route),
    };
    Ok(SocksRoute {
        destination,
        host,
        port,
    })
}

async fn source_stream(
    mut socket: TcpStream,
    mut incoming: mpsc::Receiver<NetworkBinaryFrame>,
    runtime: &NetworkRuntime,
    stream_id: &str,
) -> anyhow::Result<()> {
    let opened = incoming
        .recv()
        .await
        .ok_or_else(|| anyhow!("network stream closed before open"))?;
    if opened.kind != NetworkBinaryKind::Opened {
        write_socks_reply(&mut socket, 0x05).await?;
        return Err(decode_reset(&opened));
    }
    write_socks_reply(&mut socket, 0).await?;
    bridge_stream(socket, incoming, runtime, stream_id, INITIAL_WINDOW).await
}

async fn write_socks_reply(socket: &mut TcpStream, status: u8) -> io::Result<()> {
    socket.write_all(&[5, status, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

async fn bridge_stream(
    socket: TcpStream,
    mut incoming: mpsc::Receiver<NetworkBinaryFrame>,
    runtime: &NetworkRuntime,
    stream_id: &str,
    mut send_window: usize,
) -> anyhow::Result<()> {
    let (mut reader, mut writer) = socket.into_split();
    let mut buffer = vec![0_u8; MAX_CHUNK];
    let mut local_closed = false;
    let mut remote_closed = false;
    while !local_closed || !remote_closed {
        tokio::select! {
            read = reader.read(&mut buffer[..send_window.min(MAX_CHUNK)]), if !local_closed && send_window > 0 => {
                let read = read?;
                if read == 0 {
                    local_closed = true;
                    runtime.send(NetworkBinaryFrame {
                        kind: NetworkBinaryKind::HalfClose,
                        stream_id: stream_id.to_string(),
                        payload: Vec::new(),
                    }).await?;
                } else {
                    send_window -= read;
                    runtime.send(NetworkBinaryFrame {
                        kind: NetworkBinaryKind::Data,
                        stream_id: stream_id.to_string(),
                        payload: buffer[..read].to_vec(),
                    }).await?;
                }
            }
            frame = incoming.recv() => {
                let frame = frame.ok_or_else(|| anyhow!("network stream receiver closed"))?;
                match frame.kind {
                    NetworkBinaryKind::Data => {
                        writer.write_all(&frame.payload).await?;
                        runtime.send(NetworkBinaryFrame {
                            kind: NetworkBinaryKind::WindowUpdate,
                            stream_id: stream_id.to_string(),
                            payload: u32::try_from(frame.payload.len()).unwrap_or(u32::MAX).to_be_bytes().to_vec(),
                        }).await?;
                    }
                    NetworkBinaryKind::WindowUpdate => {
                        let bytes: [u8; 4] = frame.payload.as_slice().try_into()
                            .map_err(|_| anyhow!("invalid network window update"))?;
                        send_window = send_window.saturating_add(u32::from_be_bytes(bytes) as usize);
                    }
                    NetworkBinaryKind::HalfClose => {
                        if !remote_closed {
                            writer.shutdown().await?;
                            remote_closed = true;
                        }
                    }
                    NetworkBinaryKind::Reset => return Err(decode_reset(&frame)),
                    NetworkBinaryKind::Open | NetworkBinaryKind::Opened => {
                        return Err(anyhow!("unexpected network stream frame {:?}", frame.kind));
                    }
                }
            }
        }
    }
    Ok(())
}

fn encode_error(code: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&ProtocolError::new(code, message)).unwrap_or_default()
}

fn decode_reset(frame: &NetworkBinaryFrame) -> anyhow::Error {
    serde_json::from_slice::<ProtocolError>(&frame.payload)
        .map(|error| anyhow!("{}: {}", error.code, error.message))
        .unwrap_or_else(|_| anyhow!("network stream was reset"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_and_via_routes() {
        let local = parse_route("build-machine.treer", 8080).expect("local route");
        assert_eq!(local.destination, "build-machine.treer");
        assert_eq!(local.host, "127.0.0.1");
        let via = parse_route("git.internal.via.build-machine.treer", 22).expect("via route");
        assert_eq!(via.destination, "build-machine");
        assert_eq!(via.host, "git.internal");
        let virtual_host = parse_route("API.Internal.", 80).expect("virtual host route");
        assert_eq!(virtual_host.destination, "api.internal");
        assert_eq!(virtual_host.host, "127.0.0.1");
    }
}
