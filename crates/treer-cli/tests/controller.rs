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

type TransferCommand = (
    treer_protocol::AgentCommand,
    tokio::sync::oneshot::Sender<treer_protocol::CommandResult>,
);
type TransferIncoming =
    std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<TransferCommand>>>;
#[derive(Clone)]
struct TransferProxy {
    commands: tokio::sync::mpsc::Sender<TransferCommand>,
    incoming: TransferIncoming,
    server: std::sync::Arc<std::sync::Mutex<Option<treer_protocol::ServerInfo>>>,
}
impl TransferProxy {
    async fn relay(&self, command: treer_protocol::AgentCommand) -> axum::response::Response {
        use axum::response::IntoResponse;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.commands.send((command, sender)).await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), receiver)
            .await
            .unwrap()
            .unwrap();
        if let Some(error) = result.error {
            (
                axum::http::StatusCode::CONFLICT,
                axum::Json(serde_json::json!({"error":error})),
            )
                .into_response()
        } else {
            axum::Json(result.data.unwrap()).into_response()
        }
    }
}

async fn transfer_proxy_socket(
    axum::extract::State(state): axum::extract::State<TransferProxy>,
    ws: axum::extract::WebSocketUpgrade,
) -> axum::response::Response {
    ws.on_upgrade(move |mut socket| async move {
        let Some(Ok(axum::extract::ws::Message::Text(text))) = socket.next().await else {return;};
        let treer_protocol::AgentServerMessage::Register {server,capabilities,..} = serde_json::from_str(&text).unwrap() else {panic!("registration expected")};
        assert!(capabilities.iter().any(|cap| cap==treer_protocol::CAPABILITY_MACHINE_FILE_TRANSFER));
        *state.server.lock().unwrap() = Some(server);
        socket.send(axum::extract::ws::Message::Text(serde_json::to_string(&treer_protocol::ProxyMessage::Registered {protocol:treer_protocol::PROTOCOL_VERSION,capabilities:vec![],workspace_revision:1}).unwrap().into())).await.unwrap();
        let mut commands = state.incoming.lock().await;
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some((command,response)) = command else {break;};
                    let envelope = treer_protocol::CommandEnvelope {command_id:uuid::Uuid::new_v4().to_string(),workspace_id:"test".into(),command};
                    socket.send(axum::extract::ws::Message::Text(serde_json::to_string(&treer_protocol::ProxyMessage::Command {envelope}).unwrap().into())).await.unwrap();
                    while let Some(Ok(message)) = socket.next().await {
                        if let axum::extract::ws::Message::Text(text) = message {
                            if let treer_protocol::AgentServerMessage::CommandResult {result} = serde_json::from_str(&text).unwrap() {let _ = response.send(result); break;}
                        }
                    }
                },
                message = socket.next() => { if message.is_none() {break;} },
            }
        }
    })
}
async fn transfer_proxy_snapshot(
    axum::extract::State(state): axum::extract::State<TransferProxy>,
) -> axum::Json<serde_json::Value> {
    axum::Json(
        serde_json::json!({"revision":1,"workspace":{"workspace_id":"test","name":"test","created_at":"2026-01-01T00:00:00Z"},"servers":[state.server.lock().unwrap().clone().unwrap()],"agents":[]}),
    )
}
async fn transfer_proxy_info(
    axum::extract::State(state): axum::extract::State<TransferProxy>,
) -> axum::response::Response {
    state
        .relay(treer_protocol::AgentCommand::FileTransferInfo {})
        .await
}
async fn transfer_proxy_call(
    axum::extract::State(state): axum::extract::State<TransferProxy>,
    axum::Json(call): axum::Json<treer_protocol::MachineFileTransferCall>,
) -> axum::response::Response {
    state
        .relay(treer_protocol::AgentCommand::FileTransfer {
            actor: treer_protocol::MachineTransferActor {
                workspace_id: "test".into(),
                kind: "machine".into(),
                id: "test-server".into(),
            },
            authorized_path: call.path,
            request: call.request,
        })
        .await
}

