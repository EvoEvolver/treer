use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use treer_protocol::cli::{CliInput, CliOutput, CliRequest};

fn client(address: std::net::SocketAddr) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_treer"));
    command
        .env("TREER_AGENT_SERVER_URL", format!("http://{address}"))
        .env("TREER_IDENTITY_MODE", "agent")
        .env("TREER_AGENT_ID", "agent-test")
        .env("TREER_WORKLOAD_CREDENTIAL", "workload-test")
        .env("TREER_OPERATOR_CREDENTIAL", "must-not-use-operator")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command
}
#[tokio::test]
async fn raw_argv_stdin_eof_output_streams_and_exit_are_preserved() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(request.headers()["x-treer-agent-id"], "agent-test");
                assert_eq!(
                    request.headers()["x-treer-workload-credential"],
                    "workload-test"
                );
                assert!(!request
                    .headers()
                    .contains_key("x-treer-operator-credential"));
                Ok(response)
            },
        )
        .await
        .unwrap();
        let request = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let request: CliRequest = serde_json::from_str(&request).unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&CliOutput::StdinReady { bytes: 8192 })
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
        assert_eq!(
            request.argv,
            [
                "message",
                "send",
                "--to",
                "peer",
                "--body",
                "spaces ' quotes $()",
                "--url=literal"
            ]
        );
        let mut bytes = Vec::new();
        loop {
            let text = socket.next().await.unwrap().unwrap().into_text().unwrap();
            match serde_json::from_str::<CliInput>(&text).unwrap() {
                CliInput::Stdin { data } => bytes.extend(data),
                CliInput::Eof => break,
                _ => panic!("unexpected input"),
            }
        }
        assert_eq!(bytes, b"body\nwith\0bytes\n");
        for output in [
            CliOutput::Stdout {
                data: b"out\0\n".to_vec(),
            },
            CliOutput::Stderr {
                data: b"err\n".to_vec(),
            },
            CliOutput::Exit { code: 7 },
        ] {
            socket
                .send(Message::Text(
                    serde_json::to_string(&output).unwrap().into(),
                ))
                .await
                .unwrap();
        }
    });
    let mut child = client(address)
        .args([
            "message",
            "send",
            "--to",
            "peer",
            "--body",
            "spaces ' quotes $()",
            "--url=literal",
        ])
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"body\nwith\0bytes\n")
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    server.await.unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"out\0\n");
    assert_eq!(output.stderr, b"err\n");
}
#[tokio::test]
async fn explicit_agent_mode_never_falls_back_to_operator_credentials() {
    let output = client("127.0.0.1:1".parse().unwrap())
        .env_remove("TREER_WORKLOAD_CREDENTIAL")
        .arg("--help")
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("operator fallback is disabled"));
}
#[tokio::test]
async fn server_cannot_direct_client_credentials_to_another_origin() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&CliOutput::Forward {
                    path: "https://elsewhere.invalid/api/connect".into(),
                    listen: "127.0.0.1:0".into(),
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
    });
    let output = client(address)
        .args(["service", "forward", "x"])
        .output()
        .await
        .unwrap();
    server.await.unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid server transport path"));
}

#[tokio::test]
async fn forwarding_stream_preserves_half_close_and_authentication() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_address = reserved.local_addr().unwrap();
    drop(reserved);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&CliOutput::Forward {
                    path: "/api/services/test/connect".into(),
                    listen: local_address.to_string(),
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let (mut remote, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        loop {
            let byte = remote.read_u8().await.unwrap();
            header.push(byte);
            if header.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let header = String::from_utf8(header).unwrap().to_lowercase();
        assert!(header.starts_with("post /api/services/test/connect "));
        assert!(header.contains("x-treer-workload-credential: workload-test"));
        assert!(!header.contains("must-not-use-operator"));
        remote.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: treer-tcp\r\n\r\n").await.unwrap();
        let mut request = Vec::new();
        remote.read_to_end(&mut request).await.unwrap();
        assert_eq!(request, b"payload\0after");
        remote.write_all(b"reply after EOF").await.unwrap();
        remote.shutdown().await.unwrap();
    });
    let mut child = client(address)
        .args(["service", "forward", "test"])
        .spawn()
        .unwrap();
    let mut connected = None;
    for _ in 0..100 {
        if let Ok(stream) = tokio::net::TcpStream::connect(local_address).await {
            connected = Some(stream);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let mut local = connected.expect("forward listener starts");
    local.write_all(b"payload\0after").await.unwrap();
    local.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        local.read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response, b"reply after EOF");
    server.await.unwrap();
    child.kill().await.unwrap();
}

#[tokio::test]
async fn generic_session_streams_binary_and_remote_exit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, wait_ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut command = tokio_tungstenite::accept_async(stream).await.unwrap();
        command.next().await.unwrap().unwrap();
        command
            .send(Message::Text(
                serde_json::to_string(&CliOutput::Session {
                    path: "/api/agents/test/terminal".into(),
                    terminal: false,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut terminal = tokio_tungstenite::accept_async(stream).await.unwrap();
        ready.send(()).unwrap();
        assert_eq!(
            terminal.next().await.unwrap().unwrap().into_data().as_ref(),
            b"input"
        );
        terminal
            .send(Message::Binary(b"terminal\0output".to_vec().into()))
            .await
            .unwrap();
        terminal
            .send(Message::Text(
                serde_json::to_string(&treer_protocol::TerminalServerMessage::Closed {
                    reason: None,
                    exit_code: Some(3),
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
    });
    let mut child = client(address)
        .arg("arbitrary-new-command")
        .spawn()
        .unwrap();
    // Keep stdin open until the server switches the client to the generic session.
    wait_ready.await.unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"input").await.unwrap();
    let output = child.wait_with_output().await.unwrap();
    drop(stdin);
    server.await.unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stdout, b"terminal\0output");
}
