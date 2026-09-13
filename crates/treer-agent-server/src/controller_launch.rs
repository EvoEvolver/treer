use super::*;
pub(super) fn resolve_launch(
    request: &CreateAgentRequest,
) -> Result<(String, AgentLaunch), ProtocolError> {
    resolve_launch_with_path(request, &join_agent_path(None))
}

pub(super) fn resolve_launch_with_path(
    request: &CreateAgentRequest,
    search_path: &str,
) -> Result<(String, AgentLaunch), ProtocolError> {
    let mut launch = match request.kind.as_str() {
        "auto" => {
            let spec = first_available_interactive_agent(search_path)
                .unwrap_or_else(default_interactive_agent);
            shell_agent_launch(
                spec.command,
                spec.default_arg,
                &request.args,
                spec.confirm_workspace_trust,
                spec.install,
            )
        }
        "codex" | "claude" | "cursor" | "cursor-agent" | "grok" | "opencode" | "pi" => {
            let spec = interactive_agent_spec(&request.kind).ok_or_else(|| {
                ProtocolError::new(
                    "invalid_request",
                    format!("unsupported agent kind {}", request.kind),
                )
            })?;
            shell_agent_launch(
                spec.command,
                spec.default_arg,
                &request.args,
                spec.confirm_workspace_trust,
                spec.install,
            )
        }
        "shell" => request.args.split_first().map_or_else(
            || AgentLaunch {
                command: interactive_shell(),
                args: vec!["-i".to_string()],
                initial_writes: Vec::new(),
                publish_ports: Vec::new(),
            },
            |(command, args)| interactive_shell_command_launch(command, args),
        ),
        "command" | "app" | "bridge" => {
            let (command, args) = request.args.split_first().map_or_else(
                || (interactive_shell(), vec!["-i".to_string()]),
                |(command, args)| (command.clone(), args.to_vec()),
            );
            AgentLaunch {
                command,
                args,
                initial_writes: Vec::new(),
                publish_ports: Vec::new(),
            }
        }
        other => {
            return Err(ProtocolError::new(
                "invalid_request",
                format!("unsupported agent kind {other}"),
            ))
        }
    };
    launch.publish_ports = validate_publish_ports(&request.publish_ports)?;
    let kind = match request.kind.as_str() {
        "auto" => first_available_interactive_agent(search_path)
            .unwrap_or_else(default_interactive_agent)
            .kind
            .to_string(),
        "cursor-agent" => "cursor".to_string(),
        other => other.to_string(),
    };
    Ok((kind, launch))
}

pub(super) fn validate_publish_ports(ports: &[u16]) -> Result<Vec<u16>, ProtocolError> {
    if ports.len() > 32 {
        return Err(ProtocolError::new(
            "invalid_request",
            "at most 32 sandbox publish ports are allowed",
        ));
    }
    for port in ports {
        if *port == 0 {
            return Err(ProtocolError::new(
                "invalid_request",
                "sandbox publish port must not be 0",
            ));
        }
    }
    Ok(ports.to_vec())
}

#[derive(Clone, Copy)]
pub(super) struct InteractiveAgentSpec {
    pub(super) kind: &'static str,
    pub(super) command: &'static str,
    pub(super) default_arg: Option<&'static str>,
    pub(super) confirm_workspace_trust: bool,
    pub(super) install: Option<&'static str>,
}

pub(super) fn interactive_agent_specs() -> &'static [InteractiveAgentSpec] {
    &[
        InteractiveAgentSpec {
            kind: "claude",
            command: "claude",
            default_arg: Some("--dangerously-skip-permissions"),
            confirm_workspace_trust: true,
            install: Some("curl -fsSL https://claude.ai/install.sh | bash"),
        },
        InteractiveAgentSpec {
            kind: "cursor",
            command: "cursor-agent",
            default_arg: None,
            confirm_workspace_trust: false,
            install: Some("curl https://cursor.com/install -fsS | bash"),
        },
        InteractiveAgentSpec {
            kind: "grok",
            command: "grok",
            default_arg: Some("--always-approve"),
            confirm_workspace_trust: false,
            install: None,
        },
        InteractiveAgentSpec {
            kind: "opencode",
            command: "opencode",
            default_arg: None,
            confirm_workspace_trust: false,
            install: Some("npm install -g opencode-ai"),
        },
        InteractiveAgentSpec {
            kind: "pi",
            command: "pi",
            default_arg: None,
            confirm_workspace_trust: false,
            install: None,
        },
        InteractiveAgentSpec {
            kind: "codex",
            command: "codex",
            default_arg: Some("--dangerously-bypass-approvals-and-sandbox"),
            confirm_workspace_trust: false,
            install: Some("npm install -g @openai/codex"),
        },
    ]
}

pub(super) fn default_interactive_agent() -> InteractiveAgentSpec {
    *interactive_agent_specs()
        .iter()
        .find(|spec| spec.kind == "codex")
        .expect("codex is the default installer")
}

pub(super) fn interactive_agent_spec(kind: &str) -> Option<InteractiveAgentSpec> {
    let kind = if kind == "cursor-agent" {
        "cursor"
    } else {
        kind
    };
    interactive_agent_specs()
        .iter()
        .copied()
        .find(|spec| spec.kind == kind)
}

pub(super) fn first_available_interactive_agent(search_path: &str) -> Option<InteractiveAgentSpec> {
    interactive_agent_specs()
        .iter()
        .copied()
        .find(|spec| command_on_path(spec.command, search_path))
}

