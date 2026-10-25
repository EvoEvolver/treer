use super::*;

pub(super) async fn run_agent_command(
    client: &ApiClient,
    command: AgentCommand,
) -> anyhow::Result<Value> {
    match command {
        AgentCommand::Address { .. } => {
            unreachable!("address output is handled before JSON rendering")
        }
        AgentCommand::Mkdir { path, parents } => workload_tree::mkdir(client, &path, parents).await,
        AgentCommand::Ls { path, recursive } => workload_tree::ls(client, &path, recursive).await,
        AgentCommand::Mv {
            source,
            destination,
        } => workload_tree::mv(client, &source, &destination).await,
        AgentCommand::Rmdir { path } => workload_tree::rmdir(client, &path).await,
        AgentCommand::Start { target } => lifecycle_agent(client, &target, "start").await,
        AgentCommand::Stop { target } => stop_agent(client, &target).await,
        AgentCommand::Restart {
            target,
            cwd,
            command,
        } => {
            if command.is_empty() {
                lifecycle_agent(client, &target, "restart").await
            } else {
                replace_agent_startup(client, &target, cwd, command, true).await
            }
        }
        AgentCommand::Show { target } => {
            let target = workload_tree::resolve_target(client, &target).await?;
            client
                .value(
                    Method::GET,
                    &format!("api/agents/{}", path_segment(&target)),
                    None,
                )
                .await
        }
        AgentCommand::Attach { .. } => {
            bail!("terminal attach requires the thin client session transport")
        }
        AgentCommand::Prompt {
            target,
            text,
            queue: _,
            steer,
            notify,
            wait,
        } => prompt_and_maybe_wait(client, &target, text, steer, notify, wait).await,
        AgentCommand::Queue { command } => {
            let (target, id) = match command {
                AgentQueueCommand::Ls { target } => (target, None),
                AgentQueueCommand::Cancel { target, id } => (target, Some(id)),
            };
            let target = workload_tree::resolve_target(client, &target).await?;
            let path = format!(
                "api/agents/{}/queue{}",
                path_segment(&target),
                id.as_ref()
                    .map(|id| format!("/{}", path_segment(id)))
                    .unwrap_or_default()
            );
            client
                .value(
                    if id.is_some() {
                        Method::DELETE
                    } else {
                        Method::GET
                    },
                    &path,
                    None,
                )
                .await
        }
        AgentCommand::Read { target, lines } => read_agent(client, &target, lines).await,
        AgentCommand::Transcript {
            target,
            page,
            cursor,
            limit,
        } => read_agent_transcript(client, &target, page, cursor, limit).await,
        AgentCommand::SendKeys { target, keys } => {
            let target = workload_tree::resolve_target(client, &target).await?;
            let data = encode_keys(&keys)?;
            client
                .value(
                    Method::POST,
                    &format!("api/agents/{}/input", path_segment(&target)),
                    Some(serde_json::to_value(InputAgentRequest { data })?),
                )
                .await
        }
        AgentCommand::Wait {
            target,
            until,
            timeout,
        } => {
            let until = default_wait_statuses(until);
            Ok(serde_json::to_value(
                client.wait_for(&target, &until, timeout, None).await?,
            )?)
        }
        AgentCommand::Startup { command } => match command {
            AgentStartupCommand::Set {
                cwd,
                publish_ports,
                mut command,
            } => {
                if !publish_ports.is_empty() {
                    bail!("sandbox port publication is retired; register process ports with treer service register");
                }
                let executable = command.remove(0);
                client
                    .value(
                        Method::PUT,
                        "api/agent/startup",
                        Some(serde_json::to_value(SetAgentStartupRequest {
                            cwd,
                            command: executable,
                            args: command,
                            publish_ports,
                        })?),
                    )
                    .await
            }
            AgentStartupCommand::Show => client.value(Method::GET, "api/agent/startup", None).await,
            AgentStartupCommand::Clear => {
                client
                    .value(Method::DELETE, "api/agent/startup", None)
                    .await
            }
        },
        AgentCommand::Admin { command } => run_agent_admin_command(client, command).await,
    }
}

