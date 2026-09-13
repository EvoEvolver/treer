use std::time::Duration;

use super::*;
#[cfg(target_os = "linux")]
use tokio::net::UnixListener;

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

async fn bind_online_test_runtime(
    address: SocketAddr,
    transparent: bool,
) -> anyhow::Result<NetworkRuntime> {
    let runtime = NetworkRuntime::bind_near(address, transparent).await?;
    runtime.set_proxy_connected();
    Ok(runtime)
}

#[tokio::test]
async fn stalled_proxy_handshake_does_not_block_other_agents() {
    let runtime = bind_online_test_runtime("127.0.0.1:0".parse().unwrap(), true)
        .await
        .unwrap();
    let mut stalled = TcpStream::connect(runtime.listen_address()).await.unwrap();
    stalled.write_all(&[5]).await.unwrap();
    let mut client = TcpStream::connect(runtime.listen_address()).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        request_ipv4(&mut client, Ipv4Addr::LOCALHOST, 12345),
    )
    .await
    .unwrap();
    assert_eq!(next_frame(&runtime).await.kind, NetworkBinaryKind::Open);
    drop(stalled);
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
            5, 1, 0, 3, 12, b'a', b'p', b'i', b'.', b'i', b'n', b't', b'e', b'r', b'n', b'a', b'l',
            0, 80,
        ])
        .await
        .expect("connect request");
    let route = server.await.expect("SOCKS server task");
    assert_eq!(route.destination, "api.internal");
    assert_eq!(route.source_agent_id.as_deref(), Some("agent-a"));
}

#[tokio::test]
async fn http_connect_username_becomes_the_source_agent_identity() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("test listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept HTTP client");
        let protocol = peek_proxy_protocol(&socket)
            .await
            .expect("peek HTTP CONNECT");
        assert_eq!(protocol, ClientProtocol::HttpConnect);
        read_proxy_request(&mut socket, protocol)
            .await
            .expect("HTTP CONNECT request")
    });
    let mut client = TcpStream::connect(address)
        .await
        .expect("connect HTTP proxy");
    let identity = base64::engine::general_purpose::STANDARD.encode("agent-a:treer");
    let request = format!(
            "CONNECT api.internal:80 HTTP/1.1\r\nHost: api.internal:80\r\nProxy-Authorization: Basic {identity}\r\n\r\n"
        );
    client
        .write_all(request.as_bytes())
        .await
        .expect("HTTP CONNECT request");
    let route = server.await.expect("HTTP proxy task");
    assert_eq!(route.destination, "api.internal");
    assert_eq!(route.port, 80);
    assert_eq!(route.source_agent_id.as_deref(), Some("agent-a"));
}

