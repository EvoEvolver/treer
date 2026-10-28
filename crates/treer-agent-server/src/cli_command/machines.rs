use super::*;

pub(super) async fn run_machine_command(
    client: &ApiClient,
    command: MachineCommand,
) -> anyhow::Result<Value> {
    match command {
        MachineCommand::Put { .. } | MachineCommand::Get { .. } => {
            anyhow::bail!(
                "file_transfer_unsupported: file transfer requires a compatible treer CLI session"
            )
        }
        MachineCommand::List => {
            let snapshot: WorkspaceSnapshot =
                client.request(Method::GET, "api/discovery", None).await?;
            Ok(serde_json::to_value(snapshot.servers)?)
        }
        MachineCommand::Show { target } => Ok(serde_json::to_value(
            resolve_machine(client, &target).await?,
        )?),
        // The local API resolves `self`, ids and unique names.
        MachineCommand::Metrics { target, window } => {
            client
                .value(
                    Method::GET,
                    &format!(
                        "api/machines/{}/metrics?window_seconds={window}",
                        path_segment(&target)
                    ),
                    None,
                )
                .await
        }
        MachineCommand::Rename { target, name } => rename_machine(client, &target, name).await,
        MachineCommand::Delete { target } => delete_machine(client, &target).await,
    }
}
