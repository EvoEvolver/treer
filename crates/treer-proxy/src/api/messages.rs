use super::*;

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
    sender: MessagePrincipal,
    request: SendMessageRequest,
) -> Result<treer_protocol::SendMessageResponse, ApiFailure> {
    if request.recipients.is_empty() || request.recipients.len() > 32 {
        return Err(ApiFailure::bad_request(
            "message_recipients_invalid",
            "message requires 1-32 recipients",
        ));
    }
    let directory = workspace_app_principals(state, auth, workspace_id).await?;
    let mut recipients = Vec::with_capacity(request.recipients.len());
    for target in &request.recipients {
        let target = if matches!(target.trim(), "self" | ".") {
            sender.id.as_str()
        } else {
            target.trim()
        };
        let recipient = resolve_app_principal(&directory, target)
            .map(MessagePrincipal::from)
            .map_err(|_| message_recipient_unavailable())?;
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
    authorize_control(
        policy,
        workspace_id,
        Some(&subject),
        ACTION_MESSAGE_READ,
        message_mailbox_policy_resource(&principal),
    )
    .await?;
    Ok(messages
        .list(
            workspace_id,
            &principal,
            query.before.as_deref(),
            query.limit,
        )
        .await?)
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
