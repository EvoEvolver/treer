mod controller;
mod host_client;
mod local_api;
mod network;
mod proxy;
#[cfg(target_os = "linux")]
mod sandbox;
mod service;
mod tui;

use std::io::{self, BufRead, IsTerminal, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args as ClapArgs, Parser, Subcommand};
use tracing::info;
use treer_protocol::{
    parse_machine_enrollment_key, ApiError, MachineEnrollmentRequest, MachineEnrollmentResponse,
};
use url::Url;
use uuid::Uuid;

use controller::ControllerRuntime;
use host_client::HostClient;

#[derive(Debug, Parser)]
#[command(name = "treer-agent-server", about = "Treer machine agent runtime")]
struct Args {
    /// Open the interactive local Controller dashboard.
    #[arg(long)]
    tui: bool,
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    server: ServerArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Connect this machine to a Proxy workspace")]
    Connect(ConnectArgs),
    #[command(about = "Download and hot-activate the latest Controller and treer CLI")]
    Update(UpdateArgs),
    #[command(hide = true, about = "Run from a saved service configuration")]
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    #[command(about = "Manage the host agent-server service")]
    Service(ServiceArgs),
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    SandboxExec(sandbox::ExecArgs),
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    SandboxChild(sandbox::ChildArgs),
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    SandboxAgent(sandbox::AgentArgs),
}

#[derive(Debug, ClapArgs)]
struct UpdateArgs {
    #[arg(long, default_value = "default")]
    workspace: String,
}

#[derive(Debug, ClapArgs)]
struct ServiceArgs {
    #[arg(long, default_value = "default", global = true)]
    workspace: String,
    #[command(subcommand)]
    command: ServiceCommand,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    #[command(about = "Start the installed service")]
    Start,
    #[command(about = "Stop the installed service")]
    Stop,
    #[command(about = "Restart the installed service")]
    Restart,
    #[command(about = "Hot restart only the replaceable Controller")]
    RestartController,
    #[command(about = "Show service-manager status")]
    Status,
    #[command(about = "Show service logs")]
    Logs {
        #[arg(long, default_value_t = 100)]
        lines: usize,
        #[arg(long)]
        follow: bool,
    },
    #[command(about = "Stop and remove the installed service")]
    Uninstall,
}

#[derive(Debug, ClapArgs)]
struct ConnectArgs {
    #[arg(long, env = "TREER_PROXY_URL", default_value = "http://127.0.0.1:8787")]
    proxy: Url,
    #[arg(long = "key", env = "TREER_ENROLLMENT_KEY")]
    enrollment_key: String,
    #[arg(long, env = "TREER_WORKSPACE_ROOT", default_value = ".")]
    root: PathBuf,
    #[arg(long, env = "TREER_AGENT_SERVER_LISTEN")]
    listen: Option<SocketAddr>,
    /// Set or replace the persistent machine name.
    #[arg(long, env = "TREER_MACHINE_NAME")]
    name: Option<String>,
    /// Disable prompts. Requires --accept-risk and, on first setup, --name.
    #[arg(long)]
    non_interactive: bool,
    /// Confirm that the persistent proxy and agent host may use this account's permissions.
    #[arg(long)]
    accept_risk: bool,
}

