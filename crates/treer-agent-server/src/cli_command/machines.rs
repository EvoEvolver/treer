use super::*;

pub(super) async fn run_machine_command(
    client: &ApiClient,
    command: MachineCommand,
) -> anyhow::Result<Value> {
    match command {
        MachineCommand::List => {
            let snapshot: WorkspaceSnapshot =
                client.request(Method::GET, "api/discovery", None).await?;
            Ok(serde_json::to_value(snapshot.servers)?)
        }
        MachineCommand::Show { target } => Ok(serde_json::to_value(
            resolve_machine(client, &target).await?,
        )?),
        MachineCommand::Rename { target, name } => rename_machine(client, &target, name).await,
        MachineCommand::Delete { target } => delete_machine(client, &target).await,
    }
}