pub(super) async fn run_agent_admin_command(
    client: &ApiClient,
    command: AgentAdminCommand,
) -> anyhow::Result<Value> {
    match command {
        AgentAdminCommand::LocalInventory { ids } => local_agent_inventory(client, &ids).await,
        AgentAdminCommand::Startup { command } => match command {
            AgentAdminStartupCommand::Set {
                target,
                cwd,
                command,
            } => replace_agent_startup(client, &target, cwd, command, false).await,
            AgentAdminStartupCommand::Show { target } => {
                let target = workload_tree::resolve_target(client, &target).await?;
                client
                    .value(
                        Method::GET,
                        &format!("api/agents/{}/admin/startup", path_segment(&target)),
                        None,
                    )
                    .await
            }
        },
        AgentAdminCommand::Start { target } => lifecycle_agent(client, &target, "start").await,
        AgentAdminCommand::Restart { target } => lifecycle_agent(client, &target, "restart").await,
        AgentAdminCommand::Create {
            machine,
            kind,
            name,
            cwd,
            folder,
            publish_ports,
            recipe,
            args,
        } => {
            if recipe.is_some() {
                bail!("automatic recipe runtimes are retired; launch an explicit installer command or profile and supply the install skill");
            }
            if !publish_ports.is_empty() {
                bail!("sandbox port publication is retired; register process ports with treer service register");
            }
            let server_id = resolve_service_machine(client, machine.as_deref()).await?;
            workload_tree::create_in_folder(
                client,
                folder.as_deref(),
                "api/agents",
                serde_json::to_value(CreateAgentRequest {
                    folder_id: None,
                    server_id: Some(server_id),
                    kind,
                    name,
                    cwd,
                    args,
                    cols: 120,
                    rows: 36,
                    publish_ports,
                    recipe,
                })?,
            )
            .await
        }
        AgentAdminCommand::Rename { target, name } => rename_agent(client, &target, name).await,
        AgentAdminCommand::Stop { target } => stop_agent(client, &target).await,
        AgentAdminCommand::Delete { target } => delete_agent(client, &target).await,
        AgentAdminCommand::Profile { command } => run_profile_command(client, command).await,
    }
}

pub(super) async fn local_agent_inventory(
    client: &ApiClient,
    ids: &[String],
) -> anyhow::Result<Value> {
    // Never discover operator credentials or fall back from a managed identity.
    anyhow::ensure!(
        client.source_agent_id.is_none()
            && client.workload_credential.is_none()
            && client
                .operator_credential
                .as_ref()
                .is_some_and(|s| !s.is_empty()),
        "local_inventory_operator_required"
    );
    anyhow::ensure!(
        (1..=2).contains(&ids.len())
            && ids.iter().all(|id| id.starts_with("ag_")
                && id.len() > 3
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')),
        "local_inventory_requires_exact_agent_ids"
    );
    // This existing endpoint authenticates the operator and reads the live
    // local registry. Do not resolve through the Proxy workload tree.
    let inventory = client.value(Method::GET, "api/local/agents", None).await?;
    let agents = inventory["agents"]
        .as_array()
        .context("invalid local inventory response")?;
    let mut selected = Vec::new();
    for id in ids {
        let mut matches = agents.iter().filter(|agent| agent["agent_id"] == *id);
        let agent = matches.next();
        anyhow::ensure!(matches.next().is_none(), "duplicate local Agent identity");
        selected.push(match agent {
            Some(agent) => json!({"agent_id":id,"local_present":true,
                "server_id":agent["server_id"],"status":agent["status"],
                "run_generation":agent["run_generation"],"pid":agent["pid"]}),
            None => json!({"agent_id":id,"local_present":false,"code":"local_agent_absent"}),
        });
    }
    Ok(json!({"source":"local_controller_registry","agents":selected}))
}

pub(super) async fn lifecycle_agent(
    client: &ApiClient,
    target: &str,
    action: &str,
) -> anyhow::Result<Value> {
    let target = workload_tree::resolve_target(client, target).await?;
    client
        .value(
            Method::POST,
            &format!("api/agents/{}/{action}", path_segment(&target)),
            Some(json!({})),
        )
        .await
}

async fn replace_agent_startup(
    client: &ApiClient,
    target: &str,
    cwd: Option<String>,
    mut command: Vec<String>,
    restart: bool,
) -> anyhow::Result<Value> {
    let target = workload_tree::resolve_target(client, target).await?;
    let executable = command.remove(0);
    client
        .value(
            if restart { Method::POST } else { Method::PUT },
            &format!(
                "api/agents/{}/{}",
                path_segment(&target),
                if restart {
                    "restart-with-startup"
                } else {
                    "admin/startup"
                }
            ),
            Some(serde_json::to_value(ReplaceAgentStartupRequest {
                cwd,
                command: executable,
                args: command,
            })?),
        )
        .await
}
