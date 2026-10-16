use super::*;

/// Human-session route only: neither the machine router nor the App identity
/// API exposes this operation. Error details are never ownership evidence.
pub(crate) async fn cleanup_agent_reservation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiFailure> {
    if !body.is_empty() && body.as_ref() != b"{}" {
        return Err(ApiFailure::bad_request(
            "invalid_request",
            "An empty cleanup request is required",
        ));
    }
    auth.require_workspace_member(&workspace_id, &session.user_id)
        .await?;
    // Exact owned record lookup first, before machine/resource lookups. Other
    // users, workspaces and nonexistent IDs receive the same uninformative 404.
    let (server_id, _) = auth
        .owned_agent_reservation(&workspace_id, &agent_id, &session.user_id)
        .await?;
    authorize_control(
        &policy,
        &workspace_id,
        Some(&PolicySubject::Human {
            user_id: session.user_id.clone(),
        }),
        ACTION_AGENT_DELETE,
        PolicyResource::new(RESOURCE_AGENT, &agent_id).with_attribute("server_id", &server_id),
    )
    .await?;
    let guard = auth
        .workload_lifecycle_guard(&workspace_id, &agent_id)
        .await?;
    let (_, already_deleted) = auth
        .owned_agent_reservation(&workspace_id, &agent_id, &session.user_id)
        .await?;
    if !already_deleted {
        if state
            .snapshot(&workspace_id)
            .await?
            .agents
            .iter()
            .any(|agent| agent.agent_id == agent_id)
            || auth
                .resolve_app_deployment_by_runtime(&workspace_id, &agent_id)
                .await?
                .is_some()
        {
            return Err(observed());
        }
        let confirmed = state
            .send_command(
                &workspace_id,
                &server_id,
                AgentCommand::FenceAbsentAgentReservation {
                    agent_id: agent_id.clone(),
                },
            )
            .await
            .map_err(|error| match error.code.as_str() {
                "unsupported_command" => ApiFailure {
                    status: StatusCode::NOT_IMPLEMENTED,
                    error: ProtocolError::new(
                        "reservation_cleanup_unsupported",
                        "Controller does not support reservation cleanup",
                    ),
                },
                "agent_reservation_observed" => observed(),
                _ => ApiFailure::service_unavailable(
                    "reservation_cleanup_unavailable",
                    "Reservation absence and fencing could not be confirmed",
                ),
            })?;
        if confirmed.get("fenced").and_then(Value::as_bool) != Some(true) {
            return Err(observed());
        }
    }
    // This transaction rechecks original ownership, applies normal deletion
    // semantics, and inserts the audit row atomically. On DB failure the fence
    // remains, but credentials/name stay reserved; an explicit retry is safe.
    let repeated = auth
        .cleanup_agent_reservation(&workspace_id, &agent_id, &session.user_id)
        .await.map_err(|error| {
            // The fence is already durable. Make the recovery path explicit
            // without claiming that the PG transaction committed.
            let mut failure = ApiFailure::from(error);
            failure.error.hint = Some(vec!["The Agent ID is fenced. Explicitly retry this cleanup request to finish bookkeeping; do not retry Agent creation.".into()]);
            failure
        })?;
    guard.commit().await.map_err(|_| {
        ApiFailure::internal("lifecycle_lock_failed", "Cleanup lifecycle lock failed")
    })?;
    auth.refresh_virtual_network_hosts().await.map_err(|_| {
        ApiFailure::internal(
            "virtual_host_refresh_failed",
            "Cleanup routing refresh failed",
        )
    })?;
    refresh_service_ingress_routes(&auth).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(
        json!({"agent_id":agent_id,"deleted":true,"already_deleted":repeated}),
    ))
}

fn observed() -> ApiFailure {
    ApiFailure {
        status: StatusCode::CONFLICT,
        error: ProtocolError::new(
            "agent_reservation_observed",
            "Agent has workload state or absence is unconfirmed; use normal lifecycle controls",
        ),
    }
}
