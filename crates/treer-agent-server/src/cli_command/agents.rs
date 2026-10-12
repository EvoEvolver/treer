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
            Ok(serde_json::to_value(client.get_agent(&target).await?)?)
        }
        AgentCommand::Attach { .. } => {
            bail!("terminal attach requires the thin client session transport")
        }
        AgentCommand::Prompt { target, text, wait } => {
            prompt_and_maybe_wait(client, &target, text, wait).await
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
