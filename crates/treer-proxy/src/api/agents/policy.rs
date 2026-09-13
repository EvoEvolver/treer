use super::*;
pub(crate) async fn agent_policy_subject(
    state: &AppState,
    machine: &MachineSession,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Result<PolicySubject, ApiFailure> {
    let agent_id = headers
        .get(AGENT_ID_HEADER)
        .map(|value| {
            value
                .to_str()
                .map(str::trim)
                .map_err(|_| ProtocolError::new("invalid_agent_identity", "agent ID is invalid"))
        })
        .transpose()?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ProtocolError::new(
                "invalid_agent_identity",
                "managed agent identity is required",
            )
        })?;
    let agent = state.resolve_agent(workspace_id, agent_id).await?;
    if machine
        .server_id
        .as_ref()
        .is_some_and(|server_id| server_id != &agent.server_id)
    {
        return Err(ProtocolError::new(
            "policy_subject_mismatch",
            "agent does not belong to the authenticated machine",
        )
        .into());
    }
    Ok(PolicySubject::Agent {
        server_id: agent.server_id,
        agent_id: agent.agent_id,
    })
}

pub(crate) async fn control_policy_subject(
    state: &AppState,
    machine: Option<&MachineSession>,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Result<Option<PolicySubject>, ApiFailure> {
    let Some(machine) = machine else {
        return Ok(None);
    };
    if headers.contains_key(AGENT_ID_HEADER) {
        agent_policy_subject(state, machine, headers, workspace_id)
            .await
            .map(Some)
    } else {
        Ok(Some(PolicySubject::Machine {
            server_id: machine.server_id.clone().ok_or_else(|| {
                ProtocolError::new("machine_identity_required", "machine identity is required")
            })?,
        }))
    }
}

pub(crate) fn require_machine_target(
    subject: Option<&PolicySubject>,
    target_server_id: &str,
) -> Result<(), ApiFailure> {
    if let Some(PolicySubject::Machine { server_id }) = subject {
        if server_id != target_server_id {
            return Err(ProtocolError::new(
                "agent_identity_required",
                "cross-machine operations require an authenticated Agent workload credential",
            )
            .into());
        }
    }
    Ok(())
}

pub(crate) async fn authorize_control(
    policy: &PolicyEngine,
    workspace_id: &str,
    subject: Option<&PolicySubject>,
    action: &str,
    resource: PolicyResource,
) -> Result<(), ApiFailure> {
    let Some(subject) = subject else {
        return Ok(());
    };
    policy
        .authorize(&PolicyRequest::new(
            workspace_id,
            subject.clone(),
            action,
            resource,
        ))
        .await?;
    Ok(())
}

pub(crate) fn agent_policy_resource(agent: &AgentInfo) -> PolicyResource {
    PolicyResource::new(RESOURCE_AGENT, &agent.agent_id)
        .with_attribute("server_id", &agent.server_id)
}

pub(crate) fn policy_actor_name(subject: &PolicySubject) -> String {
    match subject {
        PolicySubject::Agent { agent_id, .. } => format!("agent:{agent_id}"),
        PolicySubject::Machine { server_id } => format!("machine:{server_id}"),
        PolicySubject::Human { user_id } => format!("human:{user_id}"),
        PolicySubject::Service { service_id } => format!("service:{service_id}"),
    }
}

pub(crate) fn launch_profile_policy_resource(profile_id: &str, name: &str) -> PolicyResource {
    PolicyResource::new(RESOURCE_AGENT_LAUNCH_PROFILE, profile_id).with_attribute("name", name)
}

pub(crate) fn profile_actor_label(
    session: Option<&CurrentSession>,
    subject: Option<&PolicySubject>,
) -> String {
    session.map_or_else(
        || {
            subject
                .map(policy_actor_name)
                .unwrap_or_else(|| "system".to_string())
        },
        |session| session.user_id.clone(),
    )
}

pub(crate) async fn prompt_installer_recipe(
    state: &AppState,
    workspace_id: &str,
    server_id: &str,
    agent_id: &str,
    recipe: &str,
) -> Result<(), ProtocolError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let output = state
            .send_command(
                workspace_id,
                server_id,
                AgentCommand::Read {
                    agent_id: agent_id.to_string(),
                    lines: Some(80),
                },
            )
            .await?;
        let text = output
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if text.contains("Do you trust") || text.contains("Press enter to continue") {
            state
                .send_command(
                    workspace_id,
                    server_id,
                    AgentCommand::Input {
                        agent_id: agent_id.to_string(),
                        data: vec![b'\r'],
                    },
                )
                .await?;
        } else if installer_composer_ready(text) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    state
        .send_command(
            workspace_id,
            server_id,
            AgentCommand::Prompt {
                agent_id: agent_id.to_string(),
                text: installer_base_prompt(recipe),
            },
        )
        .await?;
    Ok(())
}

pub(crate) fn queue_installer_recipe_prompt(
    state: AppState,
    workspace_id: String,
    server_id: String,
    agent_id: String,
    recipe: String,
) {
    tokio::spawn(async move {
        if let Err(error) =
            prompt_installer_recipe(&state, &workspace_id, &server_id, &agent_id, &recipe).await
        {
            tracing::warn!(
                ?error,
                %workspace_id,
                %agent_id,
                "failed to prompt installer with bundled skill"
            );
        }
    });
}

pub(crate) fn require_agent_can_probe_service(
    subject: &PolicySubject,
    service: &MachineService,
) -> Result<(), ApiFailure> {
    match subject {
        PolicySubject::Agent { server_id, .. } if server_id == &service.server_id => Ok(()),
        PolicySubject::Agent { .. } => Err(ApiFailure::forbidden(
            "service_not_owned",
            "agents may probe only services on their own machine",
        )),
        PolicySubject::Machine { .. }
        | PolicySubject::Human { .. }
        | PolicySubject::Service { .. } => Err(ApiFailure::forbidden(
            "ingress_agent_required",
            "a managed agent identity is required to probe a service",
        )),
    }
}

pub(crate) fn machine_service_policy_resource(
    service_id: &str,
    name: &str,
    server_id: &str,
    target_agent_id: Option<&str>,
    target_host: &str,
    target_port: u16,
) -> PolicyResource {
    let resource = PolicyResource::new(RESOURCE_MACHINE_SERVICE, service_id)
        .with_attribute("name", name)
        .with_attribute("server_id", server_id)
        .with_attribute("target_host", target_host)
        .with_attribute("target_port", target_port.to_string());
    if let Some(agent_id) = target_agent_id {
        resource.with_attribute("target_agent_id", agent_id)
    } else {
        resource
    }
}
