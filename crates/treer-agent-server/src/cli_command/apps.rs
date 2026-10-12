use super::*;

pub(super) async fn run_app_command(
    client: &ApiClient,
    command: AppCommand,
) -> anyhow::Result<Value> {
    match command {
        AppCommand::List => client.value(Method::GET, "api/apps", None).await,
        AppCommand::Show { target } => {
            let target = normalize_target(&target)?;
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
            cwd,
            port,
            public,
            executable,
            args,
        } => {
            let server_id = resolve_service_machine(client, machine.as_deref()).await?;
            client
                .value(
                    Method::POST,
                    "api/apps",
                    Some(serde_json::to_value(CreateAppDeploymentRequest {
                        server_id: Some(server_id),
                        name,
                        command: executable,
                        args,
                        cwd,
                        port,
                        hostname: String::new(),
                        public,
                    })?),
                )
                .await
        }
        AppCommand::Start { target } => app_action(client, &target, "start").await,
        AppCommand::Stop { target } => app_action(client, &target, "stop").await,
        AppCommand::Restart { target } => app_action(client, &target, "restart").await,
        AppCommand::Delete { target } => {
            let target = normalize_target(&target)?;
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
    let target = normalize_target(target)?;
    client
        .value(
            Method::POST,
            &format!("api/apps/{}/{}", path_segment(&target), action),
            Some(json!({})),
        )
        .await
}
