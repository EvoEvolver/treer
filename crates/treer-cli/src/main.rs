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
mod machine_file;
mod presentation;

#[derive(Debug, serde::Deserialize)]
struct Config {
    listen: String,
    workspace: String,
    #[serde(default)]
    install_hostname: String,
    #[serde(default)]
    operator_credential: String,
}
struct Connection {
    url: Url,
    headers: Vec<(&'static str, String)>,
    argv: Vec<String>,
}
fn config_directory() -> Option<PathBuf> {
    treer_platform::config_directory()
}
fn installed_config(
    directory: &std::path::Path,
    workspace: Option<&str>,
    explicit_url: Option<&str>,
) -> anyhow::Result<Option<Config>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot inspect installed Controllers"),
    };
    let hostname = std::process::Command::new("hostname")
        .output()
        .context("cannot determine this machine's installation hostname")?;
    anyhow::ensure!(hostname.status.success(), "hostname command failed");
    let hostname = String::from_utf8(hostname.stdout)?;
    let origin = explicit_url
        .map(Url::parse)
        .transpose()?
        .map(|url| url.origin());
    let mut matched = None;
    for entry in entries {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-controller.json"))
        {
            continue;
        }
        let config: Config = serde_json::from_slice(&fs::read(&path)?).with_context(|| {
            format!(
                "invalid installed Controller configuration {}",
                path.display()
            )
        })?;
        if (!config.install_hostname.is_empty() && config.install_hostname != hostname.trim())
            || workspace.is_some_and(|workspace| config.workspace != workspace)
            || origin.as_ref().is_some_and(|origin| {
                Url::parse(&format!("http://{}", config.listen))
                    .map_or(true, |url| &url.origin() != origin)
            })
        {
            continue;
        }
        anyhow::ensure!(
            matched.is_none(),
            "multiple installed Controllers match; select --workspace and, if necessary, --url"
        );
        matched = Some(config);
    }
    Ok(matched)
}
fn connection(raw: Vec<String>) -> anyhow::Result<Connection> {
    connection_with_environment(raw, |key| env::var(key).ok(), config_directory)
}

