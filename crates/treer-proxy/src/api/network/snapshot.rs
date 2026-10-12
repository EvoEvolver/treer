use super::*;

pub(crate) async fn virtual_network_hosts_snapshot(
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<VirtualNetworkHostsSnapshot, auth::AuthFailure> {
    auth.virtual_network_hosts_snapshot(workspace_id).await
}
