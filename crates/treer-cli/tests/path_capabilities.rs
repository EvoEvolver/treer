use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn old_controller_gets_an_explicit_error_without_opening_a_cli_session() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut input = vec![0; 4096];
        let bytes = stream.read(&mut input).await.unwrap();
        assert!(String::from_utf8_lossy(&input[..bytes]).starts_with("GET /api/health "));
        let body = r#"{"status":"ok","service":"treer-agent-server"}"#;
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        stream.write_all(response.as_bytes()).await.unwrap();
        drop(stream);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), listener.accept())
                .await
                .is_err(),
            "no command WebSocket may open on an old Controller"
        );
    });
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_treer"))
        .env_clear()
        .env("TREER_IDENTITY_MODE", "agent")
        .env("TREER_AGENT_ID", "ag_test")
        .env("TREER_WORKLOAD_CREDENTIAL", "test-only")
        .args([
            "--url",
            &format!("http://{address}"),
            "agent",
            "show",
            "../team/reviewer",
        ])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("unsupported_command"), "{error}");
    assert!(error.contains("agent.path-targets.v1"), "{error}");
    assert!(output.stdout.is_empty());
    server.await.unwrap();
}