fn connection_with_environment(
    raw: Vec<String>,
    environment: impl Fn(&str) -> Option<String>,
    discover_directory: impl Fn() -> Option<PathBuf>,
) -> anyhow::Result<Connection> {
    let bound_workspace = environment("TREER_WORKSPACE_ID");
    let bound_url = environment("TREER_AGENT_SERVER_URL");
    let mut workspace = bound_workspace.clone();
    let mut explicit_url = bound_url.clone();
    let mut requested_workspace = None;
    let mut requested_url = None;
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
            requested_url = explicit_url.clone();
        } else if let Some(value) = arg.strip_prefix("--url=") {
            explicit_url = Some(value.into());
            requested_url = explicit_url.clone();
        } else if arg == "--workspace" {
            workspace = Some(iter.next().context("--workspace requires an ID")?);
            requested_workspace = workspace.clone();
        } else if let Some(value) = arg.strip_prefix("--workspace=") {
            workspace = Some(value.into());
            requested_workspace = workspace.clone();
        } else {
            literal = true;
            argv.push(arg);
        }
    }
    let agent = environment("TREER_AGENT_ID");
    let workload = environment("TREER_WORKLOAD_CREDENTIAL");
    let managed = agent.is_some()
        || workload.is_some()
        || environment("TREER_IDENTITY_MODE").as_deref() == Some("agent");
    let operator = environment("TREER_OPERATOR_CREDENTIAL").filter(|value| !value.is_empty());
    // An explicit connection flag must never redirect an environment-bound
    // workload/operator credential. Reject before discovery or network I/O.
    // Independent clients may select an endpoint when neither binding exists.
    // Partial bindings still fail closed rather than accepting a missing half.
    if (managed || operator.is_some()) && (bound_workspace.is_some() || bound_url.is_some()) {
        let label = |value: Option<&str>| {
            value
                .filter(|s| {
                    s.starts_with("ws_")
                        && s.len() <= 96
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                })
                .unwrap_or("(bound workspace)")
                .to_owned()
        };
        if let Some(requested) = requested_workspace.as_deref() {
            anyhow::ensure!(bound_workspace.as_deref() == Some(requested),
                "current identity belongs to workspace {}; cannot operate in {} with --workspace; use an operator terminal in the requested workspace with its own credentials",
                label(bound_workspace.as_deref()), label(Some(requested)));
        }
        if let Some(requested) = requested_url.as_deref() {
            let same = bound_url.as_deref().is_some_and(|bound| {
                matches!((Url::parse(bound), Url::parse(requested)), (Ok(a), Ok(b)) if a == b)
            });
            anyhow::ensure!(same,
                "current identity belongs to workspace {}; cannot change --url for this identity; use an operator terminal in the destination workspace with its own credentials",
                label(bound_workspace.as_deref()));
        }
    }
    let config = if managed || (operator.is_some() && explicit_url.is_some()) {
        None
    } else {
        discover_directory()
            .map(|path| installed_config(&path, workspace.as_deref(), explicit_url.as_deref()))
            .transpose()?
            .flatten()
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
        let credential = operator
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
                    let mut request = reqwest::Client::builder().no_proxy().build()?.post(url).header("Connection", "upgrade").header("Upgrade", "treer-tcp");
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
        println!("treer [--url URL] [--workspace ID] [--format json|table] <server command>\nOutput: status, agent ls and machine list default to table on stdout TTY; otherwise JSON.\nBusiness help: treer --help\nCredentials: TREER_AGENT_ID + TREER_WORKLOAD_CREDENTIAL (Agent), or TREER_OPERATOR_CREDENTIAL (operator).");
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
fn ensure_file_transfer_capability(
    health: &serde_json::Value,
    argv: &[String],
) -> anyhow::Result<()> {
    if argv.first().is_some_and(|arg| arg == "machine")
        && argv
            .get(1)
            .is_some_and(|arg| matches!(arg.as_str(), "put" | "get"))
    {
        anyhow::ensure!(
            health["cli_capabilities"]
                .as_array()
                .is_some_and(|caps| caps
                    .iter()
                    .any(|cap| cap == treer_protocol::CAPABILITY_MACHINE_FILE_TRANSFER)),
            "file_transfer_unsupported: upgrade the local Controller before transferring files"
        );
    }
    Ok(())
}

async fn run(raw: Vec<String>) -> anyhow::Result<i32> {
    let (raw, mut presentation) =
        presentation::Output::parse(raw, std::io::stdout().is_terminal())?;
    let connection = connection(raw)?;
    // Never let an older interpreter silently reinterpret sibling targets globally.
    let health: serde_json::Value = reqwest::Client::builder()
        .no_proxy()
        .build()?
        .get(connection.url.join("/api/health")?)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    ensure_path_target_capability(&health)?;
    ensure_prompt_delivery_capability(&health, &connection.argv)?;
    ensure_creation_placement_capability(&health, &connection.argv)?;
    ensure_file_transfer_capability(&health, &connection.argv)?;
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
                capabilities: vec![treer_protocol::CAPABILITY_MACHINE_FILE_TRANSFER.into()],
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
                let message = match message {
                    Some(Ok(message)) => message,
                    other => {
                        stdout.write_all(&presentation.finish(1)).await?;
                        stdout.flush().await?;
                        other.context("Controller disconnected; command was not retried")??
                    }
                };
                match message {
                    Message::Text(text) => match serde_json::from_str::<CliOutput>(&text)? {
                        CliOutput::Stdout { data } => { if let Some(data) = presentation.push(data) { stdout.write_all(&data).await?; stdout.flush().await?; } }
                        CliOutput::Stderr { data } => { stderr.write_all(&data).await?; stderr.flush().await?; }
                        CliOutput::Exit { code } => { stdout.write_all(&presentation.finish(code)).await?; stdout.flush().await?; return Ok(code); },
                        CliOutput::StdinReady { bytes } => { stdin_credit = stdin_credit.checked_add(bytes).context("stdin credit overflow")?; anyhow::ensure!(stdin_credit <= 8192, "invalid stdin capacity"); },
                        CliOutput::Session { path, terminal } => return relay_session(&connection.url, &connection.headers, &path, terminal, &mut stdin).await,
                        CliOutput::Forward { path, listen } => return forward(&connection.url, &connection.headers, &path, &listen).await,
                        CliOutput::FileTransfer { path, local, remote, download, overwrite, mode } => {
                            return machine_file::transfer(&connection.url, &connection.headers, machine_file::Options { path, local, remote, download, overwrite, mode }).await;
                        },
                    },
                    Message::Ping(data) => outgoing.send(Message::Pong(data)).await?,
                    Message::Close(_) => { stdout.write_all(&presentation.finish(1)).await?; stdout.flush().await?; bail!("Controller disconnected; command was not retried"); },
                    _ => {},
                }
            }
        }
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;
    // Even partial identity contexts must never inspect the test runner's
    // real installation while exercising connection discovery/fallback.
    fn connection_with_environment(
        raw: Vec<String>,
        environment: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Connection> {
        super::connection_with_environment(raw, environment, || None)
    }
    #[test]
    fn bound_identity_rejects_explicit_workspace_and_url_before_dispatch() {
        let context = |key: &str| match key {
            "TREER_WORKSPACE_ID" => Some("ws_one".into()),
            "TREER_AGENT_SERVER_URL" => Some("http://127.0.0.1:8790".into()),
            "TREER_AGENT_ID" => Some("ag_fixture".into()),
            "TREER_WORKLOAD_CREDENTIAL" => Some("synthetic-secret".into()),
            _ => None,
        };
        for argv in [
            vec!["--workspace", "ws_two", "status"],
            vec!["--workspace=ws_two", "exec"],
            vec!["--url", "http://127.0.0.1:8791", "status"],
            vec!["--url=https://user:password@other.invalid", "status"],
        ] {
            let error =
                connection_with_environment(argv.into_iter().map(str::to_owned).collect(), context)
                    .err()
                    .expect("must reject the connection override")
                    .to_string();
            assert!(error.contains("current identity belongs to workspace ws_one"));
            assert!(error.contains("operator terminal"));
            for secret in ["synthetic-secret", "password", "other.invalid"] {
                assert!(!error.contains(secret));
            }
        }
        for argv in [
            vec!["status"],
            vec!["--workspace", "ws_one", "status"],
            vec!["--url=http://127.0.0.1:8790/", "status"],
        ] {
            let connection =
                connection_with_environment(argv.into_iter().map(str::to_owned).collect(), context)
                    .unwrap();
            assert_eq!(connection.url.as_str(), "http://127.0.0.1:8790/");
            assert_eq!(connection.argv, ["status"]);
            assert_eq!(connection.headers.len(), 2);
        }
        let operator_context = |key: &str| match key {
            "TREER_AGENT_ID" | "TREER_WORKLOAD_CREDENTIAL" => None,
            "TREER_OPERATOR_CREDENTIAL" => Some("synthetic-operator".into()),
            _ => context(key),
        };
        assert!(connection_with_environment(
            vec!["--workspace=ws_two".into(), "status".into()],
            operator_context
        )
        .is_err());
        assert!(connection_with_environment(
            vec!["--url=http://127.0.0.1:8791".into(), "status".into()],
            operator_context
        )
        .is_err());
        assert!(connection_with_environment(vec!["status".into()], operator_context).is_ok());
        for missing in ["TREER_WORKSPACE_ID", "TREER_AGENT_SERVER_URL"] {
            let partial = |key: &str| {
                if key == missing {
                    None
                } else {
                    operator_context(key)
                }
            };
            assert!(connection_with_environment(
                vec!["--workspace=ws_two".into(), "status".into()],
                partial
            )
            .is_err());
            assert!(connection_with_environment(
                vec!["--url=http://127.0.0.1:8791".into(), "status".into()],
                partial
            )
            .is_err());
            assert!(connection_with_environment(vec!["status".into()], partial).is_ok());
        }
        let pure_operator =
            |key: &str| (key == "TREER_OPERATOR_CREDENTIAL").then(|| "synthetic-operator".into());
        let selected = connection_with_environment(
            vec![
                "--workspace=ws_two".into(),
                "--url=http://127.0.0.1:8791".into(),
                "status".into(),
            ],
            pure_operator,
        )
        .unwrap();
        assert_eq!(selected.url.as_str(), "http://127.0.0.1:8791/");
    }

    #[test]
    fn unbound_agent_can_select_endpoint_but_partial_bindings_fail_closed() {
        let context = |key: &str| match key {
            "TREER_IDENTITY_MODE" => Some("agent".into()),
            "TREER_AGENT_ID" => Some("ag_fixture".into()),
            "TREER_WORKLOAD_CREDENTIAL" => Some("synthetic-secret".into()),
            _ => None,
        };
        for argv in [
            vec!["--url", "http://127.0.0.1:8791", "status"],
            vec![
                "--workspace=ws_two",
                "--url=http://127.0.0.1:8791",
                "status",
            ],
        ] {
            let selected = super::connection_with_environment(
                argv.into_iter().map(str::to_owned).collect(),
                context,
                || panic!("independent Agent must not discover operator credentials"),
            )
            .unwrap();
            assert_eq!(selected.url.as_str(), "http://127.0.0.1:8791/");
            assert_eq!(selected.argv, ["status"]);
            assert_eq!(selected.headers.len(), 2);
        }
        for (binding, value, argv) in [
            (
                "TREER_WORKSPACE_ID",
                "ws_one",
                vec!["--url=http://127.0.0.1:8791", "status"],
            ),
            (
                "TREER_AGENT_SERVER_URL",
                "http://127.0.0.1:8790",
                vec!["--workspace=ws_two", "status"],
            ),
        ] {
            let error = super::connection_with_environment(
                argv.into_iter().map(str::to_owned).collect(),
                |key| {
                    if key == binding {
                        Some(value.into())
                    } else {
                        context(key)
                    }
                },
                || panic!("binding conflict must fail before discovery"),
            )
            .err()
            .expect("partial binding must reject the missing counterpart")
            .to_string();
            assert!(error.contains("current identity belongs to workspace"));
            assert!(!error.contains("synthetic-secret"));
            assert!(!error.contains("127.0.0.1"));
        }
    }

    #[test]
    fn installed_machine_configs_resolve_workspace_hostname_and_ambiguity() {
        let directory = env::temp_dir().join(format!("treer-cli-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let hostname = std::process::Command::new("hostname").output().unwrap();
        let hostname = String::from_utf8(hostname.stdout).unwrap();
        let write = |id: &str, workspace: &str, host: &str, port| {
            fs::write(directory.join(format!("{id}-controller.json")), serde_json::to_vec(&serde_json::json!({
                "workspace":workspace, "install_hostname":host.trim(), "listen":format!("127.0.0.1:{port}"), "operator_credential":id,
            })).unwrap()).unwrap();
        };
        write("srv_one", "ws_one", &hostname, 8790);
        write("srv_foreign", "ws_one", "another-machine", 8791);
        assert_eq!(
            installed_config(&directory, Some("ws_one"), None)
                .unwrap()
                .unwrap()
                .operator_credential,
            "srv_one"
        );
        assert!(installed_config(&directory, Some("missing"), None)
            .unwrap()
            .is_none());
        assert!(
            installed_config(&directory, None, Some("http://elsewhere:8790"))
                .unwrap()
                .is_none()
        );
        write("srv_two", "ws_two", &hostname, 8792);
        assert!(installed_config(&directory, None, None)
            .unwrap_err()
            .to_string()
            .contains("multiple installed Controllers"));
        assert_eq!(
            installed_config(&directory, None, Some("http://127.0.0.1:8792"))
                .unwrap()
                .unwrap()
                .workspace,
            "ws_two"
        );
        write("srv_duplicate", "ws_one", &hostname, 8793);
        assert!(installed_config(&directory, Some("ws_one"), None).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}

fn ensure_prompt_delivery_capability(
    health: &serde_json::Value,
    argv: &[String],
) -> anyhow::Result<()> {
    if argv.first().is_some_and(|arg| arg == "agent")
        && argv
            .get(1)
            .is_some_and(|arg| matches!(arg.as_str(), "prompt" | "queue"))
    {
        anyhow::ensure!(health["cli_capabilities"].as_array().is_some_and(|caps| caps.iter().any(|cap| cap == treer_protocol::CAPABILITY_AGENT_PROMPT_DELIVERY)),
            "queue_unsupported: Controller does not advertise agent.prompt-delivery.v1; upgrade it before using prompt/queue (no command was dispatched)");
    }
    Ok(())
}

fn ensure_creation_placement_capability(
    health: &serde_json::Value,
    argv: &[String],
) -> anyhow::Result<()> {
    let creating = matches!(argv.first().map(String::as_str), Some("agent" | "app"))
        && (argv
            .get(1)
            .is_some_and(|s| matches!(s.as_str(), "create" | "new"))
            || argv.get(1).is_some_and(|s| s == "admin")
                && argv.get(2).is_some_and(|s| s == "create")
            || argv.get(1).is_some_and(|s| s == "profile")
                && argv.get(2).is_some_and(|s| s == "launch")
            || argv.get(1).is_some_and(|s| s == "admin")
                && argv.get(2).is_some_and(|s| s == "profile")
                && argv.get(3).is_some_and(|s| s == "launch"));
    if creating {
        anyhow::ensure!(health["cli_capabilities"].as_array().is_some_and(|caps| caps.iter().any(|cap| cap == treer_protocol::CAPABILITY_WORKLOAD_CREATE_PLACEMENT)),
            "creation_placement_unsupported: Controller does not advertise workload.create-placement.v1; upgrade before creation (no command was dispatched)");
    }
    Ok(())
}

fn ensure_path_target_capability(health: &serde_json::Value) -> anyhow::Result<()> {
    anyhow::ensure!(health["cli_capabilities"].as_array().is_some_and(|caps| caps.iter().any(|cap| cap == treer_protocol::CAPABILITY_AGENT_PATH_TARGETS)),
        "unsupported_command: Controller does not advertise agent.path-targets.v1; upgrade the Controller before using this CLI (no command was dispatched)");
    Ok(())
}

#[cfg(test)]
mod path_capability_tests {
    #[test]
    fn old_controller_cannot_silently_place_new_workloads_at_root() {
        for args in [
            vec!["agent", "create"],
            vec!["agent", "admin", "create"],
            vec!["app", "create"],
            vec!["agent", "profile", "launch"],
        ] {
            let argv = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(
                super::ensure_creation_placement_capability(&serde_json::json!({}), &argv)
                    .unwrap_err()
                    .to_string()
                    .contains("creation_placement_unsupported")
            );
            assert!(super::ensure_creation_placement_capability(&serde_json::json!({"cli_capabilities":[treer_protocol::CAPABILITY_WORKLOAD_CREATE_PLACEMENT]}), &argv).is_ok());
        }
        assert!(super::ensure_creation_placement_capability(
            &serde_json::json!({}),
            &["app".into(), "list".into()]
        )
        .is_ok());
    }
    #[test]
    fn old_interpreter_cannot_silently_steer_a_default_queue_prompt() {
        let old = serde_json::json!({"cli_capabilities":["agent.path-targets.v1"]});
        for command in ["prompt", "queue"] {
            let argv = vec!["agent".into(), command.into(), "target".into()];
            assert!(super::ensure_prompt_delivery_capability(&old, &argv)
                .unwrap_err()
                .to_string()
                .contains("queue_unsupported"));
            assert!(super::ensure_prompt_delivery_capability(
                &serde_json::json!({"cli_capabilities":["agent.prompt-delivery.v1"]}),
                &argv
            )
            .is_ok());
        }
        assert!(super::ensure_prompt_delivery_capability(&old, &["status".into()]).is_ok());
    }
    #[test]
    fn old_controllers_fail_before_command_dispatch_without_breaking_a_session() {
        assert!(
            super::ensure_path_target_capability(&serde_json::json!({"status":"ok"}))
                .unwrap_err()
                .to_string()
                .contains("unsupported_command")
        );
        assert!(super::ensure_path_target_capability(
            &serde_json::json!({"cli_capabilities":["unknown"]})
        )
        .is_err());
        assert!(super::ensure_path_target_capability(
            &serde_json::json!({"cli_capabilities":["agent.path-targets.v1"]})
        )
        .is_ok());
    }
}