#[tokio::test]
#[ignore = "requires freshly built sibling Controller and Host binaries"]
async fn machine_file_transfer_real_controller_cli_and_stdio_round_trip() {
    use axum::{routing::get, Router};
    let root = std::env::temp_dir().join(format!(
        "tf-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..10]
    ));
    std::fs::create_dir(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let binaries = std::path::PathBuf::from(env!("CARGO_BIN_EXE_treer"))
        .parent()
        .unwrap()
        .to_owned();
    let sibling = |name: &str| binaries.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let state = TransferProxy {
        commands: sender,
        incoming: std::sync::Arc::new(tokio::sync::Mutex::new(receiver)),
        server: Default::default(),
    };
    let app = Router::new()
        .route("/agent/connect", get(transfer_proxy_socket))
        .route(
            "/agent/workspaces/test/snapshot",
            get(transfer_proxy_snapshot),
        )
        .route(
            "/agent/workspaces/test/machines/test-server/file-transfer",
            get(transfer_proxy_info).post(transfer_proxy_call),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = listener.local_addr().unwrap();
    let proxy = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    #[cfg(unix)]
    let socket = root.join("host.sock");
    #[cfg(windows)]
    let socket = treer_platform::endpoint_name(
        &uuid::Uuid::new_v4().to_string(),
        &treer_platform::current_user_sid().unwrap(),
    );
    let controller_config = root.join("controller.json");
    let host_config = root.join("host.json");
    let hostname = String::from_utf8(
        std::process::Command::new("hostname")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    std::fs::write(&controller_config,serde_json::to_vec(&serde_json::json!({"proxy":format!("http://{proxy_address}"),"workspace":"test","server_id":"test-server","machine_token":"test-machine","operator_credential":"test-operator","root":root,"listen":address.to_string(),"host_socket":socket,"install_hostname":hostname.trim(),"network_mode":"plain"})).unwrap()).unwrap();
    std::fs::write(&host_config,serde_json::to_vec(&serde_json::json!({"socket_path":socket,"controller_path":sibling("treer-agent-server"),"controller_config_path":controller_config,"root":root})).unwrap()).unwrap();
    let mut host = tokio::process::Command::new(sibling("treer-agent-host"));
    #[cfg(unix)]
    host.process_group(0);
    let _processes = Processes(vec![host
        .args(["run", "--config"])
        .arg(&host_config)
        .envs(treer_platform::inherited_environment())
        .env_remove("TREER_AGENT_ID")
        .env_remove("TREER_WORKLOAD_CREDENTIAL")
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("LOCALAPPDATA", root.join("local"))
        .env("TREER_STATE_DIR", root.join("state"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap()]);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if state.server.lock().unwrap().is_some()
                && http
                    .get(format!("http://{address}/api/health"))
                    .send()
                    .await
                    .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let cli = |args: &[&str]| {
        let mut command = tokio::process::Command::new(sibling("treer"));
        command
            .args(args)
            .env_remove("TREER_AGENT_ID")
            .env_remove("TREER_WORKLOAD_CREDENTIAL")
            .env_remove("TREER_IDENTITY_MODE")
            .env("TREER_OPERATOR_CREDENTIAL", "test-operator")
            .env("TREER_AGENT_SERVER_URL", format!("http://{address}"))
            .env("TREER_WORKSPACE_ID", "test")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command
    };
    let bytes: Vec<u8> = (0..170000).map(|index| (index % 251) as u8).collect();
    let mut child = cli(&["machine", "put", "--machine", "self", "-", "remote.bin"])
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&bytes).await.unwrap();
    drop(stdin);
    let result = child.wait_with_output().await.unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(root.join("remote.bin")).unwrap(), bytes);
    let result = cli(&[
        "machine",
        "get",
        "--machine",
        "test-server",
        "remote.bin",
        "-",
    ])
    .output()
    .await
    .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, bytes);
    let result = cli(&[
        "machine",
        "get",
        "--machine",
        "test-server",
        "remote.bin",
        root.join("local.bin").to_str().unwrap(),
    ])
    .output()
    .await
    .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(root.join("local.bin")).unwrap(), bytes);
    let result = cli(&[
        "machine",
        "get",
        "--machine",
        "test-server",
        "controller.json",
        "-",
    ])
    .output()
    .await
    .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    proxy.abort();
    drop(_processes);
    let _ = std::fs::remove_dir_all(root);
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