pub(super) fn command_on_path(command: &str, search_path: &str) -> bool {
    std::env::split_paths(search_path).any(|directory| {
        let candidate = directory.join(command);
        candidate.is_file()
    })
}

pub(super) fn join_agent_path(treer_binary: Option<&std::path::Path>) -> String {
    let mut paths = Vec::new();
    if let Some(parent) = treer_binary.and_then(std::path::Path::parent) {
        paths.push(parent.to_path_buf());
    }
    paths.extend(user_agent_path_dirs());
    if let Some(current_path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current_path));
    }
    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    std::env::join_paths(paths)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|_| std::env::var("PATH").unwrap_or_default())
}

pub(super) fn user_agent_path_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join(".grok/bin"));
        dirs.push(home.join("Library/pnpm"));
        dirs.push(home.join("Library/pnpm/bin"));
        dirs.push(home.join(".local/share/fnm/aliases/default/bin"));
        dirs.push(home.join(".fnm/aliases/default/bin"));
        dirs.push(home.join(".nvm/current/bin"));
        dirs.push(home.join(".npm-global/bin"));
    }
    dirs.push(std::path::PathBuf::from("/opt/homebrew/bin"));
    dirs.push(std::path::PathBuf::from("/usr/local/bin"));
    dirs
}

pub(super) fn shell_agent_launch(
    agent_command: &str,
    default_arg: Option<&str>,
    args: &[String],
    confirm_workspace_trust: bool,
    install: Option<&str>,
) -> AgentLaunch {
    let mut launch_args = Vec::new();
    if let Some(default_arg) = default_arg {
        launch_args.push(default_arg.to_string());
    }
    if agent_command == "codex" {
        launch_args.push("-c".to_string());
        launch_args.push("check_for_upgrades_on_startup=false".to_string());
    }
    launch_args.extend(args.iter().cloned());
    let agent_line = shell_join(agent_command, &launch_args);
    let mut script = if let Some(install) = install {
        format!(
            "if ! command -v {} >/dev/null 2>&1; then echo 'treer: installing missing {}' >&2; {}; fi; {}",
            shell_quote(agent_command),
            agent_command,
            install,
            agent_line
        )
    } else {
        agent_line.clone()
    };
    if agent_command == "codex" {
        // Codex's in-session npm updater exits 0 and drops to the login shell,
        // which then interprets leftover TUI queries as commands. Restart in
        // the same process so the installer prompt can land.
        script = format!("{script}; exec {agent_line}");
    }
    let mut input = script.into_bytes();
    input.push(b'\r');
    let mut launch = AgentLaunch {
        command: interactive_shell(),
        args: vec!["-i".to_string()],
        initial_writes: vec![HostWrite {
            data: input,
            delay_ms: AGENT_COMMAND_DELAY.as_millis() as u64,
        }],
        publish_ports: Vec::new(),
    };
    if confirm_workspace_trust {
        launch.initial_writes.push(HostWrite {
            data: vec![b'\r'],
            delay_ms: CLAUDE_TRUST_CONFIRM_DELAY.as_millis() as u64,
        });
    }
    launch
}

pub(super) fn interactive_shell_command_launch(command: &str, args: &[String]) -> AgentLaunch {
    let mut input = shell_join(command, args).into_bytes();
    input.push(b'\r');
    AgentLaunch {
        command: interactive_shell(),
        args: vec!["-i".to_string()],
        initial_writes: vec![HostWrite {
            data: input,
            delay_ms: AGENT_COMMAND_DELAY.as_millis() as u64,
        }],
        publish_ports: Vec::new(),
    }
}

pub(super) fn native_network_launch(
    helper: Option<&std::path::Path>,
    network_proxy_url: &str,
    agent_id: &str,
    launch: AgentLaunch,
) -> AgentLaunch {
    let Some(helper) = helper else {
        return launch;
    };
    // The signed helper registers its process instance with the extension and
    // waits for an acknowledgement before exec. No Host wire change is needed.
    let mut args = vec![
        "exec".to_string(),
        "--agent-id".to_string(),
        agent_id.to_string(),
        "--network-proxy".to_string(),
        network_proxy_url.to_string(),
        "--".to_string(),
        launch.command,
    ];
    args.extend(launch.args);
    AgentLaunch {
        command: helper.display().to_string(),
        args,
        initial_writes: launch.initial_writes,
        publish_ports: launch.publish_ports,
    }
}

pub(super) fn sandbox_launch(
    executable: Option<&std::path::Path>,
    network_proxy_url: &str,
    agent_id: &str,
    launch: AgentLaunch,
) -> AgentLaunch {
    let Some(executable) = executable else {
        return launch;
    };
    let mut args = vec![
        "sandbox-exec".to_string(),
        "--network-proxy".to_string(),
        network_proxy_url.to_string(),
        "--service-socket".to_string(),
        crate::network::agent_service_socket_path(agent_id)
            .display()
            .to_string(),
    ];
    for port in &launch.publish_ports {
        args.push("--publish".to_string());
        args.push(port.to_string());
    }
    args.push("--".to_string());
    args.push(launch.command);
    args.extend(launch.args);
    AgentLaunch {
        command: executable.display().to_string(),
        args,
        initial_writes: launch.initial_writes,
        publish_ports: launch.publish_ports,
    }
}

pub(super) fn should_replace_virtual_hosts(
    current: Option<&VirtualNetworkHostsSnapshot>,
    incoming: &VirtualNetworkHostsSnapshot,
) -> bool {
    current.is_none_or(|current| incoming.revision > current.revision)
}
