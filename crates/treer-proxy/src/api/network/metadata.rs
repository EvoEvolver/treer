use super::*;

pub(in crate::api) async fn publish_virtual_network_hosts(
    state: &AppState,
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<(), ApiFailure> {
    let snapshot = virtual_network_hosts_snapshot(auth, workspace_id).await?;
    state
        .broadcast_proxy_message(
            workspace_id,
            &treer_protocol::ProxyMessage::VirtualNetworkHosts { snapshot },
        )
        .await;
    Ok(())
}

pub(in crate::api) async fn refresh_service_ingress_routes(
    auth: &AuthStore,
) -> Result<(), ApiFailure> {
    auth.refresh_service_ingresses()
        .await
        .map_err(|error| ApiFailure::internal("ingress_refresh_failed", &format!("{error:#}")))
}

pub fn spawn_network_metadata_refresh(state: AppState, auth: AuthStore) {
    spawn_public_tcp_listeners(state.clone(), auth.clone());
    tokio::spawn(async move {
        let mut refresh = tokio::time::interval(std::time::Duration::from_secs(5));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        refresh.tick().await;
        loop {
            refresh.tick().await;
            if let Err(error) = auth.refresh_virtual_network_hosts().await {
                tracing::warn!(%error, "failed to reload virtual hosts");
                continue;
            }
            if let Err(error) = auth.refresh_service_ingresses().await {
                tracing::warn!(%error, "failed to reload service ingresses");
            }
            let Ok(workspaces) = auth.all_workspaces().await else {
                tracing::warn!("failed to list workspaces for virtual-host refresh");
                continue;
            };
            for workspace in workspaces {
                match virtual_network_hosts_snapshot(&auth, &workspace.workspace_id).await {
                    Ok(snapshot) => {
                        state
                            .broadcast_proxy_message(
                                &workspace.workspace_id,
                                &treer_protocol::ProxyMessage::VirtualNetworkHosts { snapshot },
                            )
                            .await;
                    }
                    Err(error) => {
                        let (_, error) = error.into_parts();
                        tracing::warn!(
                            workspace = %workspace.workspace_id,
                            message = %error.message,
                            "failed to refresh virtual hosts"
                        );
                    }
                }
            }
        }
    });
}
