use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{anyhow, Context};
use base64::Engine;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(target_os = "linux")]
use tokio::net::UnixStream;
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

pub fn agent_service_socket_path(agent_id: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("treer-agent-service-{agent_id}.sock"))
}

#[derive(Clone)]
pub struct NetworkRuntime {
    inner: Arc<NetworkInner>,
}

struct NetworkInner {
    listen_address: SocketAddr,
    local_api_address: SocketAddr,
    outgoing: mpsc::Sender<(u64, NetworkBinaryFrame)>,
    outgoing_rx: Mutex<mpsc::Receiver<(u64, NetworkBinaryFrame)>>,
    proxy_connected: AtomicBool,
    transport_epoch: AtomicU64,
    streams: Mutex<HashMap<String, mpsc::Sender<NetworkBinaryFrame>>>,
    virtual_hosts: RwLock<HashSet<String>>,
    transparent_networking: bool,
    usage_outbox: OnceLock<Arc<crate::network_usage_outbox::UsageOutbox>>,
}

impl NetworkRuntime {
    pub async fn bind_near(
        api_address: SocketAddr,
        transparent_networking: bool,
    ) -> anyhow::Result<Self> {
        let listener = bind_near(api_address).await?;
        let listen_address = listener.local_addr()?;
        let (outgoing, outgoing_rx) = mpsc::channel(OUTGOING_CHANNEL_CAPACITY);
        let runtime = Self {
            inner: Arc::new(NetworkInner {
                listen_address,
                local_api_address: api_address,
                outgoing,
                outgoing_rx: Mutex::new(outgoing_rx),
                proxy_connected: AtomicBool::new(false),
                transport_epoch: AtomicU64::new(0),
                streams: Mutex::new(HashMap::new()),
                virtual_hosts: RwLock::new(HashSet::new()),
                transparent_networking,
                usage_outbox: OnceLock::new(),
            }),
        };
        let accept_runtime = runtime.clone();
        tokio::spawn(async move {
            let handshakes = Arc::new(tokio::sync::Semaphore::new(128));
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let Ok(permit) = handshakes.clone().try_acquire_owned() else {
                            continue;
                        };
                        let runtime = accept_runtime.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(15),
                                runtime.spawn_source(stream),
                            )
                            .await;
                        });
                    }
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

    pub async fn enable_usage_outbox(
        &self,
        directory: std::path::PathBuf,
        workspace: String,
        server: String,
    ) -> anyhow::Result<()> {
        let outbox = Arc::new(
            tokio::task::spawn_blocking(move || {
                crate::network_usage_outbox::UsageOutbox::open(directory, workspace, server)
            })
            .await??,
        );
        self.inner
            .usage_outbox
            .set(outbox.clone())
            .map_err(|_| anyhow!("usage outbox already configured"))?;
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                if !runtime.inner.proxy_connected.load(Ordering::SeqCst) {
                    continue;
                }
                for report in outbox.pending() {
                    if let Ok(payload) = serde_json::to_vec(&report) {
                        let _ = runtime
                            .send(NetworkBinaryFrame {
                                kind: NetworkBinaryKind::Usage,
                                stream_id: format!("usage_{}", report.ticket),
                                payload,
                            })
                            .await;
                    }
                }
            }
        });
        Ok(())
    }

    pub(crate) async fn report_usage(
        &self,
        stream_id: &str,
        ticket: Option<&str>,
        totals: treer_protocol::NetworkUsageTotals,
        finished: bool,
    ) -> anyhow::Result<()> {
        let payload = if let Some(ticket) = ticket {
            let report = treer_protocol::NetworkUsageReport {
                ticket: ticket.to_owned(),
                totals,
                finished,
            };
            let pending = if let Some(outbox) = self.inner.usage_outbox.get() {
                let outbox = outbox.clone();
                tokio::task::spawn_blocking(move || outbox.record(report)).await??
            } else {
                Some(report)
            };
            let Some(report) = pending else { return Ok(()) };
            serde_json::to_vec(&report)?
        } else {
            serde_json::to_vec(&totals)?
        };
        let sent = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.send(NetworkBinaryFrame {
                kind: NetworkBinaryKind::Usage,
                stream_id: stream_id.to_string(),
                payload,
            }),
        )
        .await;
        if ticket.is_some() && self.inner.usage_outbox.get().is_some() {
            return Ok(());
        }
        sent.context("usage transport stalled")?
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
        if frame.kind == NetworkBinaryKind::UsageAck {
            let report: treer_protocol::NetworkUsageReport =
                serde_json::from_slice(&frame.payload)?;
            if let Some(outbox) = self.inner.usage_outbox.get() {
                let outbox = outbox.clone();
                tokio::task::spawn_blocking(move || outbox.acknowledge(&report)).await??;
            }
            return Ok(());
        }
        if matches!(
            frame.kind,
            NetworkBinaryKind::Open | NetworkBinaryKind::OpenDatagram
        ) {
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

    pub fn set_virtual_hostnames(&self, hosts: impl IntoIterator<Item = impl AsRef<str>>) {
        let mut virtual_hosts = self
            .inner
            .virtual_hosts
            .write()
            .unwrap_or_else(|error| error.into_inner());
        virtual_hosts.clear();
        virtual_hosts.extend(
            hosts
                .into_iter()
                .map(|host| normalize_hostname(host.as_ref())),
        );
    }

    pub fn clear_virtual_hostnames(&self) {
        self.inner
            .virtual_hosts
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }

    pub async fn sync_native_virtual_hostnames(&self) -> anyhow::Result<()> {
        if !cfg!(target_os = "macos")
            || std::env::var("TREER_NETWORK_MODE").as_deref() != Ok("native-experimental")
        {
            return Ok(());
        }
        let names: Vec<String> = self
            .inner
            .virtual_hosts
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect();
        let helper = std::env::var_os("TREER_MACOS_NETWORK_HELPER")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "/Applications/TreerNetwork.app/Contents/MacOS/TreerNetwork".into());
        let input = serde_json::to_vec(&names)?;
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut child = tokio::process::Command::new(helper)
                .arg("sync-hosts")
                .arg("--network-proxy")
                .arg(self.proxy_url())
                .arg("--hosts-stdin")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;
            let mut stdin = child
                .stdin
                .take()
                .context("native DNS helper stdin unavailable")?;
            stdin.write_all(&input).await?;
            drop(stdin);
            let output = child.wait_with_output().await?;
            anyhow::ensure!(
                output.status.success(),
                "native DNS helper rejected snapshot: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("native DNS synchronization timed out")?
    }

    fn is_virtual_host(&self, host: &str) -> bool {
        self.inner
            .virtual_hosts
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains(&normalize_hostname(host))
    }

    pub async fn reset_all(&self) {
        self.inner.proxy_connected.store(false, Ordering::SeqCst);
        self.inner.transport_epoch.fetch_add(1, Ordering::SeqCst);
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
            connect_destination(
                &host,
                port,
                target_agent_id.as_deref(),
                self.inner.transparent_networking,
            ),
        )
        .await
        {
            Ok(Ok(_)) => serde_json::json!({ "healthy": true, "host": host, "port": port }),
            Ok(Err(error)) => serde_json::json!({
                "healthy": false,
                "host": host,
                "port": port,
                "error": error.to_string(),
            }),
            Err(_) => serde_json::json!({
                "healthy": false,
                "host": host,
                "port": port,
                "error": "connection timed out",
            }),
        }
    }

    async fn spawn_source(&self, mut socket: TcpStream) {
        let protocol = match peek_proxy_protocol(&socket).await {
            Ok(protocol) => protocol,
            Err(error) => {
                debug!(%error, "network proxy peek failed");
                return;
            }
        };
        let route = match read_proxy_request(&mut socket, protocol).await {
            Ok(route) => route,
            Err(error) => {
                debug!(%error, "rejected local proxy request");
                let _ = write_proxy_failure(&mut socket, protocol).await;
                return;
            }
        };
        if !route.datagram
            && route.destination == SANDBOX_LOCAL_API_IP
            && route.port == self.inner.local_api_address.port()
        {
            let local_api_address = self.inner.local_api_address;
            tokio::spawn(async move {
                if let Err(error) = bridge_local_api(socket, local_api_address, protocol).await {
                    debug!(%error, "local agent API stream closed");
                }
            });
            return;
        }
        if !route.datagram
            && !self.inner.transparent_networking
            && !self.is_virtual_host(&route.host)
            && !self.is_virtual_host(&route.destination)
        {
            let host = route.host.clone();
            let port = route.port;
            tokio::spawn(async move {
                if let Err(error) = bridge_internet_direct(socket, host, port, protocol).await {
                    debug!(%error, "direct internet stream closed");
                }
            });
            return;
        }
        let stream_id = format!("net_{}", Uuid::new_v4().simple());
        let (incoming, incoming_rx) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        let epoch;
        {
            let mut streams = self.inner.streams.lock().await;
            epoch = self.inner.transport_epoch.load(Ordering::SeqCst);
            if !self.inner.proxy_connected.load(Ordering::SeqCst) {
                drop(streams);
                let _ = write_proxy_failure(&mut socket, protocol).await;
                return;
            }
            streams.insert(stream_id.clone(), incoming);
        }
        let runtime = self.clone();
        tokio::spawn(async move {
            let request = NetworkOpenRequest {
                destination: route.destination,
                host: route.host,
                port: route.port,
                source_agent_id: route.source_agent_id,
                track_lifetime: true,
                durable_usage: runtime.inner.usage_outbox.get().is_some(),
            };
            let result = async {
                runtime
                    .send_at_epoch(
                        epoch,
                        NetworkBinaryFrame {
                            kind: if route.datagram {
                                NetworkBinaryKind::OpenDatagram
                            } else {
                                NetworkBinaryKind::Open
                            },
                            stream_id: stream_id.clone(),
                            payload: serde_json::to_vec(&request)?,
                        },
                    )
                    .await?;
                if route.datagram {
                    crate::network_datagram::source(socket, incoming_rx, &runtime, &stream_id).await
                } else {
                    source_stream(socket, incoming_rx, &runtime, &stream_id, protocol).await
                }
            }
            .await;
            let _ = runtime
                .send_at_epoch(
                    epoch,
                    NetworkBinaryFrame {
                        kind: NetworkBinaryKind::Reset,
                        stream_id: stream_id.clone(),
                        payload: encode_error("stream_closed", "source network stream finished"),
                    },
                )
                .await;
            runtime.inner.streams.lock().await.remove(&stream_id);
            if let Err(error) = result {
                debug!(stream_id, %error, "source network stream closed");
            }
        });
    }

    async fn spawn_destination(&self, frame: NetworkBinaryFrame) -> anyhow::Result<()> {
        let datagram = frame.kind == NetworkBinaryKind::OpenDatagram;
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
                if datagram {
                    return crate::network_datagram::destination(
                        request,
                        incoming_rx,
                        &runtime,
                        &stream_id,
                    )
                    .await;
                }
                let socket = connect_destination(
                    &request.host,
                    request.port,
                    request.destination_agent_id.as_deref(),
                    runtime.inner.transparent_networking,
                )
                .await?;
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
                debug!(stream_id, error = %format_args!("{error:#}"), "destination network stream closed");
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

    pub(crate) async fn send(&self, frame: NetworkBinaryFrame) -> anyhow::Result<()> {
        self.send_at_epoch(self.inner.transport_epoch.load(Ordering::SeqCst), frame)
            .await
    }

    async fn send_at_epoch(&self, epoch: u64, frame: NetworkBinaryFrame) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.inner.proxy_connected.load(Ordering::SeqCst),
            "network transport offline"
        );
        self.inner
            .outgoing
            .send((epoch, frame))
            .await
            .map_err(|_| anyhow!("network transport closed"))
    }
}

