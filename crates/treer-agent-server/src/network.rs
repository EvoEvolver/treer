use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};
use treer_protocol::{
    NetworkBinaryFrame, NetworkBinaryKind, NetworkConnectRequest, NetworkDirectTarget,
    NetworkOpenRequest, ProtocolError,
};
use uuid::Uuid;

const STREAM_CHANNEL_CAPACITY: usize = 32;
const OUTGOING_CHANNEL_CAPACITY: usize = 128;
const INITIAL_WINDOW: usize = 256 * 1024;
const MAX_CHUNK: usize = 16 * 1024;
pub const SANDBOX_LOCAL_API_IP: &str = "192.0.2.1";

#[derive(Clone)]
pub struct NetworkRuntime {
    inner: Arc<NetworkInner>,
}

struct NetworkInner {
    listen_address: SocketAddr,
    local_api_address: SocketAddr,
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
                local_api_address: api_address,
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
        if route.destination == SANDBOX_LOCAL_API_IP
            && route.port == self.inner.local_api_address.port()
        {
            let local_api_address = self.inner.local_api_address;
            tokio::spawn(async move {
                if let Err(error) = bridge_local_api(socket, local_api_address).await {
                    debug!(%error, "local agent API stream closed");
                }
            });
            return;
        }
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
                source_agent_id: route.source_agent_id,
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

async fn bridge_local_api(mut socket: TcpStream, address: SocketAddr) -> anyhow::Result<()> {
    let mut local_api = match TcpStream::connect(address).await {
        Ok(socket) => socket,
        Err(error) => {
            let _ = write_socks_reply(&mut socket, 0x05).await;
            return Err(error).with_context(|| format!("failed to connect to local API {address}"));
        }
    };
    write_socks_reply(&mut socket, 0).await?;
    tokio::io::copy_bidirectional(&mut socket, &mut local_api).await?;
    Ok(())
}

struct SocksRoute {
    destination: String,
    host: String,
    port: u16,
    source_agent_id: Option<String>,
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
    let source_agent_id = if methods.contains(&2) {
        socket.write_all(&[5, 2]).await?;
        Some(read_socks_username(socket).await?)
    } else if methods.contains(&0) {
        socket.write_all(&[5, 0]).await?;
        None
    } else {
        socket.write_all(&[5, 0xff]).await?;
        return Err(anyhow!(
            "SOCKS client does not offer a supported authentication method"
        ));
    };

