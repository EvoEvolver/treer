use super::*;

pub(super) async fn run_app_command(
    client: &ApiClient,
    command: AppCommand,
) -> anyhow::Result<Value> {
    match command {
        AppCommand::List => client.value(Method::GET, "api/apps", None).await,
        AppCommand::Show { target } => {
            let target = workload_tree::resolve_target(client, &target).await?;
            client
                .value(
                    Method::GET,
                    &format!("api/apps/{}", path_segment(&target)),
                    None,
                )
                .await
        }
        AppCommand::Create {
            machine,
            name,
            folder,
            cwd,
            port,
            public,
            executable,
            args,
        } => {
            let server_id = resolve_service_machine(client, machine.as_deref()).await?;
            workload_tree::create_in_folder(
                client,
                folder.as_deref(),
                "api/apps",
                serde_json::to_value(CreateAppDeploymentRequest {
                    folder_id: None,
                    server_id: Some(server_id),
                    name,
                    command: executable,
                    args,
                    cwd,
                    port,
                    hostname: String::new(),
                    public,
                })?,
            )
            .await
        }
        AppCommand::Start { target } => app_action(client, &target, "start").await,
        AppCommand::Stop { target } => app_action(client, &target, "stop").await,
        AppCommand::Restart { target } => app_action(client, &target, "restart").await,
        AppCommand::Update {
            target,
            cwd,
            command,
            clear_args,
            args,
        } => {
            let target = workload_tree::resolve_target(client, &target).await?;
            let args = if clear_args || !args.is_empty() {
                Some(args)
            } else {
                None
            };
            if cwd.is_none() && command.is_none() && args.is_none() {
                anyhow::bail!(
                    "app update requires --cwd, --command, arguments after --, or --clear-args"
                );
            }
            client
                .value(
                    Method::POST,
                    &format!("api/apps/{}/update", path_segment(&target)),
                    Some(serde_json::to_value(
                        treer_protocol::UpdateAppDeploymentRequest { cwd, command, args },
                    )?),
                )
                .await
        }
        AppCommand::Delete { target } => {
            let target = workload_tree::resolve_target(client, &target).await?;
            client
                .value(
                    Method::DELETE,
                    &format!("api/apps/{}", path_segment(&target)),
                    None,
                )
                .await
        }
    }
}

pub(super) async fn app_action(
    client: &ApiClient,
    target: &str,
    action: &str,
) -> anyhow::Result<Value> {
    let target = workload_tree::resolve_target(client, target).await?;
    client
        .value(
            Method::POST,
            &format!("api/apps/{}/{}", path_segment(&target), action),
            Some(json!({})),
        )
        .await
}
