use axum::extract::ws::{Message, WebSocket};
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use treer_protocol::cli::{CliInput, CliOutput, CliRequest, CLI_PROTOCOL_VERSION};

struct WriterTask(tokio::task::JoinHandle<()>);
impl Drop for WriterTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub async fn serve(
    mut socket: WebSocket,
    base: String,
    workspace: String,
    identity: Vec<(String, String)>,
) {
    if let Err(error) = run(&mut socket, base, workspace, identity).await {
        let _ = send(
            &mut socket,
            CliOutput::Stderr {
                data: format!("treer: {error}\n").into_bytes(),
            },
        )
        .await;
        let _ = send(&mut socket, CliOutput::Exit { code: 1 }).await;
    }
}
async fn send(socket: &mut WebSocket, event: CliOutput) -> anyhow::Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(&event)?.into()))
        .await?;
    Ok(())
}
async fn run(
    socket: &mut WebSocket,
    base: String,
    workspace: String,
    identity: Vec<(String, String)>,
) -> anyhow::Result<()> {
    let first = tokio::time::timeout(std::time::Duration::from_secs(15), socket.next())
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing command request"))??;
    let Message::Text(first) = first else {
        anyhow::bail!("expected command request");
    };
    let request: CliRequest = serde_json::from_str(&first)?;
    anyhow::ensure!(
        request.version == CLI_PROTOCOL_VERSION,
        "unsupported CLI protocol version"
    );
    anyhow::ensure!(!request.request_id.is_empty(), "request ID is required");
    let source_agent_id = identity
        .iter()
        .find(|(key, _)| key == "TREER_AGENT_ID")
        .map(|(_, value)| value.as_str());
    if let Some(action) = crate::cli_command::session_action(&request.argv, source_agent_id)? {
        send(socket, action).await?;
        return Ok(());
    }
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .arg("--execute-cli")
        .args(&request.argv)
        .env_remove("TREER_AGENT_ID")
        .env_remove("TREER_WORKLOAD_CREDENTIAL")
        .env_remove("TREER_OPERATOR_CREDENTIAL")
        .env_remove("TREER_MACHINE_TOKEN")
        .env_remove("TREER_ENROLLMENT_KEY")
        .env_remove("TREER_IDENTITY_MODE")
        .env_remove("TREER_GIT_PROXY_MODE")
        .env("TREER_AGENT_SERVER_URL", base)
        .env("TREER_WORKSPACE_ID", workspace)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in identity {
        command.env(key, value);
    }
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
    let (written_tx, mut written_rx) = tokio::sync::mpsc::unbounded_channel();
    let _writer = WriterTask(tokio::spawn(async move {
        while let Some(data) = stdin_rx.recv().await {
            if stdin.write_all(&data).await.is_err() {
                return;
            }
            let _ = written_tx.send(data.len());
        }
    }));
    let mut stdin_tx = Some(stdin_tx);
    let mut credit = 8192usize;
    send(socket, CliOutput::StdinReady { bytes: credit }).await?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut out = [0u8; 8192];
    let mut err = [0u8; 8192];
    let mut out_open = true;
    let mut err_open = true;
    let mut status = None;
    loop {
        tokio::select! {
            read = stdout.read(&mut out), if out_open => {
                let count = read?; out_open = count != 0;
                if count != 0 { send(socket, CliOutput::Stdout { data: out[..count].to_vec() }).await?; }
            }
            read = stderr.read(&mut err), if err_open => {
                let count = read?; err_open = count != 0;
                if count != 0 { send(socket, CliOutput::Stderr { data: err[..count].to_vec() }).await?; }
            }
            result = child.wait(), if status.is_none() => { status = Some(result?); }
            Some(bytes) = written_rx.recv() => {
                credit += bytes;
                send(socket, CliOutput::StdinReady { bytes }).await?;
            }
            message = socket.next() => match message {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<CliInput>(&text)? {
                    CliInput::Stdin { data } => {
                        anyhow::ensure!(data.len() <= credit, "stdin exceeds advertised capacity");
                        anyhow::ensure!(data.len() <= 8192, "stdin frame is too large");
                        credit -= data.len();
                        if let Some(stdin_tx) = &stdin_tx { stdin_tx.try_send(data).map_err(|_| anyhow::anyhow!("stdin queue unavailable"))?; }
                    },
                    CliInput::Eof => { stdin_tx.take(); },
                    CliInput::Cancel => { child.kill().await?; return send(socket, CliOutput::Exit { code: 130 }).await; },
                    CliInput::Resize { .. } => {},
                },
                Some(Ok(Message::Ping(data))) => socket.send(Message::Pong(data)).await?,
                Some(Ok(Message::Pong(_))) => {},
                _ => { child.kill().await?; return Ok(()); }
            }
        }
        if !out_open && !err_open && status.is_some() {
            break;
        }
    }
    send(
        socket,
        CliOutput::Exit {
            code: status.expect("child status").code().unwrap_or(1),
        },
    )
    .await
}
