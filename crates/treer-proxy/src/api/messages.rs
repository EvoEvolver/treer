use super::*;

async fn browser_message_principal(
    auth: &AuthStore,
    session: &CurrentSession,
    workspace_id: &str,
) -> Result<(PolicySubject, MessagePrincipal), ApiFailure> {
    let role = auth
        .workspace_member_role(workspace_id, &session.user_id)
        .await?;
    Ok((
        PolicySubject::Human {
            user_id: session.user_id.clone(),
        },
        MessagePrincipal {
            app_endpoint: None,
            path: None,
            kind: MessagePrincipalKind::Human,
            id: session.user_id.clone(),
            name: session.preferred_name.clone(),
            role: Some(role),
        },
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_browser_message(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path(workspace_id): Path<String>,
    Json(request): Json<SendMessageRequest>,
) -> Result<Json<treer_protocol::SendMessageResponse>, ApiFailure> {
    let (subject, principal) = browser_message_principal(&auth, &session, &workspace_id).await?;
    Ok(Json(
        send_message_for(
            &state,
            &auth,
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            request,
        )
        .await?,
    ))
}

pub(super) async fn list_browser_messages(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path(workspace_id): Path<String>,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<treer_protocol::MessagePage>, ApiFailure> {
    let (subject, principal) = browser_message_principal(&auth, &session, &workspace_id).await?;
    Ok(Json(
        list_messages_for(&policy, &messages, &workspace_id, subject, principal, query).await?,
    ))
}

pub(super) async fn get_browser_message(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path((workspace_id, message_id)): Path<(String, String)>,
) -> Result<Json<GetMessageResponse>, ApiFailure> {
    let (subject, principal) = browser_message_principal(&auth, &session, &workspace_id).await?;
    Ok(Json(
        get_message_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            &message_id,
        )
        .await?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_app_message(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SendMessageRequest>,
) -> Result<Json<treer_protocol::SendMessageResponse>, ApiFailure> {
    let (workspace_id, subject, sender) =
        app_message_identity(&state, &auth, &identity, &headers, &service_id).await?;
    Ok(Json(
        send_message_for(
            &state,
            &auth,
            &policy,
            &messages,
            &workspace_id,
            subject,
            sender,
            request,
        )
        .await?,
    ))
}

pub(super) async fn get_app_message(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path((service_id, message_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<GetMessageResponse>, ApiFailure> {
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &headers, &service_id).await?;
    Ok(Json(
        get_message_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            &message_id,
        )
        .await?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn list_app_messages(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path(service_id): Path<String>,
    Query(query): Query<ListMessagesQuery>,
    headers: HeaderMap,
) -> Result<Json<treer_protocol::MessagePage>, ApiFailure> {
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &headers, &service_id).await?;
    Ok(Json(
        list_messages_for(&policy, &messages, &workspace_id, subject, principal, query).await?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn receive_app_messages(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ReceiveMessagesRequest>,
) -> Result<Json<treer_protocol::ReceiveMessagesResponse>, ApiFailure> {
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &headers, &service_id).await?;
    Ok(Json(
        receive_messages_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            request,
        )
        .await?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn acknowledge_app_messages(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AcknowledgeMessagesRequest>,
) -> Result<Json<treer_protocol::AcknowledgeMessagesResponse>, ApiFailure> {
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &headers, &service_id).await?;
    Ok(Json(
        acknowledge_messages_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            request,
        )
        .await?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_core_message(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Extension(machine): Extension<MachineSession>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SendMessageRequest>,
) -> Result<Json<treer_protocol::SendMessageResponse>, ApiFailure> {
    let (subject, sender) =
        message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    Ok(Json(
        send_message_for(
            &state,
            &auth,
            &policy,
            &messages,
            &workspace_id,
            subject,
            sender,
            request,
        )
        .await?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_message_for(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    messages: &MessageStore,
    workspace_id: &str,
    subject: PolicySubject,
    mut sender: MessagePrincipal,
    request: SendMessageRequest,
) -> Result<treer_protocol::SendMessageResponse, ApiFailure> {
    if request.recipients.is_empty() || request.recipients.len() > 32 {
        return Err(ApiFailure::bad_request(
            "message_recipients_invalid",
            "message requires 1-32 recipients",
        ));
    }
    let directory = workspace_app_principals(state, auth, workspace_id).await?;
    let tree = auth.workload_tree(workspace_id).await?;
    let caller = (sender.kind == MessagePrincipalKind::Agent).then_some(sender.id.as_str());
    if sender.kind == MessagePrincipalKind::Agent {
        sender.path = Some(tree.workload_path(&sender.id, &sender.name)?);
    }
    if let Some(endpoint) = request.sender_endpoint.as_deref() {
        if !treer_protocol::valid_message_app_endpoint(endpoint)
            || sender.kind != MessagePrincipalKind::Agent
            || auth
                .resolve_app_deployment_by_runtime(workspace_id, &sender.id)
                .await?
                .is_none()
        {
            return Err(ApiFailure::forbidden(
                "message_app_sender_required",
                "logical sender requires the owning Managed App",
            ));
        }
        sender.app_endpoint = Some(endpoint.to_string());
    }
    let mut recipients = Vec::with_capacity(request.recipients.len());
    for target in &request.recipients {
        let target = if matches!(target.trim(), "self" | ".") {
            sender.id.as_str()
        } else {
            target.trim()
        };
        let resolved = resolve_tree_principal(&directory, &tree, caller, target);
        let mut recipient = match resolved {
            Ok(principal) => MessagePrincipal::from(principal),
            Err(error) => {
                // Existing Tree nodes win. Otherwise mount a bounded relative
                // subtree only underneath a real Managed App, never an Agent.
                let mut addressed = None;
                for (index, _) in target.match_indices('/').rev() {
                    let (owner, suffix) = target.split_at(index);
                    let endpoint = &suffix[1..];
                    if owner.is_empty() || !treer_protocol::valid_message_app_endpoint(endpoint) {
                        continue;
                    }
                    if let Ok(principal) = resolve_tree_principal(&directory, &tree, caller, owner)
                    {
                        if principal.kind == AppPrincipalKind::Agent
                            && auth
                                .resolve_app_deployment_by_runtime(workspace_id, &principal.id)
                                .await?
                                .is_some()
                        {
                            addressed = Some((principal, endpoint));
                            break;
                        }
                    }
                }
                if let Some((owner, endpoint)) = addressed {
                    if owner.kind != AppPrincipalKind::Agent
                        || auth
                            .resolve_app_deployment_by_runtime(workspace_id, &owner.id)
                            .await?
                            .is_none()
                    {
                        return Err(message_recipient_unavailable());
                    }
                    let mut principal = MessagePrincipal::from(owner);
                    principal.app_endpoint = Some(endpoint.to_string());
                    principal
                } else if error.code == "agent_not_in_folder" {
                    return Err(ApiFailure::from(error));
                } else {
                    return Err(message_recipient_unavailable());
                }
            }
        };
        if recipient.kind == MessagePrincipalKind::Agent {
            recipient.path = Some(tree.workload_path(&recipient.id, &recipient.name)?);
        }
        recipients.push(recipient);
    }
    let recipient_count = recipients.len();
    let mut policy_requests = recipients
        .iter()
        .map(|recipient| {
            PolicyRequest::new(
                workspace_id,
                subject.clone(),
                ACTION_MESSAGE_SEND,
                message_mailbox_policy_resource(recipient),
            )
        })
        .collect::<Vec<_>>();
    policy_requests.extend(request.context_ids.iter().map(|context_id| {
        PolicyRequest::new(
            workspace_id,
            subject.clone(),
            ACTION_MESSAGE_READ,
            PolicyResource::new(RESOURCE_MESSAGE, context_id),
        )
    }));
    let authorization = policy
        .authorize_batch(&policy_requests)
        .await
        .map_err(|denial| {
            if denial.request_index < recipient_count {
                message_recipient_unavailable()
            } else {
                ApiFailure::from(denial.error)
            }
        })?;
    Ok(messages
        .send_with_policy_revision(
            workspace_id,
            &sender,
            &recipients,
            &request,
            authorization.revision,
        )
        .await?)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn get_core_message(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Extension(machine): Extension<MachineSession>,
    Path((workspace_id, message_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<GetMessageResponse>, ApiFailure> {
    let (subject, principal) =
        message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    Ok(Json(
        get_message_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            &message_id,
        )
        .await?,
    ))
}

pub(super) async fn get_message_for(
    policy: &PolicyEngine,
    messages: &MessageStore,
    workspace_id: &str,
    subject: PolicySubject,
    principal: MessagePrincipal,
    message_id: &str,
) -> Result<GetMessageResponse, ApiFailure> {
    authorize_control(
        policy,
        workspace_id,
        Some(&subject),
        ACTION_MESSAGE_READ,
        PolicyResource::new(RESOURCE_MESSAGE, message_id),
    )
    .await?;
    Ok(GetMessageResponse {
        message: messages.get(workspace_id, &principal, message_id).await?,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn list_core_messages(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Extension(machine): Extension<MachineSession>,
    Path(workspace_id): Path<String>,
    Query(query): Query<ListMessagesQuery>,
    headers: HeaderMap,
) -> Result<Json<treer_protocol::MessagePage>, ApiFailure> {
    let (subject, principal) =
        message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    Ok(Json(
        list_messages_for(&policy, &messages, &workspace_id, subject, principal, query).await?,
    ))
}

pub(super) async fn list_messages_for(
    policy: &PolicyEngine,
    messages: &MessageStore,
    workspace_id: &str,
    subject: PolicySubject,
    principal: MessagePrincipal,
    query: ListMessagesQuery,
) -> Result<treer_protocol::MessagePage, ApiFailure> {
    let address = match (query.app_owner_id.as_deref(), query.app_endpoint.as_deref()) {
        (None, None) => None,
        (Some(owner), Some(endpoint))
            if !owner.is_empty()
                && owner.len() <= 256
                && treer_protocol::valid_message_app_endpoint(endpoint) =>
        {
            Some((owner, endpoint))
        }
        _ => {
            return Err(ApiFailure::bad_request(
                "message_address_filter_invalid",
                "App owner and valid subtree path must be supplied together",
            ))
        }
    };
    authorize_control(
        policy,
        workspace_id,
        Some(&subject),
        ACTION_MESSAGE_READ,
        message_mailbox_policy_resource(&principal),
    )
    .await?;
    Ok(match address {
        Some(address) => {
            messages
                .list_app_conversation(
                    workspace_id,
                    &principal,
                    query.before.as_deref(),
                    query.limit,
                    Some(address),
                )
                .await?
        }
        None => {
            messages
                .list(
                    workspace_id,
                    &principal,
                    query.before.as_deref(),
                    query.limit,
                )
                .await?
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn receive_core_messages(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Extension(machine): Extension<MachineSession>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ReceiveMessagesRequest>,
) -> Result<Json<treer_protocol::ReceiveMessagesResponse>, ApiFailure> {
    let (subject, principal) =
        message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    Ok(Json(
        receive_messages_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            request,
        )
        .await?,
    ))
}

pub(super) async fn receive_messages_for(
    policy: &PolicyEngine,
    messages: &MessageStore,
    workspace_id: &str,
    subject: PolicySubject,
    principal: MessagePrincipal,
    request: ReceiveMessagesRequest,
) -> Result<treer_protocol::ReceiveMessagesResponse, ApiFailure> {
    authorize_control(
        policy,
        workspace_id,
        Some(&subject),
        ACTION_MESSAGE_RECEIVE,
        message_mailbox_policy_resource(&principal),
    )
    .await?;
    Ok(messages
        .receive(
            workspace_id,
            &principal,
            request.limit,
            request.wait_milliseconds,
        )
        .await?)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn acknowledge_core_messages(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Extension(machine): Extension<MachineSession>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AcknowledgeMessagesRequest>,
) -> Result<Json<treer_protocol::AcknowledgeMessagesResponse>, ApiFailure> {
    let (subject, principal) =
        message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    Ok(Json(
        acknowledge_messages_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            request,
        )
        .await?,
    ))
}

pub(super) async fn acknowledge_messages_for(
    policy: &PolicyEngine,
    messages: &MessageStore,
    workspace_id: &str,
    subject: PolicySubject,
    principal: MessagePrincipal,
    request: AcknowledgeMessagesRequest,
) -> Result<treer_protocol::AcknowledgeMessagesResponse, ApiFailure> {
    let policy_requests = request
        .delivery_ids
        .iter()
        .map(|delivery_id| {
            PolicyRequest::new(
                workspace_id,
                subject.clone(),
                ACTION_MESSAGE_ACK,
                PolicyResource::new(RESOURCE_MESSAGE_DELIVERY, delivery_id),
            )
        })
        .collect::<Vec<_>>();
    let authorization = policy
        .authorize_batch(&policy_requests)
        .await
        .map_err(|denial| ApiFailure::from(denial.error))?;
    Ok(messages
        .acknowledge_with_policy_revision(
            workspace_id,
            &principal,
            &request,
            authorization.revision,
        )
        .await?)
}

pub(super) async fn import_core_messages(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    machine: Option<Extension<MachineSession>>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ImportMessagesRequest>,
) -> Result<Json<treer_protocol::ImportMessagesResponse>, ApiFailure> {
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?
    .ok_or_else(|| {
        ApiFailure::forbidden(
            "message_import_denied",
            "message import requires a local operator",
        )
    })?;
    let PolicySubject::Machine { server_id } = &subject else {
        return Err(ApiFailure::forbidden(
            "message_import_denied",
            "message import requires a local operator",
        ));
    };
    authorize_control(
        &policy,
        &workspace_id,
        Some(&subject),
        ACTION_MESSAGE_IMPORT,
        PolicyResource::new(RESOURCE_MESSAGE_IMPORT, &workspace_id),
    )
    .await?;
    let importer = MessagePrincipal {
        app_endpoint: None,
        path: None,
        kind: MessagePrincipalKind::Machine,
        id: server_id.clone(),
        name: server_id.clone(),
        role: None,
    };
    Ok(Json(
        messages
            .import_legacy_mail(&workspace_id, &importer, &request)
            .await?,
    ))
}

pub(super) async fn message_request_principal(
    state: &AppState,
    machine: &MachineSession,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Result<(PolicySubject, MessagePrincipal), ApiFailure> {
    let subject = agent_policy_subject(state, machine, headers, workspace_id).await?;
    let PolicySubject::Agent { agent_id, .. } = &subject else {
        return Err(ApiFailure::unauthorized(
            "message_agent_required",
            "managed Agent identity is required for this Message operation",
        ));
    };
    let agent = state.resolve_agent(workspace_id, agent_id).await?;
    Ok((
        subject,
        MessagePrincipal {
            app_endpoint: None,
            path: None,
            kind: MessagePrincipalKind::Agent,
            id: agent.agent_id,
            name: agent.name,
            role: None,
        },
    ))
}

pub(super) fn message_mailbox_policy_resource(principal: &MessagePrincipal) -> PolicyResource {
    PolicyResource::new(RESOURCE_MESSAGE_MAILBOX, &principal.id)
        .with_attribute("principal_kind", principal.kind.as_str())
}

pub(super) fn message_recipient_unavailable() -> ApiFailure {
    ApiFailure::not_found(
        "message_recipient_unavailable",
        "a recipient does not exist or is not available to this sender",
    )
}
