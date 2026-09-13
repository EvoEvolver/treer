use super::*;
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
) -> Result<Json<Value>, AuthFailure> {
    let (token, url) = auth
        .create_invitation(&organization_id, &session.user_id)
        .await?;
    Ok(Json(json!({ "token": token, "url": url.as_str() })))
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