    let mut request = [0_u8; 4];
    socket.read_exact(&mut request).await?;
    if request[0] != 5 || request[1] != 1 {
        return Err(anyhow!("only SOCKS5 CONNECT is supported"));
    }
    let destination = match request[3] {
        3 => {
            let length = socket.read_u8().await?;
            let mut value = vec![0_u8; usize::from(length)];
            socket.read_exact(&mut value).await?;
            String::from_utf8(value).context("SOCKS destination is not UTF-8")?
        }
        1 => {
            let mut value = [0_u8; 4];
            socket.read_exact(&mut value).await?;
            Ipv4Addr::from(value).to_string()
        }
        4 => {
            let mut value = [0_u8; 16];
            socket.read_exact(&mut value).await?;
            Ipv6Addr::from(value).to_string()
        }
        _ => return Err(anyhow!("unsupported SOCKS destination type")),
    };
    let port = socket.read_u16().await?;
    let mut route = parse_route(&destination, port)?;
    route.source_agent_id = source_agent_id;
    Ok(route)
}

async fn read_socks_username(socket: &mut TcpStream) -> anyhow::Result<String> {
    let version = socket.read_u8().await?;
    let username_len = socket.read_u8().await?;
    let mut username = vec![0_u8; usize::from(username_len)];
    socket.read_exact(&mut username).await?;
    let password_len = socket.read_u8().await?;
    let mut password = vec![0_u8; usize::from(password_len)];
    socket.read_exact(&mut password).await?;
    if version != 1 || username.is_empty() || password != b"treer" {
        socket.write_all(&[1, 1]).await?;
        return Err(anyhow!("invalid Treer SOCKS agent identity"));
    }
    let username = String::from_utf8(username).context("SOCKS username is not UTF-8")?;
    socket.write_all(&[1, 0]).await?;
    Ok(username)
}

fn parse_route(domain: &str, port: u16) -> anyhow::Result<SocksRoute> {
    let route = domain.trim_end_matches('.').to_ascii_lowercase();
    if route.is_empty() || route.len() > 253 {
        return Err(anyhow!("Treer destination hostname is invalid"));
    }
    Ok(SocksRoute {
        destination: route.clone(),
        host: route,
        port,
        source_agent_id: None,
    })
}

async fn source_stream(
    mut socket: TcpStream,
    mut incoming: mpsc::Receiver<NetworkBinaryFrame>,
    runtime: &NetworkRuntime,
    stream_id: &str,
) -> anyhow::Result<()> {
    let route = incoming
        .recv()
        .await
        .ok_or_else(|| anyhow!("network stream closed before open"))?;
    match route.kind {
        NetworkBinaryKind::Direct => {
            let target: NetworkDirectTarget = match serde_json::from_slice(&route.payload) {
                Ok(target) => target,
                Err(error) => {
                    write_socks_reply(&mut socket, 0x05).await?;
                    return Err(error).context("invalid direct network target");
                }
            };
            if target.host.is_empty() || target.port == 0 {
                write_socks_reply(&mut socket, 0x05).await?;
                return Err(anyhow!("direct network target is invalid"));
            }
            let mut destination =
                match TcpStream::connect((target.host.as_str(), target.port)).await {
                    Ok(socket) => socket,
                    Err(error) => {
                        write_socks_reply(&mut socket, 0x05).await?;
                        return Err(error).with_context(|| {
                            format!(
                                "failed to connect directly to {}:{}",
                                target.host, target.port
                            )
                        });
                    }
                };
            write_socks_reply(&mut socket, 0).await?;
            tokio::io::copy_bidirectional(&mut socket, &mut destination)
                .await
                .context("direct network stream failed")?;
            Ok(())
        }
        NetworkBinaryKind::Opened => {
            let result = async {
                write_socks_reply(&mut socket, 0).await?;
                bridge_stream(socket, incoming, runtime, stream_id, INITIAL_WINDOW).await
            }
            .await;
            if let Err(error) = &result {
                let _ = runtime
                    .send(NetworkBinaryFrame {
                        kind: NetworkBinaryKind::Reset,
                        stream_id: stream_id.to_string(),
                        payload: encode_error("stream_error", &error.to_string()),
                    })
                    .await;
            }
            result
        }
        NetworkBinaryKind::Reset => {
            write_socks_reply(&mut socket, 0x05).await?;
            Err(decode_reset(&route))
        }
        _ => {
            write_socks_reply(&mut socket, 0x05).await?;
            let error = anyhow!("unexpected network route frame {:?}", route.kind);
            let _ = runtime
                .send(NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Reset,
                    stream_id: stream_id.to_string(),
                    payload: encode_error("invalid_network_route", &error.to_string()),
                })
                .await;
            Err(error)
        }
    }
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
                    NetworkBinaryKind::Open | NetworkBinaryKind::Opened | NetworkBinaryKind::Direct => {
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
    use std::time::Duration;

    use super::*;

    async fn next_frame(runtime: &NetworkRuntime) -> NetworkBinaryFrame {
        tokio::time::timeout(Duration::from_secs(2), runtime.next_outgoing())
            .await
            .expect("network frame timeout")
            .expect("network runtime stopped")
    }

    async fn request_ipv4(client: &mut TcpStream, address: Ipv4Addr, port: u16) {
        client.write_all(&[5, 1, 0]).await.expect("SOCKS greeting");
        let mut method = [0_u8; 2];
        client.read_exact(&mut method).await.expect("SOCKS method");
        assert_eq!(method, [5, 0]);
        client
            .write_all(&[5, 1, 0, 1])
            .await
            .expect("SOCKS connect header");
        client
            .write_all(&address.octets())
            .await
            .expect("SOCKS connect address");
        client
            .write_all(&port.to_be_bytes())
            .await
            .expect("SOCKS connect port");
    }

    #[test]
    fn parses_domains_without_implicit_machine_routes() {
        let virtual_host = parse_route("API.Internal.", 80).expect("virtual host route");
        assert_eq!(virtual_host.destination, "api.internal");
        assert_eq!(virtual_host.host, "api.internal");
        let ordinary = parse_route("github.com", 443).expect("ordinary route");
        assert_eq!(ordinary.destination, "github.com");
        assert_eq!(ordinary.host, "github.com");
    }

    #[tokio::test]
    async fn socks_username_becomes_the_source_agent_identity() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("test listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept SOCKS client");
            read_socks_request(&mut socket)
                .await
                .expect("SOCKS request")
        });
        let mut client = TcpStream::connect(address)
            .await
            .expect("connect SOCKS server");
        client.write_all(&[5, 1, 2]).await.expect("greeting");
        let mut method = [0_u8; 2];
        client.read_exact(&mut method).await.expect("method");
        assert_eq!(method, [5, 2]);
        client
            .write_all(&[
                1, 7, b'a', b'g', b'e', b'n', b't', b'-', b'a', 5, b't', b'r', b'e', b'e', b'r',
            ])
            .await
            .expect("username authentication");
        let mut auth = [0_u8; 2];
        client.read_exact(&mut auth).await.expect("auth response");
        assert_eq!(auth, [1, 0]);
        client
            .write_all(&[
                5, 1, 0, 3, 12, b'a', b'p', b'i', b'.', b'i', b'n', b't', b'e', b'r', b'n', b'a',
                b'l', 0, 80,
            ])
            .await
            .expect("connect request");
        let route = server.await.expect("SOCKS server task");
        assert_eq!(route.destination, "api.internal");
        assert_eq!(route.source_agent_id.as_deref(), Some("agent-a"));
    }

    #[tokio::test]
    async fn sandbox_local_api_stays_on_the_source_machine() {
        let local_api = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind local API");
        let local_api_address = local_api.local_addr().expect("local API address");
        let local_api_task = tokio::spawn(async move {
            let (mut socket, _) = local_api.accept().await.expect("accept local API request");
            let mut request = [0_u8; 18];
            socket
                .read_exact(&mut request)
                .await
                .expect("read local API request");
            assert_eq!(&request, b"GET / HTTP/1.0\r\n\r\n");
            socket
                .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("write local API response");
        });
        let runtime = NetworkRuntime::bind_near(local_api_address)
            .await
            .expect("bind network runtime");
        let mut client = TcpStream::connect(runtime.listen_address())
            .await
            .expect("connect SOCKS client");
        request_ipv4(
            &mut client,
            SANDBOX_LOCAL_API_IP.parse().expect("sandbox local API IP"),
            local_api_address.port(),
        )
        .await;
        let mut reply = [0_u8; 10];
        client.read_exact(&mut reply).await.expect("SOCKS reply");
        assert_eq!(reply[1], 0);

        client
            .write_all(b"GET / HTTP/1.0\r\n\r\n")
            .await
            .expect("write local API request");
        client.shutdown().await.expect("finish local API request");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .expect("local API response timeout")
            .expect("read local API response");
        assert!(response.ends_with(b"\r\n\r\nok"));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), runtime.next_outgoing())
                .await
                .is_err()
        );
        local_api_task.await.expect("local API task");
    }

    #[tokio::test]
    async fn direct_route_bridges_locally_without_proxy_data_frames() {
        let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind target server");
        let target_port = target.local_addr().expect("target address").port();
        let target_task = tokio::spawn(async move {
            let (mut socket, _) = target.accept().await.expect("accept target connection");
            let mut request = [0_u8; 18];
            socket
                .read_exact(&mut request)
                .await
                .expect("read target request");
            assert_eq!(&request, b"GET / HTTP/1.0\r\n\r\n");
            socket
                .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("write target response");
        });

        let api_reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve API port");
        let api_address = api_reservation.local_addr().expect("API address");
        drop(api_reservation);
        let runtime = NetworkRuntime::bind_near(api_address)
            .await
            .expect("bind network runtime");
        let mut client = TcpStream::connect(runtime.listen_address())
            .await
            .expect("connect SOCKS client");
        client.write_all(&[5, 1, 0]).await.expect("SOCKS greeting");
        let mut method = [0_u8; 2];
        client.read_exact(&mut method).await.expect("SOCKS method");
        assert_eq!(method, [5, 0]);
        client
            .write_all(&[
                5, 1, 0, 3, 11, b'd', b'i', b'r', b'e', b'c', b't', b'.', b't', b'e', b's', b't',
                0, 80,
            ])
            .await
            .expect("SOCKS connect request");

        let source_open = next_frame(&runtime).await;
        assert_eq!(source_open.kind, NetworkBinaryKind::Open);
        assert_eq!(
            serde_json::from_slice::<NetworkOpenRequest>(&source_open.payload)
                .expect("decode network open")
                .destination,
            "direct.test"
        );
        runtime
            .handle_incoming(NetworkBinaryFrame {
                kind: NetworkBinaryKind::Direct,
                stream_id: source_open.stream_id,
                payload: serde_json::to_vec(&NetworkDirectTarget {
                    host: Ipv4Addr::LOCALHOST.to_string(),
                    port: target_port,
                })
                .expect("encode direct target"),
            })
            .await
            .expect("apply direct route");

        let mut reply = [0_u8; 10];
        client.read_exact(&mut reply).await.expect("SOCKS reply");
        assert_eq!(reply[1], 0);
        client
            .write_all(b"GET / HTTP/1.0\r\n\r\n")
            .await
            .expect("write HTTP request");
        client.shutdown().await.expect("half-close HTTP request");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .expect("HTTP response timeout")
            .expect("read HTTP response");
        assert!(response.ends_with(b"\r\n\r\nok"));

        target_task.await.expect("target task");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), runtime.next_outgoing())
                .await
                .is_err(),
            "direct traffic must not produce Proxy data frames"
        );
    }

    #[tokio::test]
    async fn distinct_leg_ids_support_same_machine_tcp_round_trip() {
        let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind target server");
        let target_port = target.local_addr().expect("target address").port();
        let target_task = tokio::spawn(async move {
            let (mut socket, _) = target.accept().await.expect("accept target connection");
            let mut request = [0_u8; 18];
            socket
                .read_exact(&mut request)
                .await
                .expect("read target request");
            assert_eq!(&request, b"GET / HTTP/1.0\r\n\r\n");
            socket
                .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("write target response");
        });

        let api_reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve API port");
        let api_address = api_reservation.local_addr().expect("API address");
        drop(api_reservation);
        let runtime = NetworkRuntime::bind_near(api_address)
            .await
            .expect("bind network runtime");
        let mut client = TcpStream::connect(runtime.listen_address())
            .await
            .expect("connect SOCKS client");
        client.write_all(&[5, 1, 0]).await.expect("SOCKS greeting");
        let mut method = [0_u8; 2];
        client.read_exact(&mut method).await.expect("SOCKS method");
        assert_eq!(method, [5, 0]);
        client
            .write_all(&[
                5, 1, 0, 3, 8, b't', b'e', b's', b't', b'.', b'a', b'p', b'i', 0, 80,
            ])
            .await
            .expect("SOCKS connect request");

        let source_open = next_frame(&runtime).await;
        assert_eq!(source_open.kind, NetworkBinaryKind::Open);
        let source_stream_id = source_open.stream_id;
        let destination_stream_id = "net_destination".to_string();
        assert_ne!(source_stream_id, destination_stream_id);
        runtime
            .handle_incoming(NetworkBinaryFrame {
                kind: NetworkBinaryKind::Open,
                stream_id: destination_stream_id.clone(),
                payload: serde_json::to_vec(&NetworkConnectRequest {
                    source_server_id: "server".to_string(),
                    source_agent_id: None,
                    host: Ipv4Addr::LOCALHOST.to_string(),
                    port: target_port,
                })
                .expect("encode destination request"),
            })
            .await
            .expect("open destination leg");
        let mut destination_opened = next_frame(&runtime).await;
        assert_eq!(destination_opened.kind, NetworkBinaryKind::Opened);
        assert_eq!(destination_opened.stream_id, destination_stream_id);
        destination_opened.stream_id.clone_from(&source_stream_id);
        runtime
            .handle_incoming(destination_opened)
            .await
            .expect("open source leg");

        let mut reply = [0_u8; 10];
        client.read_exact(&mut reply).await.expect("SOCKS reply");
        assert_eq!(reply[1], 0);

        let relay_runtime = runtime.clone();
        let relay_source_id = source_stream_id.clone();
        let relay_destination_id = destination_stream_id.clone();
        let relay = tokio::spawn(async move {
            loop {
                let mut frame = next_frame(&relay_runtime).await;
                if frame.stream_id == relay_source_id {
                    frame.stream_id.clone_from(&relay_destination_id);
                } else if frame.stream_id == relay_destination_id {
                    frame.stream_id.clone_from(&relay_source_id);
                } else {
                    panic!("unexpected network stream {}", frame.stream_id);
                }
                relay_runtime
                    .handle_incoming(frame)
                    .await
                    .expect("relay same-machine frame");
            }
        });

        client
            .write_all(b"GET / HTTP/1.0\r\n\r\n")
            .await
            .expect("write HTTP request");
        client.shutdown().await.expect("half-close HTTP request");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .expect("HTTP response timeout")
            .expect("read HTTP response");
        assert!(response.ends_with(b"\r\n\r\nok"));

        target_task.await.expect("target task");
        relay.abort();
    }
}
