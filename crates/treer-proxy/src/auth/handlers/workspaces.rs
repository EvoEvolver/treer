use super::*;
pub(crate) async fn workspace_access(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(workspace_id): AxumPath<String>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "access": auth.workspace_access_info(&workspace_id, &session.user_id).await?
    })))
}

pub(crate) async fn update_workspace_access(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(workspace_id): AxumPath<String>,
    Json(request): Json<UpdateWorkspaceAccessRequest>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "access": auth.update_workspace_access_mode(&workspace_id, &session.user_id, &request.access_mode).await?
    })))
}

pub(crate) async fn update_workspace_user_grant(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((workspace_id, user_id)): AxumPath<(String, String)>,
    Json(request): Json<UpdateWorkspaceGrantRequest>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "access": auth.upsert_workspace_user_grant(&workspace_id, &session.user_id, &user_id, &request.role).await?
    })))
}

pub(crate) async fn delete_workspace_user_grant(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((workspace_id, user_id)): AxumPath<(String, String)>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "access": auth.remove_workspace_user_grant(&workspace_id, &session.user_id, &user_id).await?
    })))
}

pub(crate) async fn update_workspace_group_grant(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((workspace_id, group_id)): AxumPath<(String, String)>,
    Json(request): Json<UpdateWorkspaceGrantRequest>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "access": auth.upsert_workspace_group_grant(&workspace_id, &session.user_id, &group_id, &request.role).await?
    })))
}

pub(crate) async fn delete_workspace_group_grant(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((workspace_id, group_id)): AxumPath<(String, String)>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "access": auth.remove_workspace_group_grant(&workspace_id, &session.user_id, &group_id).await?
    })))
}

pub(crate) async fn audit_events(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
    Query(query): Query<AuditEventsQuery>,
) -> Result<Json<Value>, AuthFailure> {
    let events = auth
        .list_audit_events(
            &organization_id,
            &session.user_id,
            query.workspace_id.as_deref(),
            query.before,
            query.limit,
        )
        .await?;
    let next_cursor = events.last().map(|event| event.sequence);
    Ok(Json(
        json!({ "events": events, "next_cursor": next_cursor }),
    ))
}