pub(crate) trait DestinationStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> DestinationStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

async fn connect_destination(
    host: &str,
    port: u16,
    target_agent_id: Option<&str>,
    transparent_networking: bool,
) -> anyhow::Result<Box<dyn DestinationStream>> {
    if let Some(_agent_id) = target_agent_id {
        // Capturing egress does not imply private service namespaces. macOS
        // native capture shares host ports; Linux alone has the Unix bridge.
        if transparent_networking && cfg!(target_os = "linux") {
            #[cfg(target_os = "linux")]
            {
                let path = agent_service_socket_path(_agent_id);
                let mut socket = UnixStream::connect(&path).await.with_context(|| {
                    format!("Agent {_agent_id} is offline or its service bridge is unavailable")
                })?;
                socket
                    .write_u16(port)
                    .await
                    .context("failed to select Agent service port")?;
                if socket
                    .read_u8()
                    .await
                    .context("Agent service bridge closed before connecting")?
                    != 0
                {
                    return Err(anyhow!("Agent service is not listening on port {port}"));
                }
                return Ok(Box::new(socket));
            }
        }
        let socket = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .with_context(|| format!("failed to connect to Agent service on port {port}"))?;
        return Ok(Box::new(socket));
    }
    let socket = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("failed to connect to {host}:{port}"))?;
    Ok(Box::new(socket))
}

