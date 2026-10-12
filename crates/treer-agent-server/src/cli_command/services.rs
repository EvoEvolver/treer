use super::*;

pub(super) async fn run_network_command(
    _client: &ApiClient,
    command: NetworkCommand,
) -> anyhow::Result<Value> {
    match command {
        NetworkCommand::Connect { .. } => {
            bail!("network connect must run before API client initialization")
        }
    }
}

pub(super) async fn run_interface_command(
    client: &ApiClient,
    command: InterfaceCommand,
) -> anyhow::Result<Value> {
    match command {
        InterfaceCommand::Show => client.value(Method::GET, "api/interface", None).await,
        InterfaceCommand::Register {
            port,
            instance_id,
            capabilities,
            ui_path,
        } => {
            client
                .value(
                    Method::PUT,
                    "api/interface",
                    Some(serde_json::to_value(RegisterAgentInterfaceRequest {
                        protocol: AGENT_INTERFACE_PROTOCOL_V1.to_string(),
                        instance_id,
                        port,
                        capabilities,
                        ui_path,
                    })?),
                )
                .await
        }
        InterfaceCommand::Clear => client.value(Method::DELETE, "api/interface", None).await,
    }
}

pub(super) async fn register_service(
    client: &ApiClient,
    request: CreateMachineServiceRequest,
) -> anyhow::Result<Value> {
    if request.primary {
        if request.protocol != MachineServiceProtocol::Http {
            bail!("a primary endpoint requires --protocol http");
        }
        let agent_id = client
            .source_agent_id
            .as_deref()
            .context("--primary requires a managed Agent identity")?;
        let services = list_services(client).await?;
        let existing = primary_service(&services, agent_id)?.or_else(|| {
            services.iter().find(|service| {
                service.target_agent_id.as_deref() == Some(agent_id)
                    && service.name.eq_ignore_ascii_case(request.name.trim())
            })
        });
        if let Some(service) = existing {
            if service.server_id != request.server_id {
                bail!("an Agent's primary service cannot be moved to another machine");
            }
            // Reuse service.update ownership/Policy checks and preserve the ID.
            return client
                .value(
                    Method::PATCH,
                    &format!("api/services/{}", path_segment(&service.service_id)),
                    Some(
                        json!({"name": request.name, "target_host": request.target_host,
                    "target_port": request.target_port, "protocol": "http", "primary": true}),
                    ),
                )
                .await;
        }
    }
    client
        .value(
            Method::POST,
            "api/services",
            Some(serde_json::to_value(request)?),
        )
        .await
}

pub(super) async fn list_services(client: &ApiClient) -> anyhow::Result<Vec<MachineService>> {
    #[derive(serde::Deserialize)]
    struct Services {
        services: Vec<MachineService>,
    }
    Ok(client
        .request::<Services>(Method::GET, "api/services", None)
        .await?
        .services)
}

pub(super) fn primary_service<'a>(
    services: &'a [MachineService],
    agent_id: &str,
) -> anyhow::Result<Option<&'a MachineService>> {
    let mut matches = services
        .iter()
        .filter(|service| service.primary && service.target_agent_id.as_deref() == Some(agent_id));
    let Some(service) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() || service.protocol != MachineServiceProtocol::Http {
        bail!("Agent {agent_id} has an invalid primary HTTP service binding");
    }
    Ok(Some(service))
}

pub(super) async fn resolve_service_machine(
    client: &ApiClient,
    requested: Option<&str>,
) -> anyhow::Result<String> {
    match requested {
        Some(target) => Ok(resolve_machine(client, target).await?.server_id),
        None => std::env::var("TREER_SERVER_ID").context(
            "--machine is required outside a managed agent; managed agents default to their own machine",
        ),
    }
}
