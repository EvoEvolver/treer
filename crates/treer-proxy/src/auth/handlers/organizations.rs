use super::*;

pub(crate) async fn preview_invitation(
    Extension(auth): Extension<AuthStore>,
    peer: Option<Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    Json(request): Json<treer_protocol::InvitationTokenRequest>,
) -> Result<Response, AuthFailure> {
    let source = peer
        .map(|Extension(axum::extract::ConnectInfo(peer))| peer.ip().to_string())
        .unwrap_or_else(|| "unavailable".into());
    auth.reserve_invitation_attempt(&source).await?;
    let preview = auth.preview_invitation(&request.token).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(preview)).into_response())
}

async fn invitation_session(
    auth: &AuthStore,
    headers: &HeaderMap,
) -> Result<CurrentSession, AuthFailure> {
    if headers.contains_key(header::AUTHORIZATION) {
        return Err(AuthFailure::unauthorized(
            "authentication_required",
            "browser session cookie required",
        ));
    }
    let cookie = cookie_value(headers, SESSION_COOKIE).ok_or_else(|| {
        AuthFailure::unauthorized("authentication_required", "browser session cookie required")
    })?;
    auth.session(&cookie).await?.ok_or_else(|| {
        AuthFailure::unauthorized("authentication_required", "browser session cookie required")
    })
}

pub(crate) async fn pending_invitation(
    Extension(auth): Extension<AuthStore>,
    headers: HeaderMap,
) -> Result<Response, AuthFailure> {
    let session = invitation_session(&auth, &headers).await?;
    let response = match auth.pending_invitation(&session).await? {
        Some(value) => Json(value).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], response).into_response())
}

pub(crate) async fn discard_pending_invitation(
    Extension(auth): Extension<AuthStore>,
    headers: HeaderMap,
) -> Result<Response, AuthFailure> {
    require_console_origin(&auth, &headers)?;
    let session = invitation_session(&auth, &headers).await?;
    auth.discard_pending_invitation(&session).await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub(crate) async fn accept_invitation(
    Extension(auth): Extension<AuthStore>,
    policy: Option<Extension<crate::policy::PolicyEngine>>,
    peer: Option<Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
    Json(request): Json<treer_protocol::InvitationAcceptRequest>,
) -> Result<Response, AuthFailure> {
    // This is deliberately cookie-only, unlike generic native/operator APIs.
    // Short-lived service-audience tokens cannot authorize joining an org.
    require_console_origin(&auth, &headers)?;
    let session = invitation_session(&auth, &headers).await?;
    let source = peer
        .map(|Extension(axum::extract::ConnectInfo(peer))| peer.ip().to_string())
        .unwrap_or_else(|| "unavailable".into());
    auth.reserve_invitation_attempt(&source).await?;
    let token = match (request.token, request.pending_id) {
        (Some(token), None) => token,
        (None, Some(id)) => auth.consume_pending_invitation(&session, &id).await?,
        _ => return Err(invalid_invitation()),
    };
    let accepted = auth
        .accept_invitation_with(
            &token,
            &session.user_id,
            policy.as_ref().map(|Extension(policy)| policy),
        )
        .await?;
    auth.discard_pending_invitation(&session).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(accepted)).into_response())
}
pub(crate) async fn organizations(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "organizations": auth.list_organizations(&session.user_id).await?
    })))
}

pub(crate) async fn create_organization_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Json(request): Json<CreateOrganizationRequest>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "organization": auth.create_organization(&session.user_id, &request.name).await?
    })))
}

pub(crate) async fn rename_organization_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
    Json(request): Json<RenameOrganizationRequest>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "organization": auth
            .rename_organization(&organization_id, &session.user_id, &request.name)
            .await?
    })))
}

pub(crate) async fn members(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
) -> Result<Json<Value>, AuthFailure> {
    let role = auth
        .require_organization_member(&organization_id, &session.user_id)
        .await?;
    Ok(Json(json!({
        "members": auth.list_members(&organization_id, &session.user_id).await?,
        "current_role": role
    })))
}

pub(crate) async fn organization_groups(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "groups": auth.list_organization_groups(&organization_id, &session.user_id).await?
    })))
}

pub(crate) async fn create_organization_group_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
    Json(request): Json<CreateOrganizationGroupRequest>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "group": auth.create_organization_group(&organization_id, &session.user_id, &request.name).await?
    })))
}

