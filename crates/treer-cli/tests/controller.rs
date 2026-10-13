//! Run with `cargo build -p treer-agent-server -p treer-agent-host`, then
//! `cargo test -p treer-cli --test controller -- --ignored`.

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};
use treer_protocol::cli::{CliInput, CliOutput, CliRequest, CLI_PROTOCOL_VERSION};

struct Processes(Vec<tokio::process::Child>);
impl Drop for Processes {
    fn drop(&mut self) {
        for child in &mut self.0 {
            if child.try_wait().ok().flatten().is_some() {
                continue;
            }
            if let Some(pid) = child.id() {
                #[cfg(unix)]
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", "--", &format!("-{pid}")])
                    .status();
                #[cfg(windows)]
                {
                    let _ = pid;
                    let _ = child.start_kill();
                }
            }
        }
    }
}

#[tokio::test]
#[ignore = "requires freshly built sibling Controller and Host binaries"]
async fn real_controller_auth_parser_stdin_and_cancel() {
    let root = std::env::temp_dir().join(format!(
        "tc-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    ));
    std::fs::create_dir_all(&root).unwrap();
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_treer"));
    let binaries = binary.parent().unwrap();
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let (request_seen, mut wait_request) = tokio::sync::mpsc::unbounded_channel();
    let proxy_task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = proxy.accept().await.unwrap();
            let request_seen = request_seen.clone();
            tokio::spawn(async move {
                let mut header = Vec::new();
                while let Ok(byte) = stream.read_u8().await {
                    header.push(byte);
                    if header.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                if String::from_utf8_lossy(&header).contains("/snapshot ") {
                    let _ = request_seen.send(());
                }
                std::future::pending::<()>().await;
            });
        }
    });
    #[cfg(unix)]
    let socket = root.join("host.sock");
    #[cfg(windows)]
    let socket = treer_platform::endpoint_name(
        &uuid::Uuid::new_v4().to_string(),
        &treer_platform::current_user_sid().unwrap(),
    );
    let sibling = |name: &str| binaries.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let controller_config = root.join("controller.json");
    let host_config = root.join("host.json");
    std::fs::write(&controller_config, serde_json::to_vec(&serde_json::json!({
        "proxy": format!("http://{proxy_address}"), "workspace":"test", "server_id":"test-server", "machine_token":"test-machine",
        "operator_credential":"test-operator", "root":root, "listen":address.to_string(), "host_socket":socket,
        "install_hostname":String::from_utf8(std::process::Command::new("hostname").output().unwrap().stdout).unwrap().trim(), "network_mode":"plain"
    })).unwrap()).unwrap();
    std::fs::write(&host_config, serde_json::to_vec(&serde_json::json!({
        "socket_path":socket, "controller_path":sibling("treer-agent-server"), "controller_config_path":controller_config, "root":root
    })).unwrap()).unwrap();
    let mut host = tokio::process::Command::new(sibling("treer-agent-host"));
    #[cfg(unix)]
    host.process_group(0);
    let mut processes = Processes(vec![host
        .args(["run", "--config"])
        .arg(&host_config)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("LOCALAPPDATA", root.join("local"))
        .env("TREER_STATE_DIR", root.join("state"))
        .env_remove("TREER_AGENT_ID")
        .env_remove("TREER_WORKLOAD_CREDENTIAL")
        .env_remove("TREER_IDENTITY_MODE")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap()]);
    let http = reqwest::Client::new();
    let mut online = false;
    for _ in 0..250 {
        if http
            .get(format!("http://{address}/api/health"))
            .send()
            .await
            .is_ok()
        {
            online = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(online, "isolated Controller starts");
    let mut forged = format!("ws://{address}/api/cli/session")
        .into_client_request()
        .unwrap();
    forged
        .headers_mut()
        .insert("host", HeaderValue::from_static("127.0.0.1:1"));
    forged.headers_mut().insert(
        "x-treer-operator-credential",
        HeaderValue::from_static("test-operator"),
    );
    let (mut forged, _) = tokio_tungstenite::connect_async(forged).await.unwrap();
    forged
        .send(Message::Text(
            serde_json::to_string(&CliRequest {
                version: CLI_PROTOCOL_VERSION,
                request_id: uuid::Uuid::new_v4().to_string(),
                argv: vec!["status".into()],
                terminal: false,
                cols: 120,
                rows: 36,
                capabilities: Vec::new(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), wait_request.recv())
        .await
        .unwrap()
        .unwrap();
    forged
        .send(Message::Text(
            serde_json::to_string(&CliInput::Cancel).unwrap().into(),
        ))
        .await
        .unwrap();
    loop {
        let text = tokio::time::timeout(std::time::Duration::from_secs(5), forged.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        if let CliOutput::Exit { code } = serde_json::from_str(&text).unwrap() {
            assert_eq!(code, 130);
            break;
        }
    }
    let command = |argv: &[&str]| {
        let mut command = tokio::process::Command::new(&binary);
        command
            .args(argv)
            .env("TREER_AGENT_SERVER_URL", format!("http://{address}"))
            .env("TREER_OPERATOR_CREDENTIAL", "test-operator")
            .env_remove("TREER_AGENT_ID")
            .env_remove("TREER_WORKLOAD_CREDENTIAL")
            .env_remove("TREER_IDENTITY_MODE")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        command
    };
    let help = command(&["--help"]).output().await.unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("service"));
    let invalid = command(&["unknown-business-command"])
        .output()
        .await
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    let skill = command(&["--skill"]).output().await.unwrap();
    assert!(skill.status.success());
    assert!(skill.stdout.starts_with(b"---"));
    let wrong = command(&["--help"])
        .env("TREER_OPERATOR_CREDENTIAL", "wrong")
        .output()
        .await
        .unwrap();
    assert!(!wrong.status.success());
    let mut body = command(&["message", "send", "--to", "peer"])
        .spawn()
        .unwrap();
    body.stdin
        .take()
        .unwrap()
        .write_all(b"stdin body\n")
        .await
        .unwrap();
    let body = tokio::time::timeout(std::time::Duration::from_secs(5), body.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body.status.code(), Some(1)); // operator is deliberately unauthorized to send Agent Messages
    assert!(!String::from_utf8_lossy(&body.stderr).contains("failed to read stdin"));
    let files = command(&[
        "message",
        "send",
        "--to",
        "peer",
        "--body-file",
        "/should-not-be-read",
    ])
    .output()
    .await
    .unwrap();
    assert!(String::from_utf8_lossy(&files.stderr).contains("pipe the body to stdin"));
    #[cfg(unix)]
    {
        let mut pending = command(&["status"]).spawn().unwrap();
        let mut pending_stdin = pending.stdin.take().unwrap();
        let large_stdin = tokio::spawn(async move {
            let _ = pending_stdin.write_all(&vec![b'x'; 1024 * 1024]).await;
        });
        // Proxy sockets stay open without responding, so the business request remains pending.
        tokio::time::timeout(std::time::Duration::from_secs(5), wait_request.recv())
            .await
            .unwrap()
            .unwrap();
        std::process::Command::new("kill")
            .args(["-INT", &pending.id().unwrap().to_string()])
            .status()
            .unwrap();
        let cancelled = tokio::time::timeout(std::time::Duration::from_secs(5), pending.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.code(), Some(130));
        large_stdin.await.unwrap();
    }
    let help_again = command(&["--help"]).output().await.unwrap();
    assert!(
        help_again.status.success(),
        "cancel leaves Controller alive"
    );
    #[cfg(unix)]
    std::process::Command::new("kill")
        .args(["-TERM", &processes.0[0].id().unwrap().to_string()])
        .status()
        .unwrap();
    #[cfg(windows)]
    processes.0[0].start_kill().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), processes.0[0].wait())
        .await
        .unwrap()
        .unwrap();
    proxy_task.abort();
    // Host shutdown owns and stops its isolated Controller; generated state stays under this temporary root.
}
