use super::*;

pub(super) async fn run_profile_command(
    client: &ApiClient,
    command: ProfileCommand,
) -> anyhow::Result<Value> {
    match command {
        ProfileCommand::List => client.value(Method::GET, "api/launch-profiles", None).await,
        ProfileCommand::Show { target } => {
            let target = normalize_target(&target)?;
            client
                .value(
                    Method::GET,
                    &format!("api/launch-profiles/{}", path_segment(&target)),
                    None,
                )
                .await
        }
        ProfileCommand::Create {
            name,
            description,
            cwd,
            executable,
            args,
        } => {
            let request = CreateAgentLaunchProfileRequest {
                name,
                description,
                cwd,
                command: executable,
                args,
            };
            client
                .value(
                    Method::POST,
                    "api/launch-profiles",
                    Some(serde_json::to_value(request)?),
                )
                .await
        }
        ProfileCommand::Update {
            target,
            name,
            description,
            cwd,
            executable,
            args,
            clear_args,
        } => {
            let args = if clear_args {
                Some(Vec::new())
            } else if args.is_empty() {
                None
            } else {
                Some(args)
            };
            if name.is_none()
                && description.is_none()
                && cwd.is_none()
                && executable.is_none()
                && args.is_none()
            {
                bail!("profile update requires at least one changed field");
            }
            let target = normalize_target(&target)?;
            client
                .value(
                    Method::PATCH,
                    &format!("api/launch-profiles/{}", path_segment(&target)),
                    Some(serde_json::to_value(UpdateAgentLaunchProfileRequest {
                        name,
                        description,
                        cwd,
                        command: executable,
                        args,
                    })?),
                )
                .await
        }
        ProfileCommand::Delete { target } => {
            let target = normalize_target(&target)?;
            client
                .value(
                    Method::DELETE,
                    &format!("api/launch-profiles/{}", path_segment(&target)),
                    None,
                )
                .await
        }
        ProfileCommand::Launch {
            target,
            machine,
            name,
            cwd,
        } => {
            let target = normalize_target(&target)?;
            let server_id = match machine {
                Some(machine) => Some(resolve_machine(client, &machine).await?.server_id),
                None => None,
            };
            client
                .value(
                    Method::POST,
                    &format!("api/launch-profiles/{}/launch", path_segment(&target)),
                    Some(serde_json::to_value(LaunchAgentProfileRequest {
                        server_id,
                        agent_name: name,
                        cwd,
                        cols: 120,
                        rows: 36,
                    })?),
                )
                .await
        }
    }
}