pub(crate) async fn delete_organization_group_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, group_id)): AxumPath<(String, String)>,
) -> Result<Json<Value>, AuthFailure> {
    auth.delete_organization_group(&organization_id, &session.user_id, &group_id)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub(crate) async fn add_organization_group_member_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, group_id, user_id)): AxumPath<(String, String, String)>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "groups": auth.set_organization_group_member(&organization_id, &session.user_id, &group_id, &user_id, true).await?
    })))
}

pub(crate) async fn remove_organization_group_member_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, group_id, user_id)): AxumPath<(String, String, String)>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "groups": auth.set_organization_group_member(&organization_id, &session.user_id, &group_id, &user_id, false).await?
    })))
}
pub(crate) async fn create_invitation(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
    body: axum::body::Bytes,
) -> Result<Response, AuthFailure> {
    let request = parse_optional_json::<treer_protocol::CreateInvitationRequest>(&body, "{}")?;
    let created = auth
        .create_organization_invitation(
            &organization_id,
            &AdminActor::User(session.user_id),
            request,
        )
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(created)).into_response())
}

pub(crate) async fn list_invitations(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
) -> Result<Json<treer_protocol::InvitationList>, AuthFailure> {
    Ok(Json(treer_protocol::InvitationList {
        invitations: auth
            .list_organization_invitations(&organization_id, &AdminActor::User(session.user_id))
            .await?,
    }))
}

pub(crate) async fn revoke_invitation(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, invitation_id)): AxumPath<(String, String)>,
) -> Result<StatusCode, AuthFailure> {
    auth.revoke_organization_invitation(
        &organization_id,
        &AdminActor::User(session.user_id),
        &invitation_id,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn list_agent_grants(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
) -> Result<Json<treer_protocol::AgentGrantList>, AuthFailure> {
    Ok(Json(
        auth.list_agent_grants(&organization_id, &session.user_id)
            .await?,
    ))
}

pub(crate) async fn put_agent_grant(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, agent_id)): AxumPath<(String, String)>,
    Json(request): Json<treer_protocol::PutAgentGrantRequest>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "grant": auth
            .put_agent_grant(&organization_id, &session.user_id, &agent_id, request)
            .await?
    })))
}

pub(crate) async fn delete_agent_grant(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, agent_id)): AxumPath<(String, String)>,
) -> Result<StatusCode, AuthFailure> {
    auth.delete_agent_grant(&organization_id, &session.user_id, &agent_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn leave_organization_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
) -> Result<StatusCode, AuthFailure> {
    auth.leave_organization(&organization_id, &session.user_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn transfer_organization_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
    Json(request): Json<treer_protocol::TransferOrganizationRequest>,
) -> Result<Json<Value>, AuthFailure> {
    auth.transfer_organization(&organization_id, &session.user_id, &request.user_id)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub(crate) async fn delete_organization_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
    Json(request): Json<treer_protocol::DeleteOrganizationRequest>,
) -> Result<Response, AuthFailure> {
    delete_organization_response(
        &auth,
        &organization_id,
        &AdminActor::User(session.user_id),
        &request.confirm_name,
    )
    .await
}

pub(crate) async fn delete_organization_response(
    auth: &AuthStore,
    organization_id: &str,
    actor: &AdminActor,
    confirm_name: &str,
) -> Result<Response, AuthFailure> {
    match auth
        .delete_organization(organization_id, actor, confirm_name)
        .await?
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(blocked) => Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": ProtocolError::new(
                    "organization_has_workspaces",
                    format!(
                        "delete the organization's {} workspace(s) first",
                        blocked.workspace_count
                    ),
                ),
                "workspace_count": blocked.workspace_count,
            })),
        )
            .into_response()),
    }
}

/// Parse a JSON body that older clients may omit entirely.
pub(crate) fn parse_optional_json<T: serde::de::DeserializeOwned>(
    body: &[u8],
    empty: &str,
) -> Result<T, AuthFailure> {
    let body = if body.iter().all(u8::is_ascii_whitespace) {
        empty.as_bytes()
    } else {
        body
    };
    serde_json::from_slice(body)
        .map_err(|error| AuthFailure::bad_request("invalid_request", &error.to_string()))
}

pub(crate) async fn update_member_role_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, user_id)): AxumPath<(String, String)>,
    Json(request): Json<UpdateMemberRoleRequest>,
) -> Result<Json<Value>, AuthFailure> {
    auth.update_member_role(&organization_id, &session.user_id, &user_id, &request.role)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub(crate) async fn remove_member_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, user_id)): AxumPath<(String, String)>,
) -> Result<Json<Value>, AuthFailure> {
    auth.remove_member(&organization_id, &session.user_id, &user_id)
        .await?;
    Ok(Json(json!({ "ok": true })))
}