#[derive(Debug, Clone, ClapArgs)]
struct ServerArgs {
    #[arg(long, env = "TREER_PROXY_URL", default_value = "http://127.0.0.1:8787")]
    proxy: Url,
    #[arg(long, env = "TREER_WORKSPACE_ID", default_value = "default")]
    workspace: String,
    #[arg(long, env = "TREER_SERVER_ID")]
    server_id: Option<String>,
    #[arg(long, env = "TREER_MACHINE_TOKEN")]
    machine_token: Option<String>,
    #[arg(long, env = "TREER_WORKSPACE_ROOT", default_value = ".")]
    root: PathBuf,
    #[arg(
        long,
        env = "TREER_AGENT_SERVER_LISTEN",
        default_value = "127.0.0.1:8790"
    )]
    listen: SocketAddr,
    #[arg(long, env = "TREER_HOST_SOCKET", default_value = ".treer/host.sock")]
    host_socket: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "treer_agent_server=info".into()),
        )
        .init();
    let args = Args::parse();
    if args.tui {
        if args.command.is_some() {
            anyhow::bail!("--tui cannot be combined with a subcommand");
        }
        return tui::run(&args.server.workspace).await;
    }
    match args.command {
        None => run_server(args.server).await,
        Some(Command::Connect(connect)) => connect_machine(connect).await,
        Some(Command::Update(update)) => service::update(&update.workspace).await,
        Some(Command::Run { config }) => {
            let config = service::ServiceConfig::load(&config)?;
            run_server(ServerArgs {
                proxy: Url::parse(&config.proxy).context("invalid proxy URL in service config")?,
                workspace: config.workspace,
                server_id: Some(config.server_id),
                machine_token: Some(config.machine_token),
                root: config.root,
                listen: config
                    .listen
                    .parse()
                    .context("invalid listen address in service config")?,
                host_socket: config.host_socket,
            })
            .await
        }
        Some(Command::Service(service_args)) => {
            run_service_command(service_args).await?;
            Ok(())
        }
        #[cfg(target_os = "linux")]
        Some(Command::SandboxExec(sandbox_args)) => sandbox::run(sandbox_args).await,
        #[cfg(target_os = "linux")]
        Some(Command::SandboxChild(sandbox_args)) => sandbox::run_child(sandbox_args).await,
        #[cfg(target_os = "linux")]
        Some(Command::SandboxAgent(sandbox_args)) => sandbox::run_agent(sandbox_args).await,
    }
}

async fn run_server(args: ServerArgs) -> Result<()> {
    require_loopback_listen(args.listen)?;
    let root = std::fs::canonicalize(&args.root)
        .with_context(|| format!("invalid workspace root {}", args.root.display()))?;
    let server_id = match args.server_id {
        Some(server_id) => server_id,
        None => load_or_create_server_id(&root)?,
    };
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| server_id.clone());
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("failed to bind local API at {}", args.listen))?;
    let listen_address = listener.local_addr()?;
    let network = network::NetworkRuntime::bind_near(listen_address)
        .await
        .context("failed to bind local network proxy")?;
    let sandbox_executable = transparent_network_executable()?;
    let agent_server_url = agent_server_url(listen_address, sandbox_executable.is_some());
    let (host, host_events) = HostClient::connect(&args.host_socket).await?;
    let sync = host.sync(std::collections::BTreeMap::new()).await?;
    let (runtime, mut host_disconnected) = ControllerRuntime::from_sync(
        host,
        sync,
        host_events,
        controller::ControllerConfig {
            workspace_id: args.workspace.clone(),
            server_id: server_id.clone(),
            agent_server_url,
            network_proxy_url: network.proxy_url(),
            treer_binary: sibling_treer_binary(),
            sandbox_executable,
        },
    )
    .map_err(|error| anyhow::anyhow!(error.message))?;
    let proxy_http = normalize_http_url(args.proxy.clone())?;
    let proxy_ws = agent_websocket_url(args.proxy)?;
    let server = proxy::server_info(
        server_id.clone(),
        args.workspace.clone(),
        hostname,
        root.display().to_string(),
    );

    let proxy_client = proxy::ProxyClient::new(
        proxy_ws,
        args.machine_token.clone(),
        server,
        runtime.clone(),
        network.clone(),
    );
    let proxy_task = tokio::spawn(proxy_client.run_forever());

    let local_state = local_api::LocalApiState::new(
        proxy_http,
        args.workspace.clone(),
        server_id.clone(),
        args.machine_token,
        runtime,
    );
    let app = local_api::router(local_state);
    info!(
        address = %listen_address,
        network_proxy = %network.listen_address(),
        %server_id,
        workspace = %args.workspace,
        root = %root.display(),
        "treer agent server listening"
    );
    let result = tokio::select! {
        result = axum::serve(listener, app) => result.context("local API failed"),
        changed = host_disconnected.changed() => {
            changed.context("host disconnect watcher closed")?;
            anyhow::bail!("host connection closed")
        }
    };
    proxy_task.abort();
    result
}

