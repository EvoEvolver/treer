use super::*;
#[allow(clippy::too_many_arguments)]
pub(crate) async fn exec_machine(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<MachineExecRequest>,
) -> Result<Json<Value>, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_EXEC,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    let audit_cwd = request.cwd.clone();
    let audit_timeout_ms = request.timeout_ms;
    let data = state
        .send_command(&workspace_id, &server_id, AgentCommand::Exec { request })
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.exec",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: None,
            payload: json!({
                "cwd": audit_cwd,
                "timeout_ms": audit_timeout_ms,
                "exit_code": data.get("exit_code"),
                "timed_out": data.get("timed_out"),
                "truncated": data.get("truncated"),
            }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record machine exec audit event");
    }
    Ok(Json(data))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload_machine_file(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<UploadMachineFileRequest>,
) -> Result<Json<Value>, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_FILE_WRITE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    if request.content_base64.len() > MAX_MACHINE_FILE_BYTES.div_ceil(3) * 4 + 4 {
        return Err(ApiFailure::bad_request(
            "machine_file_too_large",
            &format!("uploaded files may contain at most {MAX_MACHINE_FILE_BYTES} bytes"),
        ));
    }
    let contents = base64::engine::general_purpose::STANDARD
        .decode(&request.content_base64)
        .map_err(|_| {
            ApiFailure::bad_request("invalid_machine_file", "file content is not valid base64")
        })?;
    if contents.len() > MAX_MACHINE_FILE_BYTES {
        return Err(ApiFailure::bad_request(
            "machine_file_too_large",
            &format!("uploaded files may contain at most {MAX_MACHINE_FILE_BYTES} bytes"),
        ));
    }
    let upload_id = format!("upl_{}", Uuid::new_v4().simple());
    state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::UploadBegin {
                upload_id: upload_id.clone(),
                directory: request.directory.clone(),
                file_name: request.file_name.clone(),
                overwrite: request.overwrite,
            },
        )
        .await?;
    let upload_result = async {
        for chunk in contents.chunks(MACHINE_FILE_CHUNK_BYTES) {
            state
                .send_command(
                    &workspace_id,
                    &server_id,
                    AgentCommand::UploadChunk {
                        upload_id: upload_id.clone(),
                        content_base64: base64::engine::general_purpose::STANDARD.encode(chunk),
                    },
                )
                .await?;
        }
        let value = state
            .send_command(
                &workspace_id,
                &server_id,
                AgentCommand::UploadCommit {
                    upload_id: upload_id.clone(),
                },
            )
            .await?;
        serde_json::from_value::<UploadMachineFileResponse>(value).map_err(|error| {
            ProtocolError::new(
                "invalid_machine_upload_response",
                format!("Controller returned an invalid upload response: {error}"),
            )
        })
    }
    .await;
    let uploaded = match upload_result {
        Ok(uploaded) => uploaded,
        Err(error) => {
            let _ = state
                .send_command(
                    &workspace_id,
                    &server_id,
                    AgentCommand::UploadAbort {
                        upload_id: upload_id.clone(),
                    },
                )
                .await;
            return Err(error.into());
        }
    };
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.file.uploaded",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: None,
            payload: json!({
                "path": uploaded.path,
                "bytes_written": uploaded.bytes_written,
                "overwrite": request.overwrite,
            }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record machine upload audit event");
    }
    Ok(Json(serde_json::to_value(uploaded)?))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn rename_server(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_UPDATE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    let name = normalize_display_name(request.name)?;
    auth.set_machine_name(&workspace_id, &server_id, &name)
        .await?;
    let renamed = state
        .rename_server(&workspace_id, &server_id, name.clone())
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.renamed",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: Some(&name),
            payload: json!({}),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record runtime audit event");
    }
    Ok(Json(serde_json::to_value(renamed)?))
}

pub(crate) async fn delete_server(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let server = state.resolve_server(&workspace_id, &server_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_DELETE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    let agents = state
        .snapshot(&workspace_id)
        .await?
        .agents
        .into_iter()
        .filter(|agent| agent.server_id == server_id)
        .collect::<Vec<_>>();
    let stops = agents
        .iter()
        .filter(|agent| !agent.status.is_terminal())
        .map(|agent| async {
            tokio::time::timeout(
                Duration::from_secs(3),
                state.send_command(
                    &workspace_id,
                    &server_id,
                    AgentCommand::Stop {
                        agent_id: agent.agent_id.clone(),
                    },
                ),
            )
            .await
        });
    let _ = futures_util::future::join_all(stops).await;
    let shutdown_requested = if server.labels.get("treer.shutdown").map(String::as_str) == Some("1")
    {
        matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                state.send_command(&workspace_id, &server_id, AgentCommand::ShutdownMachine),
            )
            .await,
            Ok(Ok(_))
        )
    } else {
        false
    };
    let agent_ids = agents
        .iter()
        .map(|agent| agent.agent_id.clone())
        .collect::<Vec<_>>();
    auth.delete_machine(&workspace_id, &server_id, &agent_ids)
        .await?;
    let (server, deleted_agents) = state.delete_server(&workspace_id, &server_id).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.deleted",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: Some(&server.name),
            payload: json!({ "deleted_agent_count": deleted_agents.len(), "shutdown_requested": shutdown_requested }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record runtime audit event");
    }
    Ok(Json(json!({
        "server": server,
        "deleted_agents": deleted_agents,
        "shutdown_requested": shutdown_requested,
    })))
}