pub(crate) async fn connect_agent_service(
    agent_id: &str,
    port: u16,
    transparent_networking: bool,
) -> anyhow::Result<Box<dyn DestinationStream>> {
    connect_destination("127.0.0.1", port, Some(agent_id), transparent_networking).await
}

async fn bridge_local_api(
    mut socket: TcpStream,
    address: SocketAddr,
    protocol: ClientProtocol,
) -> anyhow::Result<()> {
    let mut local_api = match TcpStream::connect(address).await {
        Ok(socket) => socket,
        Err(error) => {
            let _ = write_proxy_failure(&mut socket, protocol).await;
            return Err(error).with_context(|| format!("failed to connect to local API {address}"));
        }
    };
    write_proxy_success(&mut socket, protocol).await?;
    tokio::io::copy_bidirectional(&mut socket, &mut local_api).await?;
    Ok(())
}

struct SocksRoute {
    destination: String,
    host: String,
    port: u16,
    source_agent_id: Option<String>,
    datagram: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClientProtocol {
    Socks,
    HttpConnect,
}

async fn peek_proxy_protocol(socket: &TcpStream) -> anyhow::Result<ClientProtocol> {
    let mut first = [0_u8; 1];
    let n = socket.peek(&mut first).await?;
    if n == 0 {
        return Err(anyhow!("proxy connection closed"));
    }
    if first[0] == 5 {
        Ok(ClientProtocol::Socks)
    } else {
        Ok(ClientProtocol::HttpConnect)
    }
}

async fn read_proxy_request(
    socket: &mut TcpStream,
    protocol: ClientProtocol,
) -> anyhow::Result<SocksRoute> {
    match protocol {
        ClientProtocol::Socks => read_socks_request(socket).await,
        ClientProtocol::HttpConnect => read_http_connect_request(socket).await,
    }
}

async fn bind_near(api_address: SocketAddr) -> io::Result<TcpListener> {
    let ip = if api_address.ip().is_loopback() {
        api_address.ip()
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    let first = if api_address.port() == 0 {
        0
    } else {
        api_address.port().saturating_add(1)
    };
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
    // Private, authenticated Treer datagram transport. Each datagram is framed
    // by a big-endian u16 length over this TCP association, including length 0.
    let datagram = request[1] == 0xf0 && source_agent_id.is_some();
    if request[0] != 5 || request[2] != 0 || (request[1] != 1 && !datagram) {
        return Err(anyhow!("unsupported SOCKS5 command"));
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
    route.datagram = datagram;
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

async fn read_http_connect_request(socket: &mut TcpStream) -> anyhow::Result<SocksRoute> {
    let header = read_http_header_block(socket).await?;
    let text = std::str::from_utf8(&header).context("HTTP proxy request is not UTF-8")?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if !method.eq_ignore_ascii_case("CONNECT") {
        return Err(anyhow!("only HTTP CONNECT is supported"));
    }
    let (host, port) = parse_connect_target(target)?;
    let mut source_agent_id = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("proxy-authorization") {
            source_agent_id = Some(parse_proxy_basic_identity(value.trim())?);
        }
    }
    let mut route = parse_route(&host, port)?;
    route.source_agent_id = source_agent_id;
    Ok(route)
}

async fn read_http_header_block(socket: &mut TcpStream) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    while buf.len() < 8192 {
        buf.push(socket.read_u8().await?);
        if buf.ends_with(b"\r\n\r\n") {
            return Ok(buf);
        }
    }
    Err(anyhow!("HTTP proxy headers too large"))
}

fn parse_connect_target(target: &str) -> anyhow::Result<(String, u16)> {
    if let Some(rest) = target.strip_prefix('[') {
        let (host, port) = rest
            .split_once("]:")
            .ok_or_else(|| anyhow!("invalid IPv6 CONNECT target"))?;
        let port = port.parse().context("invalid IPv6 CONNECT port")?;
        return Ok((host.to_string(), port));
    }
    let (host, port) = target
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("invalid CONNECT target"))?;
    let port = port.parse().context("invalid CONNECT port")?;
    Ok((host.to_string(), port))
}

fn parse_proxy_basic_identity(value: &str) -> anyhow::Result<String> {
    let encoded = value
        .strip_prefix("Basic ")
        .or_else(|| value.strip_prefix("basic "))
        .ok_or_else(|| anyhow!("HTTP proxy auth must be Basic"))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .context("HTTP proxy identity is not Base64")?;
    let text = String::from_utf8(decoded).context("HTTP proxy identity is not UTF-8")?;
    let (user, password) = text
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid HTTP proxy identity"))?;
    if user.is_empty() || password != "treer" {
        return Err(anyhow!("invalid Treer HTTP proxy agent identity"));
    }
    Ok(user.to_string())
}

fn normalize_hostname(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

async fn bridge_internet_direct(
    mut socket: TcpStream,
    host: String,
    port: u16,
    protocol: ClientProtocol,
) -> anyhow::Result<()> {
    let mut destination = match TcpStream::connect((host.as_str(), port)).await {
        Ok(destination) => destination,
        Err(error) => {
            write_proxy_failure(&mut socket, protocol).await?;
            return Err(error)
                .with_context(|| format!("failed to connect directly to {host}:{port}"));
        }
    };
    write_proxy_success(&mut socket, protocol).await?;
    tokio::io::copy_bidirectional(&mut socket, &mut destination)
        .await
        .context("direct internet stream failed")?;
    Ok(())
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
        datagram: false,
    })
}