fn agent_server_url(listen_address: SocketAddr, transparent: bool) -> String {
    let host = if transparent {
        network::SANDBOX_LOCAL_API_IP
    } else {
        "127.0.0.1"
    };
    format!("http://{host}:{}", listen_address.port())
}

fn transparent_network_executable() -> Result<Option<PathBuf>> {
    let mode = std::env::var("TREER_NETWORK_MODE").unwrap_or_else(|_| {
        if cfg!(target_os = "linux") {
            "transparent".to_string()
        } else {
            "proxy-env".to_string()
        }
    });
    match mode.as_str() {
        "proxy-env" => Ok(None),
        "transparent" if cfg!(target_os = "linux") => std::env::current_exe()
            .context("failed to locate Controller executable for network sandbox")
            .map(Some),
        "transparent" => anyhow::bail!(
            "transparent network mode is currently supported only on Linux; use a Linux container"
        ),
        _ => anyhow::bail!("TREER_NETWORK_MODE must be transparent or proxy-env"),
    }
}

async fn run_service_command(args: ServiceArgs) -> Result<()> {
    match args.command {
        ServiceCommand::Start => service::start(&args.workspace),
        ServiceCommand::Stop => service::stop(&args.workspace),
        ServiceCommand::Restart => service::restart(&args.workspace),
        ServiceCommand::RestartController => service::restart_controller(&args.workspace),
        ServiceCommand::Status => service::status(&args.workspace),
        ServiceCommand::Logs { lines, follow } => service::logs(&args.workspace, lines, follow),
        ServiceCommand::Uninstall => service::uninstall(&args.workspace),
    }
}

async fn connect_machine(args: ConnectArgs) -> Result<()> {
    if let Some(listen) = args.listen {
        require_loopback_listen(listen)?;
    }
    let enrollment = parse_machine_enrollment_key(&args.enrollment_key)
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let existing_identity = service::load_machine_identity()?;
    let mut input = io::BufReader::new(io::stdin());
    let mut output = io::stderr();
    let identity = prepare_machine_identity(
        &args,
        existing_identity,
        io::stdin().is_terminal(),
        &mut input,
        &mut output,
    )?;
    service::save_machine_identity(&identity)?;
    let proxy = normalize_http_url(args.proxy.clone())?;
    if let Some(mut config) = service::registered_config(&enrollment.workspace_id)? {
        let installed_proxy = normalize_http_url(
            Url::parse(&config.proxy).context("invalid proxy URL in installed service config")?,
        )?;
        if installed_proxy != proxy {
            anyhow::bail!(
                "workspace {} is already connected to {}; uninstall it before connecting the same workspace ID to {}",
                enrollment.workspace_id,
                installed_proxy,
                proxy
            );
        }
        bind_machine_identity(&proxy, &config, &identity).await?;
        let response = claim_machine_enrollment(&proxy, &args.enrollment_key, &identity).await?;
        if response.server_id != config.server_id {
            anyhow::bail!(
                "Proxy returned machine {} instead of the installed machine {}",
                response.server_id,
                config.server_id
            );
        }
        config.machine_token = response.machine_token;
        config.proxy = proxy.to_string();
        service::refresh_registration(config)?;
        println!(
            "treer: reusing machine {} for workspace {}",
            identity.name, enrollment.workspace_id
        );
        return Ok(());
    }
    service::preflight_registration(&enrollment.workspace_id)?;
    let root = std::fs::canonicalize(&args.root)
        .with_context(|| format!("invalid workspace root {}", args.root.display()))?;
    let response = claim_machine_enrollment(&proxy, &args.enrollment_key, &identity).await?;
    if response.workspace_id != enrollment.workspace_id {
        anyhow::bail!("Proxy returned a workspace that does not match the enrollment key");
    }
    let listen = service::resolve_listen(&response.workspace_id, args.listen).await?;
    let host_socket = service::host_socket_path(&response.workspace_id)?;
    service::register(service::ServiceConfig {
        proxy: proxy.to_string(),
        workspace: response.workspace_id.clone(),
        server_id: response.server_id,
        machine_token: response.machine_token,
        root,
        listen: listen.to_string(),
        host_socket,
    })?;
    service::start(&response.workspace_id)?;
    println!(
        "treer: connected workspace {} to {}",
        response.workspace_id, proxy
    );
    Ok(())
}

