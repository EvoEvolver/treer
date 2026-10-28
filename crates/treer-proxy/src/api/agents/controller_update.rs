//! "Update machine M to release V". The Controller downloads and verifies the
//! signed release, replaces its binaries and asks its Host for a restart; the
//! replacement reporting V on reconnect completes the update. The Proxy only
//! remembers the requested version per machine, in memory.
use super::*;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use treer_protocol::controller_update::{
    binary_version, valid_version, UpdateRequest, UpdateState, UpdateStatus, CAPABILITY,
};

static REQUESTED: LazyLock<Mutex<HashMap<(String, String), String>>> =
    LazyLock::new(Default::default);

fn requested(workspace_id: &str, server_id: &str) -> Option<String> {
    REQUESTED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&(workspace_id.to_string(), server_id.to_string()))
        .cloned()
}

fn failure(status: StatusCode, code: &str, message: &str) -> ApiFailure {
    ApiFailure {
        status,
        error: ProtocolError::new(code, message),
    }
}

async fn authorize(
    state: &AppState,
    policy: &PolicyEngine,
    machine: Option<&MachineSession>,
    headers: &HeaderMap,
    workspace_id: &str,
    server_id: &str,
) -> Result<Option<PolicySubject>, ApiFailure> {
    // Replacing Controller binaries is code execution on the machine.
    let subject = control_policy_subject(state, machine, headers, workspace_id).await?;
    require_machine_target(subject.as_ref(), server_id)?;
    authorize_control(
        policy,
        workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_EXEC,
        PolicyResource::new(RESOURCE_MACHINE, server_id),
    )
    .await?;
    Ok(subject)
}

async fn controller_status(
    state: &AppState,
    workspace_id: &str,
    server_id: &str,
) -> Option<UpdateStatus> {
    let data = tokio::time::timeout(
        Duration::from_secs(5),
        state.send_command(
            workspace_id,
            server_id,
            AgentCommand::ControllerUpdateStatus {},
        ),
    )
    .await
    .ok()?
    .ok()?;
    serde_json::from_value(data).ok()
}

/// Combine the Controller's in-memory progress with the requested version.
fn combine(
    target: Option<String>,
    online: bool,
    controller_version: &str,
    reported: Option<UpdateStatus>,
) -> UpdateStatus {
    let state = match (reported.map(|status| status.state), &target) {
        (Some(state), _) if state != UpdateState::Idle => state,
        (_, None) => UpdateState::Idle,
        (_, Some(_)) if !online => UpdateState::Restarting,
        (_, Some(target)) if binary_version(target) == controller_version => UpdateState::Done,
        (_, Some(target)) => UpdateState::Failed {
            reason: format!(
                "Controller is running {controller_version} after the update, not {target}"
            ),
        },
    };
    UpdateStatus {
        state,
        target_version: target,
        controller_version: Some(controller_version.to_string()),
    }
}

async fn current_status(
    state: &AppState,
    workspace_id: &str,
    server_id: &str,
) -> Result<UpdateStatus, ApiFailure> {
    let server = state.resolve_server(workspace_id, server_id).await?;
    let online = server.status == ServerStatus::Online;
    let reported = if online {
        controller_status(state, workspace_id, server_id).await
    } else {
        None
    };
    Ok(combine(
        requested(workspace_id, server_id),
        online,
        &server.controller_build.version,
        reported,
    ))
}

pub(crate) async fn controller_update_status(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
) -> Result<Json<UpdateStatus>, ApiFailure> {
    let server_id = state
        .resolve_server(&workspace_id, &server_id)
        .await?
        .server_id;
    let machine = machine.as_ref().map(|value| &value.0);
    authorize(
        &state,
        &policy,
        machine,
        &headers,
        &workspace_id,
        &server_id,
    )
    .await?;
    Ok(Json(
        current_status(&state, &workspace_id, &server_id).await?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn controller_update_start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<UpdateRequest>,
) -> Result<(StatusCode, Json<UpdateStatus>), ApiFailure> {
    let server_id = state
        .resolve_server(&workspace_id, &server_id)
        .await?
        .server_id;
    let machine = machine.as_ref().map(|value| &value.0);
    let subject = authorize(
        &state,
        &policy,
        machine,
        &headers,
        &workspace_id,
        &server_id,
    )
    .await?;
    if !valid_version(&request.version) {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "controller_update_invalid_version",
            "version must be a release tag such as v1.2.3",
        ));
    }
    let capabilities = state.server_capabilities(&workspace_id, &server_id).await?;
    if !capabilities.contains(CAPABILITY) {
        return Err(failure(
            StatusCode::CONFLICT,
            "controller_update_unsupported",
            "This Controller predates remote updates; run `treer-agent-server update` on the machine once",
        ));
    }
    let data = tokio::time::timeout(
        Duration::from_secs(10),
        state.send_command(
            &workspace_id,
            &server_id,
            AgentCommand::ControllerUpdateStart {
                version: request.version.clone(),
            },
        ),
    )
    .await
    .map_err(|_| {
        failure(
            StatusCode::GATEWAY_TIMEOUT,
            "controller_update_unconfirmed",
            "The Controller did not answer; read the update status before retrying",
        )
    })??;
    REQUESTED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(
            (workspace_id.clone(), server_id.clone()),
            request.version.clone(),
        );
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.update",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: None,
            payload: json!({ "controller_update_version": request.version }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record Controller update audit event");
    }
    let mut status: UpdateStatus = serde_json::from_value(data).map_err(|_| {
        failure(
            StatusCode::BAD_GATEWAY,
            "controller_update_unconfirmed",
            "The Controller returned an invalid update status",
        )
    })?;
    status.target_version = Some(request.version);
    Ok((StatusCode::ACCEPTED, Json(status)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reported(state: UpdateState) -> Option<UpdateStatus> {
        Some(UpdateStatus {
            state,
            target_version: None,
            controller_version: None,
        })
    }

    #[test]
    fn status_follows_controller_progress_then_reconnect_version() {
        let target = || Some("v0.2.0".to_string());
        assert_eq!(combine(None, true, "0.1.0", None).state, UpdateState::Idle);
        assert_eq!(
            combine(target(), true, "0.1.0", reported(UpdateState::Installing)).state,
            UpdateState::Installing
        );
        // Offline between the old Controller exiting and the new one registering.
        assert_eq!(
            combine(target(), false, "0.1.0", None).state,
            UpdateState::Restarting
        );
        // A fresh Controller process reports idle; its version decides.
        assert_eq!(
            combine(target(), true, "0.2.0", reported(UpdateState::Idle)).state,
            UpdateState::Done
        );
        assert!(matches!(
            combine(target(), true, "0.1.0", reported(UpdateState::Idle)).state,
            UpdateState::Failed { .. }
        ));
        let failed = UpdateState::Failed {
            reason: "release signature verification failed".into(),
        };
        assert_eq!(
            combine(target(), true, "0.1.0", reported(failed.clone())).state,
            failed
        );
    }
}