#[tokio::test]
async fn http_connect_direct_route_bridges_locally() {
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
    let runtime = bind_online_test_runtime(api_address, false)
        .await
        .expect("bind network runtime");
    runtime.set_virtual_hostnames(["direct.test"]);
    let mut client = TcpStream::connect(runtime.listen_address())
        .await
        .expect("connect HTTP proxy");
    let identity = base64::engine::general_purpose::STANDARD.encode("agent-a:treer");
    client
        .write_all(
            format!(
                "CONNECT direct.test:80 HTTP/1.1\r\nProxy-Authorization: Basic {identity}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("HTTP CONNECT request");

    let source_open = next_frame(&runtime).await;
    assert_eq!(source_open.kind, NetworkBinaryKind::Open);
    let open = serde_json::from_slice::<NetworkOpenRequest>(&source_open.payload)
        .expect("decode network open");
    assert_eq!(open.destination, "direct.test");
    assert_eq!(open.source_agent_id.as_deref(), Some("agent-a"));
    runtime
        .handle_incoming(NetworkBinaryFrame {
            kind: NetworkBinaryKind::Direct,
            stream_id: source_open.stream_id,
            payload: serde_json::to_vec(&NetworkDirectTarget {
                report_usage: false,
                usage_ticket: None,
                host: Ipv4Addr::LOCALHOST.to_string(),
                port: target_port,
            })
            .expect("encode direct target"),
        })
        .await
        .expect("apply direct route");

    let mut established = [0_u8; 39];
    client
        .read_exact(&mut established)
        .await
        .expect("HTTP CONNECT reply");
    assert_eq!(&established, b"HTTP/1.1 200 Connection Established\r\n\r\n");
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
    let runtime = bind_online_test_runtime(local_api_address, false)
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
    let runtime = bind_online_test_runtime(api_address, false)
        .await
        .expect("bind network runtime");
    runtime.set_virtual_hostnames(["direct.test"]);
    let mut client = TcpStream::connect(runtime.listen_address())
        .await
        .expect("connect SOCKS client");
    client.write_all(&[5, 1, 0]).await.expect("SOCKS greeting");
    let mut method = [0_u8; 2];
    client.read_exact(&mut method).await.expect("SOCKS method");
    assert_eq!(method, [5, 0]);
    client
        .write_all(&[
            5, 1, 0, 3, 11, b'd', b'i', b'r', b'e', b'c', b't', b'.', b't', b'e', b's', b't', 0, 80,
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
                report_usage: true,
                usage_ticket: None,
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
    let usage = next_frame(&runtime).await;
    assert_eq!(usage.kind, NetworkBinaryKind::Usage);
    let totals: treer_protocol::NetworkUsageTotals =
        serde_json::from_slice(&usage.payload).unwrap();
    assert_eq!(totals.sent_bytes, 18);
    assert_eq!(totals.received_bytes, response.len() as u64);
    assert!(totals.sent_chunks > 0 && totals.received_chunks > 0);
    let completed = next_frame(&runtime).await;
    assert_eq!(completed.kind, NetworkBinaryKind::Reset);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), runtime.next_outgoing())
            .await
            .is_err(),
        "Direct completion emits one lifecycle Reset, never Proxy payload frames"
    );
}

#[tokio::test]
async fn unknown_host_dials_locally_without_proxy_open() {
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
    let runtime = bind_online_test_runtime(api_address, false)
        .await
        .expect("bind network runtime");
    let mut client = TcpStream::connect(runtime.listen_address())
        .await
        .expect("connect HTTP proxy");
    let identity = base64::engine::general_purpose::STANDARD.encode("agent-a:treer");
    client
            .write_all(
                format!(
                    "CONNECT 127.0.0.1:{target_port} HTTP/1.1\r\nProxy-Authorization: Basic {identity}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("HTTP CONNECT request");

    let mut established = [0_u8; 39];
    tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut established))
        .await
        .expect("HTTP CONNECT reply timeout")
        .expect("HTTP CONNECT reply");
    assert_eq!(&established, b"HTTP/1.1 200 Connection Established\r\n\r\n");
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
    assert!(
        tokio::time::timeout(Duration::from_millis(50), runtime.next_outgoing())
            .await
            .is_err(),
        "public internet must not wait on Proxy Open frames"
    );
    runtime.reset_all().await;
    target_task.await.expect("target task");
}

#[tokio::test]
async fn virtual_host_still_sends_open_when_present_in_the_snapshot() {
    let api_reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("reserve API port");
    let api_address = api_reservation.local_addr().expect("API address");
    drop(api_reservation);
    let runtime = bind_online_test_runtime(api_address, false)
        .await
        .expect("bind network runtime");
    runtime.set_virtual_hostnames(["api.internal"]);
    let mut client = TcpStream::connect(runtime.listen_address())
        .await
        .expect("connect HTTP proxy");
    let identity = base64::engine::general_purpose::STANDARD.encode("agent-a:treer");
    client
        .write_all(
            format!(
                "CONNECT api.internal:80 HTTP/1.1\r\nProxy-Authorization: Basic {identity}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("HTTP CONNECT request");
    let source_open = next_frame(&runtime).await;
    assert_eq!(source_open.kind, NetworkBinaryKind::Open);
    let open = serde_json::from_slice::<NetworkOpenRequest>(&source_open.payload)
        .expect("decode network open");
    assert_eq!(open.destination, "api.internal");
}

#[tokio::test]
async fn offline_transparent_open_fails_without_waiting_for_reconnect() {
    let api = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let runtime = NetworkRuntime::bind_near(api.local_addr().unwrap(), true)
        .await
        .unwrap();
    let mut client = TcpStream::connect(runtime.listen_address()).await.unwrap();
    request_ipv4(&mut client, Ipv4Addr::new(203, 0, 113, 1), 443).await;
    let mut reply = [0; 10];
    tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(reply[1], 0);
    runtime.set_proxy_connected();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), runtime.next_outgoing())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn reconnect_discards_queued_and_late_opens_from_old_transport() {
    let api = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let runtime = bind_online_test_runtime(api.local_addr().unwrap(), true)
        .await
        .unwrap();
    let frame = |id: &str| NetworkBinaryFrame {
        kind: NetworkBinaryKind::Open,
        stream_id: id.into(),
        payload: vec![],
    };
    runtime.send_at_epoch(0, frame("queued-old")).await.unwrap();
    runtime.reset_all().await;
    runtime.set_proxy_connected();
    runtime.send_at_epoch(0, frame("late-old")).await.unwrap();
    runtime.send(frame("new")).await.unwrap();
    assert_eq!(next_frame(&runtime).await.stream_id, "new");
}

#[tokio::test]
async fn transparent_direct_stream_stops_when_proxy_disconnects() {
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let target_port = target.local_addr().unwrap().port();
    let api = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let runtime = bind_online_test_runtime(api.local_addr().unwrap(), true)
        .await
        .unwrap();
    let mut client = TcpStream::connect(runtime.listen_address()).await.unwrap();
    request_ipv4(&mut client, Ipv4Addr::LOCALHOST, target_port).await;
    let open = next_frame(&runtime).await;
    assert_eq!(open.kind, NetworkBinaryKind::Open);
    runtime
        .handle_incoming(NetworkBinaryFrame {
            kind: NetworkBinaryKind::Direct,
            stream_id: open.stream_id,
            payload: serde_json::to_vec(&NetworkDirectTarget {
                report_usage: false,
                usage_ticket: None,
                host: Ipv4Addr::LOCALHOST.to_string(),
                port: target_port,
            })
            .unwrap(),
        })
        .await
        .unwrap();
    let (mut destination, _) = target.accept().await.unwrap();
    let mut reply = [0; 10];
    client.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[1], 0);
    client.write_all(b"before reset\x00\xff").await.unwrap();
    let mut payload = [0; 14];
    destination.read_exact(&mut payload).await.unwrap();
    assert_eq!(&payload, b"before reset\x00\xff");
    runtime.reset_all().await;
    for stream in [&mut client, &mut destination] {
        let mut byte = [0];
        let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
            .await
            .expect("Direct socket must close after authorization disconnect");
        assert!(matches!(result, Ok(0) | Err(_)));
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_transparent_service_ingress_uses_shared_host_ports() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut service = connect_agent_service("agent-native", port, true)
        .await
        .expect("transparent Mac services do not require Linux namespaces");
    let (mut accepted, _) = listener.accept().await.unwrap();
    service.write_all(b"shared\x00\xff").await.unwrap();
    service.shutdown().await.unwrap();
    let mut request = Vec::new();
    accepted.read_to_end(&mut request).await.unwrap();
    assert_eq!(request, b"shared\x00\xff");
    accepted.write_all(b"after FIN").await.unwrap();
    accepted.shutdown().await.unwrap();
    let mut response = Vec::new();
    service.read_to_end(&mut response).await.unwrap();
    assert_eq!(response, b"after FIN");
}

#[tokio::test]
async fn reset_all_does_not_kill_direct_internet_streams() {
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
    let runtime = bind_online_test_runtime(api_address, false)
        .await
        .expect("bind network runtime");
    let mut client = TcpStream::connect(runtime.listen_address())
        .await
        .expect("connect HTTP proxy");
    let identity = base64::engine::general_purpose::STANDARD.encode("agent-a:treer");
    client
            .write_all(
                format!(
                    "CONNECT 127.0.0.1:{target_port} HTTP/1.1\r\nProxy-Authorization: Basic {identity}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("HTTP CONNECT request");
    let mut established = [0_u8; 39];
    client
        .read_exact(&mut established)
        .await
        .expect("HTTP CONNECT reply");
    runtime.reset_all().await;
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
    let runtime = bind_online_test_runtime(api_address, false)
        .await
        .expect("bind network runtime");
    runtime.set_virtual_hostnames(["test.api"]);
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
                destination_agent_id: None,
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn transparent_agent_destination_uses_its_unix_bridge() {
    let agent_id = format!("test-{}", Uuid::new_v4().simple());
    let path = agent_service_socket_path(&agent_id);
    let listener = UnixListener::bind(&path).expect("bind Agent service bridge");
    let bridge = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept bridge request");
        assert_eq!(stream.read_u16().await.expect("read service port"), 4242);
        stream.write_u8(0).await.expect("write bridge ACK");
        let mut payload = [0_u8; 4];
        stream.read_exact(&mut payload).await.expect("read payload");
        assert_eq!(&payload, b"ping");
    });

    let mut stream = connect_destination("ignored.example", 4242, Some(&agent_id), true)
        .await
        .expect("connect through Agent bridge");
    stream
        .write_all(b"ping")
        .await
        .expect("write bridge payload");
    drop(stream);
    bridge.await.expect("bridge task");
    std::fs::remove_file(path).expect("remove bridge socket");
}