async fn claim_machine_enrollment(
    proxy: &Url,
    enrollment_key: &str,
    identity: &service::MachineIdentity,
) -> Result<MachineEnrollmentResponse> {
    let endpoint = proxy
        .join("api/machines/enroll")
        .context("failed to build Proxy enrollment URL")?;
    let response = reqwest::Client::new()
        .post(endpoint.clone())
        .bearer_auth(enrollment_key)
        .json(&MachineEnrollmentRequest {
            installation_id: identity.installation_id.clone(),
            name: identity.name.clone(),
        })
        .send()
        .await
        .with_context(|| format!("failed to connect to {endpoint}"))?;
    let status = response.status();
    if !status.is_success() {
        let error = response.json::<ApiError>().await.ok();
        let message = error
            .map(|error| error.error.message)
            .unwrap_or_else(|| format!("Proxy enrollment failed with HTTP {status}"));
        anyhow::bail!(message);
    }
    response
        .json()
        .await
        .context("Proxy returned an invalid enrollment response")
}

async fn bind_machine_identity(
    proxy: &Url,
    config: &service::ServiceConfig,
    identity: &service::MachineIdentity,
) -> Result<()> {
    let endpoint = proxy
        .join("agent/machine/identity")
        .context("failed to build machine identity URL")?;
    let response = reqwest::Client::new()
        .post(endpoint.clone())
        .bearer_auth(&config.machine_token)
        .json(&MachineEnrollmentRequest {
            installation_id: identity.installation_id.clone(),
            name: identity.name.clone(),
        })
        .send()
        .await
        .with_context(|| format!("failed to connect to {endpoint}"))?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let error = response.json::<ApiError>().await.ok();
    anyhow::bail!(
        "{}",
        error
            .map(|error| error.error.message)
            .unwrap_or_else(|| format!("Proxy identity binding failed with HTTP {status}"))
    )
}

fn prepare_machine_identity(
    args: &ConnectArgs,
    existing: Option<service::MachineIdentity>,
    terminal: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<service::MachineIdentity> {
    writeln!(output, "Treer setup security notice")?;
    writeln!(
        output,
        "Treer installs a persistent proxy and agent host that run with your user account's system permissions."
    )?;
    writeln!(
        output,
        "Workspace agents can execute commands and make network requests on this machine."
    )?;
    writeln!(
        output,
        "Run Treer in a dedicated account, VM, container, or other sandbox when possible."
    )?;

    if args.non_interactive {
        if !args.accept_risk {
            anyhow::bail!("--non-interactive requires --accept-risk");
        }
    } else {
        if !terminal {
            anyhow::bail!(
                "interactive setup requires a terminal; use --non-interactive --accept-risk for automation"
            );
        }
        if !args.accept_risk {
            write!(output, "Continue setup? [y/N] ")?;
            output.flush()?;
            let mut answer = String::new();
            input.read_line(&mut answer)?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                anyhow::bail!("setup cancelled");
            }
        }
    }

    let requested_name = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let name = if let Some(name) = requested_name {
        name.to_string()
    } else if let Some(identity) = existing.as_ref() {
        writeln!(output, "Reusing machine name: {}", identity.name)?;
        identity.name.clone()
    } else if args.non_interactive {
        anyhow::bail!("first-time non-interactive setup requires --name <machine-name>");
    } else {
        write!(output, "Machine name: ")?;
        output.flush()?;
        let mut name = String::new();
        input.read_line(&mut name)?;
        name.trim().to_string()
    };
    service::validate_machine_name(&name)?;
    Ok(match existing {
        Some(mut identity) => {
            identity.name = name;
            identity
        }
        None => service::MachineIdentity::new(name),
    })
}

fn require_loopback_listen(listen: SocketAddr) -> Result<()> {
    if !listen.ip().is_loopback() {
        anyhow::bail!(
            "the unauthenticated local agent API must listen on a loopback address, got {listen}"
        );
    }
    Ok(())
}

