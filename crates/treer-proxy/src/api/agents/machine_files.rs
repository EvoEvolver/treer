use super::*;
use treer_protocol::{
    MachineFileTransferCall, MachineFileTransferRequest as Transfer, MachineTransferActor,
};

fn transfer_error(error: ProtocolError) -> ApiFailure {
    if error.code == "unsupported_command" {
        return ApiFailure { status: StatusCode::NOT_IMPLEMENTED, error: ProtocolError::new("file_transfer_unsupported", "target Controller does not support machine.file-transfer.v1; upgrade it before transferring files") };
    }
    error.into()
}

pub(crate) async fn machine_file_transfer_info(
    State(state): State<AppState>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    let subject =
        control_policy_subject(&state, machine.as_deref(), &headers, &workspace_id).await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    Ok(Json(
        state
            .send_command(&workspace_id, &server_id, AgentCommand::FileTransferInfo {})
            .await
            .map_err(transfer_error)?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn machine_file_transfer(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(call): Json<MachineFileTransferCall>,
) -> Result<Json<Value>, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    // These are filesystem request facts, never workspace Tree selectors. The
    // Controller checks the actual object and immutable handle path before I/O.
    if call.path.is_empty()
        || call.path.len() > 4096
        || call.path.contains(['\\', ':'])
        || call.path.chars().any(char::is_control)
        || call
            .path
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(ApiFailure::bad_request(
            "invalid_machine_path",
            "file path must be root-relative with normal components",
        ));
    }
    let mut subject =
        control_policy_subject(&state, machine.as_deref(), &headers, &workspace_id).await?;
    if subject.is_none() {
        if let Some(session) = session.as_deref() {
            subject = Some(PolicySubject::Human {
                user_id: session.user_id.clone(),
            });
        }
    }
    require_machine_target(subject.as_ref(), &server_id)?;
    let read = matches!(
        &call.request,
        Transfer::GetBegin { .. }
            | Transfer::GetChunk { .. }
            | Transfer::GetFinish { .. }
            | Transfer::Abort { read: true, .. }
    );
    let action = if read {
        crate::policy::ACTION_MACHINE_FILE_READ
    } else {
        ACTION_MACHINE_FILE_WRITE
    };
    let mut resource = PolicyResource::new(RESOURCE_MACHINE, &server_id)
        .with_attribute("file.path", &call.path)
        .with_attribute("file.direction", if read { "get" } else { "put" });
    if let Transfer::PutBegin {
        overwrite,
        mode,
        size,
        ..
    } = &call.request
    {
        resource = resource
            .with_attribute("file.overwrite", overwrite.to_string())
            .with_attribute("file.mode", format!("{mode:o}"));
        if let Some(size) = size {
            resource = resource.with_attribute("file.size", size.to_string());
        }
    }
    authorize_control(&policy, &workspace_id, subject.as_ref(), action, resource).await?;
    let (kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    let actor_id = actor_id.ok_or_else(|| {
        ApiFailure::unauthorized(
            "identity_required",
            "file transfer requires an authenticated caller",
        )
    })?;
    let actor = MachineTransferActor {
        workspace_id: workspace_id.clone(),
        kind: kind.into(),
        id: actor_id.into(),
    };
    let intent = matches!(
        &call.request,
        Transfer::PutBegin { .. } | Transfer::GetBegin { .. } | Transfer::PutFinish { .. }
    );
    if intent {
        // Durable intent precedes any read/commit; a later completion event is
        // separate because SQL and the destination filesystem are not atomic.
        auth.record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind: kind,
            actor_id: Some(actor_id),
            action: if read {
                "machine.file.read.intent"
            } else {
                "machine.file.write.intent"
            },
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: None,
            payload: json!({"path":call.path}),
        })
        .await
        .map_err(|_| {
            ApiFailure::service_unavailable(
                "audit_unavailable",
                "file transfer audit unavailable; operation not sent",
            )
        })?;
    }
    let finished = matches!(
        &call.request,
        Transfer::PutFinish { .. } | Transfer::GetFinish { .. }
    );
    let result = state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::FileTransfer {
                actor,
                authorized_path: call.path.clone(),
                request: call.request,
            },
        )
        .await
        .map_err(transfer_error);
    if finished || result.is_err() {
        let payload = match &result {
            Ok(data) => {
                let size = data
                    .get("size")
                    .and_then(Value::as_u64)
                    .filter(|size| *size <= treer_protocol::MACHINE_TRANSFER_HARD_LIMIT);
                let hash = data.get("sha256").and_then(Value::as_str).filter(|hash| {
                    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                });
                json!({"path":call.path,"size":size,"sha256":hash,"outcome":"done"})
            }
            Err(error) => json!({"path":call.path,"outcome":"failed","code":error.error.code}),
        };
        if auth
            .record_workspace_audit(NewWorkspaceAuditEvent {
                workspace_id: &workspace_id,
                actor_kind: kind,
                actor_id: Some(actor_id),
                action: if read {
                    "machine.file.read"
                } else {
                    "machine.file.write"
                },
                resource_kind: "machine",
                resource_id: &server_id,
                resource_name: None,
                payload,
            })
            .await
            .is_err()
        {
            tracing::warn!(%workspace_id, %server_id, "file transfer completion audit unavailable; durable intent retained");
        }
    }
    Ok(Json(result?))
}