async fn source_stream(
    mut socket: TcpStream,
    mut incoming: mpsc::Receiver<NetworkBinaryFrame>,
    runtime: &NetworkRuntime,
    stream_id: &str,
    protocol: ClientProtocol,
) -> anyhow::Result<()> {
    let route = tokio::time::timeout(std::time::Duration::from_secs(15), incoming.recv())
        .await
        .context("network route authorization timed out")?
        .ok_or_else(|| anyhow!("network stream closed before open"))?;
    match route.kind {
        NetworkBinaryKind::Direct => {
            let target: NetworkDirectTarget = match serde_json::from_slice(&route.payload) {
                Ok(target) => target,
                Err(error) => {
                    write_proxy_failure(&mut socket, protocol).await?;
                    return Err(error).context("invalid direct network target");
                }
            };
            if target.host.is_empty() || target.port == 0 {
                write_proxy_failure(&mut socket, protocol).await?;
                return Err(anyhow!("direct network target is invalid"));
            }
            let connecting = tokio::select! {
                result = tokio::time::timeout(std::time::Duration::from_secs(10),
                    TcpStream::connect((target.host.as_str(), target.port))) => {
                    result.context("Direct connection timed out").and_then(|result| result.map_err(Into::into))
                }
                _ = incoming.recv() => Err(anyhow!("Direct authorization ended while connecting")),
            };
            let mut destination = match connecting {
                Ok(socket) => socket,
                Err(error) => {
                    if target.usage_ticket.is_some() {
                        let _ = runtime
                            .report_usage(
                                stream_id,
                                target.usage_ticket.as_deref(),
                                Default::default(),
                                true,
                            )
                            .await;
                    }
                    write_proxy_failure(&mut socket, protocol).await?;
                    return Err(error).with_context(|| {
                        format!(
                            "failed to connect directly to {}:{}",
                            target.host, target.port
                        )
                    });
                }
            };
            write_proxy_success(&mut socket, protocol).await?;
            // Direct is authorized by the Proxy too. It must stop when that
            // authorization channel resets, just like a relayed stream.
            let sent = Arc::new(crate::network_meter::WriteCounter::default());
            let received = Arc::new(crate::network_meter::WriteCounter::default());
            let mut source = crate::network_meter::Metered {
                stream: &mut socket,
                written: received.clone(),
            };
            let mut target_socket = crate::network_meter::Metered {
                stream: &mut destination,
                written: sent.clone(),
            };
            let transfer = tokio::io::copy_bidirectional(&mut source, &mut target_socket);
            tokio::pin!(transfer);
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tick.tick().await;
            let report = |finished| {
                runtime.report_usage(
                    stream_id,
                    target.usage_ticket.as_deref(),
                    crate::network_meter::totals(&sent, &received),
                    finished,
                )
            };
            let result = loop {
                tokio::select! {
                    result = &mut transfer => break result.map(|_| ()).context("direct network stream failed"),
                    _ = tick.tick(), if target.report_usage => { report(false).await?; }
                    frame = incoming.recv() => break match frame {
                        Some(frame) if frame.kind == NetworkBinaryKind::Reset => Err(decode_reset(&frame)),
                        Some(_) => Err(anyhow!("unexpected frame on direct network stream")),
                        None => Err(anyhow!("direct network authorization channel closed")),
                    }
                }
            };
            if target.report_usage {
                let _ = report(true).await;
            }
            result
        }
        NetworkBinaryKind::Opened => {
            let result = async {
                write_proxy_success(&mut socket, protocol).await?;
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
            write_proxy_failure(&mut socket, protocol).await?;
            Err(decode_reset(&route))
        }
        _ => {
            write_proxy_failure(&mut socket, protocol).await?;
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

async fn write_http_error(socket: &mut TcpStream, status: u16) -> io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        407 => "Proxy Authentication Required",
        501 => "Not Implemented",
        _ => "Bad Gateway",
    };
    socket
        .write_all(
            format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
}

async fn write_proxy_success(socket: &mut TcpStream, protocol: ClientProtocol) -> io::Result<()> {
    match protocol {
        ClientProtocol::Socks => write_socks_reply(socket, 0).await,
        ClientProtocol::HttpConnect => {
            socket
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
        }
    }
}

async fn write_proxy_failure(socket: &mut TcpStream, protocol: ClientProtocol) -> io::Result<()> {
    match protocol {
        ClientProtocol::Socks => write_socks_reply(socket, 0x05).await,
        ClientProtocol::HttpConnect => write_http_error(socket, 502).await,
    }
}

async fn bridge_stream<S>(
    socket: S,
    mut incoming: mpsc::Receiver<NetworkBinaryFrame>,
    runtime: &NetworkRuntime,
    stream_id: &str,
    mut send_window: usize,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(socket);
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
                    NetworkBinaryKind::Open | NetworkBinaryKind::OpenDatagram | NetworkBinaryKind::Opened | NetworkBinaryKind::Direct | NetworkBinaryKind::Usage | NetworkBinaryKind::UsageAck => {
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
#[path = "network_tests.rs"]
mod tests;