fn normalize_http_url(mut url: Url) -> Result<Url> {
    match url.scheme() {
        "http" | "https" => {}
        "ws" => {
            url.set_scheme("http")
                .map_err(|_| anyhow::anyhow!("invalid proxy URL scheme"))?;
        }
        "wss" => {
            url.set_scheme("https")
                .map_err(|_| anyhow::anyhow!("invalid proxy URL scheme"))?;
        }
        scheme => anyhow::bail!("unsupported proxy URL scheme {scheme}"),
    }
    url.set_path("/");
    url.set_query(None);
    Ok(url)
}

fn agent_websocket_url(mut url: Url) -> Result<Url> {
    match url.scheme() {
        "http" => url
            .set_scheme("ws")
            .map_err(|_| anyhow::anyhow!("invalid proxy URL scheme"))?,
        "https" => url
            .set_scheme("wss")
            .map_err(|_| anyhow::anyhow!("invalid proxy URL scheme"))?,
        "ws" | "wss" => {}
        scheme => anyhow::bail!("unsupported proxy URL scheme {scheme}"),
    }
    url.set_path("/agent/connect");
    url.set_query(None);
    Ok(url)
}

fn load_or_create_server_id(root: &Path) -> Result<String> {
    let state_dir = root.join(".treer");
    let path = state_dir.join("server-id");
    match std::fs::read_to_string(&path) {
        Ok(value) if !value.trim().is_empty() => return Ok(value.trim().to_string()),
        Ok(_) | Err(_) => {}
    }
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    let server_id = format!("srv_{}", Uuid::new_v4().simple());
    std::fs::write(&path, format!("{server_id}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(server_id)
}

fn sibling_treer_binary() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    service::installed_treer_binary(&executable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::io::Cursor;
    use treer_protocol::format_machine_enrollment_key;

    #[test]
    fn local_agent_api_only_accepts_loopback_addresses() {
        assert!(require_loopback_listen("127.0.0.1:8790".parse().expect("IPv4 loopback")).is_ok());
        assert!(require_loopback_listen("[::1]:8790".parse().expect("IPv6 loopback")).is_ok());
        assert!(require_loopback_listen("0.0.0.0:8790".parse().expect("unspecified")).is_err());
        assert!(
            require_loopback_listen("192.0.2.1:8790".parse().expect("public address")).is_err()
        );
    }

    #[test]
    fn transparent_agents_use_the_sandbox_local_api_route() {
        let address = "127.0.0.1:8790".parse().expect("local API address");
        assert_eq!(agent_server_url(address, true), "http://192.0.2.1:8790");
        assert_eq!(agent_server_url(address, false), "http://127.0.0.1:8790");
    }

    #[test]
    fn tui_mode_accepts_a_workspace() {
        let args =
            Args::try_parse_from(["treer-agent-server", "--tui", "--workspace", "workspace-a"])
                .expect("parse TUI mode");
        assert!(args.tui);
        assert!(args.command.is_none());
        assert_eq!(args.server.workspace, "workspace-a");
    }

    #[test]
    fn connect_command_gets_its_workspace_from_the_key() {
        let key = format_machine_enrollment_key(
            "workspace-a",
            "abc123",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("enrollment key");
        let args = Args::try_parse_from([
            "treer-agent-server",
            "connect",
            "--proxy",
            "https://treer.example",
            "--key",
            &key,
        ])
        .expect("parse connect command");
        let Some(Command::Connect(connect)) = args.command else {
            panic!("expected connect command");
        };
        assert_eq!(
            parse_machine_enrollment_key(&connect.enrollment_key)
                .expect("parse enrollment key")
                .workspace_id,
            "workspace-a"
        );
    }

    fn setup_args(name: Option<&str>, non_interactive: bool, accept_risk: bool) -> ConnectArgs {
        ConnectArgs {
            proxy: Url::parse("https://treer.example/").expect("proxy URL"),
            enrollment_key: "test-key".to_string(),
            root: PathBuf::from("."),
            listen: None,
            name: name.map(str::to_string),
            non_interactive,
            accept_risk,
        }
    }

    #[test]
    fn first_interactive_setup_confirms_risk_and_asks_for_a_name() {
        let args = setup_args(None, false, false);
        let mut input = Cursor::new(b"yes\nBuild machine\n");
        let mut output = Vec::new();
        let identity = prepare_machine_identity(&args, None, true, &mut input, &mut output)
            .expect("interactive setup");
        assert_eq!(identity.name, "Build machine");
        assert!(identity.installation_id.starts_with("mid_"));
        let output = String::from_utf8(output).expect("setup output");
        assert!(output.contains("persistent proxy and agent host"));
        assert!(output.contains("system permissions"));
        assert!(output.contains("sandbox"));
        assert!(output.contains("Continue setup?"));
        assert!(output.contains("Machine name:"));
    }

    #[test]
    fn later_setup_reuses_the_saved_machine_name() {
        let args = setup_args(None, false, false);
        let existing = service::MachineIdentity {
            installation_id: "mid_0123456789abcdef0123456789abcdef".to_string(),
            name: "Existing builder".to_string(),
        };
        let mut input = Cursor::new(b"y\n");
        let mut output = Vec::new();
        let identity =
            prepare_machine_identity(&args, Some(existing.clone()), true, &mut input, &mut output)
                .expect("repeat setup");
        assert_eq!(identity, existing);
        assert!(String::from_utf8(output)
            .expect("setup output")
            .contains("Reusing machine name: Existing builder"));
    }

    #[test]
    fn automated_setup_requires_explicit_risk_acceptance_and_first_name() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let missing_acceptance = prepare_machine_identity(
            &setup_args(Some("builder"), true, false),
            None,
            false,
            &mut input,
            &mut output,
        )
        .expect_err("risk acceptance is required");
        assert!(missing_acceptance.to_string().contains("--accept-risk"));

        let missing_name = prepare_machine_identity(
            &setup_args(None, true, true),
            None,
            false,
            &mut input,
            &mut output,
        )
        .expect_err("first setup name is required");
        assert!(missing_name.to_string().contains("--name"));

        let identity = prepare_machine_identity(
            &setup_args(Some("builder"), true, true),
            None,
            false,
            &mut input,
            &mut output,
        )
        .expect("explicit automated setup");
        assert_eq!(identity.name, "builder");
    }

    #[test]
    fn interactive_setup_refuses_to_guess_without_a_terminal() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = prepare_machine_identity(
            &setup_args(Some("builder"), false, false),
            None,
            false,
            &mut input,
            &mut output,
        )
        .expect_err("terminal is required");
        assert!(error
            .to_string()
            .contains("--non-interactive --accept-risk"));
    }

    #[test]
    fn update_command_accepts_a_workspace() {
        let args =
            Args::try_parse_from(["treer-agent-server", "update", "--workspace", "workspace-a"])
                .expect("parse update command");
        let Some(Command::Update(update)) = args.command else {
            panic!("expected update command");
        };
        assert_eq!(update.workspace, "workspace-a");
    }

    #[tokio::test]
    async fn enrollment_exchange_uses_a_bearer_key() {
        const KEY: &str = "enr_v1_776f726b73706163652d61_abc123.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let app = Router::new().route(
            "/api/machines/enroll",
            post(
                |headers: HeaderMap, Json(request): Json<MachineEnrollmentRequest>| async move {
                    let expected = format!("Bearer {KEY}");
                    assert_eq!(
                        headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok()),
                        Some(expected.as_str())
                    );
                    assert_eq!(
                        request.installation_id,
                        "mid_0123456789abcdef0123456789abcdef"
                    );
                    assert_eq!(request.name, "builder");
                    Json(MachineEnrollmentResponse {
                        workspace_id: "workspace-a".to_string(),
                        server_id: "srv_test".to_string(),
                        machine_token: "srv_test.secret".to_string(),
                    })
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test Proxy");
        let address = listener.local_addr().expect("test Proxy address");
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let response = claim_machine_enrollment(
            &Url::parse(&format!("http://{address}/")).expect("Proxy URL"),
            KEY,
            &service::MachineIdentity {
                installation_id: "mid_0123456789abcdef0123456789abcdef".to_string(),
                name: "builder".to_string(),
            },
        )
        .await
        .expect("claim enrollment");
        assert_eq!(response.workspace_id, "workspace-a");
        assert_eq!(response.server_id, "srv_test");
        server.abort();
    }
}
