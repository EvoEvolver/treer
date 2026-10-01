//! Transport-only client. Business argv is interpreted by the Controller.
use anyhow::{bail, Context};
use futures_util::{SinkExt, StreamExt};
use std::{env, fs, io::IsTerminal, path::PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};
use treer_protocol::{
    cli::{CliInput, CliOutput, CliRequest, CLI_PROTOCOL_VERSION},
    AGENT_ID_HEADER, OPERATOR_CREDENTIAL_HEADER, WORKLOAD_CREDENTIAL_HEADER,
};
use url::Url;

#[derive(serde::Deserialize)]
struct Config {
    listen: String,
    #[serde(default)]
    operator_credential: String,
}
struct Connection {
    url: Url,
    headers: Vec<(&'static str, String)>,
    argv: Vec<String>,
}
fn config_path(workspace: &str) -> Option<PathBuf> {
    let root = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let key: String = workspace
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(
        root.join("treer/agent-servers")
            .join(format!("{key}-controller.json")),
    )
}
fn connection(raw: Vec<String>) -> anyhow::Result<Connection> {
    let mut workspace = env::var("TREER_WORKSPACE_ID").unwrap_or_else(|_| "default".into());
    let mut explicit_url = env::var("TREER_AGENT_SERVER_URL").ok();
    let mut argv = Vec::new();
    let mut iter = raw.into_iter();
    let mut literal = false;
    while let Some(arg) = iter.next() {
        if literal {
            argv.push(arg);
            continue;
        }
        if arg == "--" {
            literal = true;
            argv.push(arg);
        } else if arg == "--url" {
            explicit_url = Some(iter.next().context("--url requires an address")?);
        } else if let Some(value) = arg.strip_prefix("--url=") {
            explicit_url = Some(value.into());
        } else if arg == "--workspace" {
            workspace = iter.next().context("--workspace requires an ID")?;
        } else if let Some(value) = arg.strip_prefix("--workspace=") {
            workspace = value.into();
        } else {
            literal = true;
            argv.push(arg);
        }
    }
    let agent = env::var("TREER_AGENT_ID").ok();
    let workload = env::var("TREER_WORKLOAD_CREDENTIAL").ok();
    let managed = agent.is_some()
        || workload.is_some()
        || env::var("TREER_IDENTITY_MODE").as_deref() == Ok("agent");
    let config = if managed {
        None
    } else {
        config_path(&workspace)
            .and_then(|path| fs::read(path).ok())
            .and_then(|data| serde_json::from_slice::<Config>(&data).ok())
    };
    let address = explicit_url
        .or_else(|| config.as_ref().map(|c| format!("http://{}", c.listen)))
        .unwrap_or_else(|| "http://127.0.0.1:8790".into());
    let mut headers = Vec::new();
    if managed {
        let agent = agent
            .filter(|v| !v.is_empty())
            .context("managed Agent identity is missing; operator fallback is disabled")?;
        let workload = workload
            .filter(|v| !v.is_empty())
            .context("Agent workload credential is missing; operator fallback is disabled")?;
        headers.push((AGENT_ID_HEADER, agent));
        headers.push((WORKLOAD_CREDENTIAL_HEADER, workload));
    } else {
        let credential = env::var("TREER_OPERATOR_CREDENTIAL")
            .ok()
            .or_else(|| config.map(|c| c.operator_credential))
            .filter(|v| !v.is_empty())
            .context("local operator credential is missing")?;
        headers.push((OPERATOR_CREDENTIAL_HEADER, credential));
    }
    Ok(Connection {
        url: Url::parse(&address)?,
        headers,
        argv,
    })
}

fn transport_url(base: &Url, path: &str) -> anyhow::Result<Url> {
    anyhow::ensure!(
        path.starts_with("/api/") && !path.contains(".."),
        "invalid server transport path"
    );
    let url = base.join(path)?;
    anyhow::ensure!(
        url.origin() == base.origin(),
        "cross-origin transport is forbidden"
    );
    Ok(url)
}
struct RawMode;
impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
async fn relay_session(
    base: &Url,
    headers: &[(&'static str, String)],
    path: &str,
    terminal: bool,
    stdin: &mut tokio::io::Stdin,
) -> anyhow::Result<i32> {
    anyhow::ensure!(
        !terminal || (std::io::stdin().is_terminal() && std::io::stdout().is_terminal()),
        "this session requires an interactive terminal"
    );
    let mut url = transport_url(base, path)?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid session URL"))?;
    let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 36));
    url.query_pairs_mut()
        .append_pair("cols", &cols.to_string())
        .append_pair("rows", &rows.to_string());
    let mut request = url.as_str().into_client_request()?;
    for (header, value) in headers {
        request
            .headers_mut()
            .insert(*header, HeaderValue::from_str(value)?);
    }
    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    let _raw = if terminal {
        crossterm::terminal::enable_raw_mode()?;
        Some(RawMode)
    } else {
        None
    };
    let (mut outgoing, mut incoming) = socket.split();
    let mut stdout = tokio::io::stdout();
    let mut buffer = [0u8; 4096];
    let mut stdin_open = true;
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    #[cfg(unix)]
    let mut resize = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    #[cfg(not(unix))]
    let mut resize = tokio::time::interval(std::time::Duration::from_millis(250));
    loop {
        tokio::select! {
            read = stdin.read(&mut buffer), if stdin_open => {
                let count = read?;
                if count == 0 { if terminal { return Ok(0); } stdin_open = false; continue; }
                if terminal { if let Some(index) = buffer[..count].iter().position(|byte| *byte == 0x1d) {
                    if index > 0 { outgoing.send(Message::Binary(buffer[..index].to_vec().into())).await?; }
                    outgoing.send(Message::Close(None)).await?;
                    return Ok(0);
                }}
                outgoing.send(Message::Binary(buffer[..count].to_vec().into())).await?;
            }
            _ = &mut interrupt, if !terminal => return Ok(130),
            _ = wait_resize(&mut resize), if terminal => {
                let (cols, rows) = crossterm::terminal::size()?;
                outgoing.send(Message::Text(serde_json::to_string(&treer_protocol::TerminalClientMessage::Resize { cols, rows })?.into())).await?;
            }
            message = incoming.next() => match message.context("session disconnected")?? {
                Message::Binary(data) => { stdout.write_all(&data).await?; stdout.flush().await?; }
                Message::Text(text) => match serde_json::from_str::<treer_protocol::TerminalServerMessage>(&text)? {
                    treer_protocol::TerminalServerMessage::Closed { exit_code, .. } => return Ok(exit_code.unwrap_or(0)),
                    treer_protocol::TerminalServerMessage::Error { error } => bail!("{}: {}", error.code, error.message),
                    _ => {},
                },
                Message::Ping(data) => outgoing.send(Message::Pong(data)).await?,
                Message::Close(_) => return Ok(0), _ => {},
            }
        }
    }
}
#[cfg(unix)]
async fn wait_resize(signal: &mut tokio::signal::unix::Signal) {
    signal.recv().await;
}
#[cfg(not(unix))]
async fn wait_resize(signal: &mut tokio::time::Interval) {
    signal.tick().await;
}
async fn forward(
    base: &Url,
    headers: &[(&str, String)],
    path: &str,
    listen: &str,
) -> anyhow::Result<i32> {
    let address: std::net::SocketAddr = listen
        .parse()
        .context("invalid forwarding listen address")?;
    anyhow::ensure!(
        address.ip().is_loopback(),
        "forward listeners must bind loopback"
    );
    let listener = tokio::net::TcpListener::bind(address).await?;
    let url = transport_url(base, path)?;
    let headers = headers
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect::<Vec<_>>();
    eprintln!("treer: forwarding on {}", listener.local_addr()?);
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(0),
            accepted = listener.accept() => {
                let (mut local, _) = accepted?; let url = url.clone(); let headers = headers.clone();
                connections.spawn(async move {
                    let mut request = reqwest::Client::new().post(url).header("Connection", "upgrade").header("Upgrade", "treer-tcp");
                    for (key, value) in headers { request = request.header(key, value); }
                    let response = request.send().await?;
                    anyhow::ensure!(response.status() == reqwest::StatusCode::SWITCHING_PROTOCOLS, "forward denied: {}", response.status());
                    let mut remote = response.upgrade().await?;
                    tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
                    Ok::<_, anyhow::Error>(())
                });
            }
            result = connections.join_next(), if !connections.is_empty() => {
                if let Some(result) = result { match result { Ok(Err(error)) => eprintln!("treer: forward: {error:#}"), Err(error) => eprintln!("treer: forward: {error}"), _ => {} } }
            }
        }
    }
}
#[tokio::main]
async fn main() {
    let raw: Vec<_> = env::args().skip(1).collect();
    if raw == ["--version"] || raw == ["-V"] {
        println!("treer {}", treer_build_info::DISPLAY);
        return;
    }
    if raw == ["--client-help"] {
        println!("treer [--url URL] [--workspace ID] <server command>\nBusiness help: treer --help\nCredentials: TREER_AGENT_ID + TREER_WORKLOAD_CREDENTIAL (Agent), or TREER_OPERATOR_CREDENTIAL (operator).");
        return;
    }
    match run(raw).await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("treer: {error:#}\nThe Controller must be online; use treer-agent-server for cold-start recovery.");
            std::process::exit(1);
        }
    }
}
async fn send<S>(outgoing: &mut S, value: &CliInput) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    outgoing
        .send(Message::Text(serde_json::to_string(value)?.into()))
        .await?;
    Ok(())
}
async fn run(raw: Vec<String>) -> anyhow::Result<i32> {
    let connection = connection(raw)?;
    let mut url = connection.url.join("/api/cli/session")?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => bail!("unsupported Controller URL scheme {other}"),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid Controller URL"))?;
    let mut request = url.as_str().into_client_request()?;
    for (header, value) in &connection.headers {
        request
            .headers_mut()
            .insert(*header, HeaderValue::from_str(value)?);
    }
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .context("cannot connect to Controller")?;
    let (mut outgoing, mut incoming) = socket.split();
    let terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 36));
    outgoing
        .send(Message::Text(
            serde_json::to_string(&CliRequest {
                version: CLI_PROTOCOL_VERSION,
                request_id: uuid::Uuid::new_v4().to_string(),
                argv: connection.argv,
                terminal,
                cols,
                rows,
            })?
            .into(),
        ))
        .await?;
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut input = [0u8; 8192];
    let mut stdin_open = true;
    let mut stdin_credit = 0usize;
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let mut cancelling = false;
    loop {
        tokio::select! {
            read = stdin.read(&mut input[..stdin_credit.min(8192)]), if stdin_open && stdin_credit > 0 => {
                let count = read?; if count == 0 { stdin_open = false; send(&mut outgoing, &CliInput::Eof).await?; }
                else { stdin_credit -= count; send(&mut outgoing, &CliInput::Stdin { data: input[..count].to_vec() }).await?; }
            }
            _ = &mut interrupt, if !cancelling => { cancelling = true; stdin_open = false; send(&mut outgoing, &CliInput::Cancel).await?; },
            message = incoming.next() => {
                let message = message.context("Controller disconnected; command was not retried")??;
                match message {
                    Message::Text(text) => match serde_json::from_str::<CliOutput>(&text)? {
                        CliOutput::Stdout { data } => { stdout.write_all(&data).await?; stdout.flush().await?; }
                        CliOutput::Stderr { data } => { stderr.write_all(&data).await?; stderr.flush().await?; }
                        CliOutput::Exit { code } => return Ok(code),
                        CliOutput::StdinReady { bytes } => { stdin_credit = stdin_credit.checked_add(bytes).context("stdin credit overflow")?; anyhow::ensure!(stdin_credit <= 8192, "invalid stdin capacity"); },
                        CliOutput::Session { path, terminal } => return relay_session(&connection.url, &connection.headers, &path, terminal, &mut stdin).await,
                        CliOutput::Forward { path, listen } => return forward(&connection.url, &connection.headers, &path, &listen).await,
                    },
                    Message::Ping(data) => outgoing.send(Message::Pong(data)).await?,
                    Message::Close(_) => bail!("Controller disconnected; command was not retried"),
                    _ => {},
                }
            }
        }
    }
}
