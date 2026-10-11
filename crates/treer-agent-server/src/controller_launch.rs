use super::*;

pub(super) fn resolve_launch(
    request: &CreateAgentRequest,
) -> Result<(String, AgentLaunch), ProtocolError> {
    if !matches!(
        request.kind.as_str(),
        "shell" | "command" | "app" | "bridge"
    ) {
        return Err(ProtocolError::new(
            "agent_kind_retired",
            "Provider-specific and auto Agent kinds are retired; select a launch profile or pass kind=command with an explicit executable and argv",
        ));
    }
    validate_publish_ports(&request.publish_ports)?;
    let (command, args) = request.args.split_first().map_or_else(
        || (interactive_shell(), vec!["-i".to_string()]),
        |(command, args)| (command.clone(), args.to_vec()),
    );
    if command.trim().is_empty() {
        return Err(ProtocolError::new(
            "invalid_request",
            "Agent executable must not be empty",
        ));
    }
    Ok((
        request.kind.clone(),
        AgentLaunch {
            command,
            args,
            initial_writes: Vec::new(),
            publish_ports: Vec::new(),
        },
    ))
}

pub(super) fn validate_publish_ports(ports: &[u16]) -> Result<Vec<u16>, ProtocolError> {
    if !ports.is_empty() {
        return Err(ProtocolError::new(
            "sandbox_ports_retired",
            "Namespace port publication is retired; bind a Controller-reachable port and register it with treer service register",
        ));
    }
    Ok(Vec::new())
}

pub(super) fn join_agent_path(treer_binary: Option<&std::path::Path>) -> String {
    let mut paths = Vec::new();
    if let Some(parent) = treer_binary.and_then(std::path::Path::parent) {
        paths.push(parent.to_path_buf());
    }
    if let Some(current_path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current_path));
    }
    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    std::env::join_paths(paths)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|_| std::env::var("PATH").unwrap_or_default())
}

pub(super) fn should_replace_virtual_hosts(
    current: Option<&VirtualNetworkHostsSnapshot>,
    incoming: &VirtualNetworkHostsSnapshot,
) -> bool {
    current.is_none_or(|current| incoming.revision > current.revision)
}
