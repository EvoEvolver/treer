use super::*;
pub(crate) async fn authenticate_request(
    auth: &AuthStore,
    headers: &HeaderMap,
) -> Result<CurrentSession, AuthFailure> {
    if auth.disabled {
        return Ok(CurrentSession {
            token: "local".to_string(),
            user_id: "local".to_string(),
            email: "local@treer.invalid".to_string(),
            preferred_name: "Local user".to_string(),
        });
    }
    let token = bearer_token(headers)
        .map(str::to_owned)
        .or_else(|| cookie_value(headers, SESSION_COOKIE))
        .ok_or_else(|| {
            AuthFailure::unauthorized("authentication_required", "authentication required")
        })?;
    auth.session(&token).await?.ok_or_else(|| {
        AuthFailure::unauthorized("authentication_required", "authentication required")
    })
}

pub(crate) async fn login(
    Extension(auth): Extension<AuthStore>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<Response, AuthFailure> {
    let client = native_client_attribution(
        &headers,
        request.device_id.as_deref(),
        request.device_name.as_deref(),
    )?;
    let session = auth
        .login_with_client(&request.email, &request.password, client)
        .await?;
    Ok(session_response(&auth, &session, &headers))
}

pub(crate) async fn oauth_config(Extension(auth): Extension<AuthStore>) -> Json<Value> {
    Json(auth.oauth_public_config())
}
pub(crate) async fn register(
    Extension(auth): Extension<AuthStore>,
    headers: HeaderMap,
    Json(request): Json<RegisterRequest>,
) -> Result<Response, AuthFailure> {
    let client = native_client_attribution(
        &headers,
        request.device_id.as_deref(),
        request.device_name.as_deref(),
    )?;
    let session = auth
        .register_with_client(
            request.invite.as_deref(),
            &request.email,
            &request.preferred_name,
            &request.password,
            client,
        )
        .await?;
    Ok(session_response(&auth, &session, &headers))
}

pub(crate) async fn me(Extension(session): Extension<CurrentSession>) -> Json<Value> {
    Json(user_json(&session))
}

pub(crate) async fn update_profile(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Json(request): Json<UpdateProfileRequest>,
) -> Result<Json<Value>, AuthFailure> {
    let user = auth
        .update_profile(&session.user_id, &request.email, &request.preferred_name)
        .await?;
    Ok(Json(user_json(&user)))
}
pub(crate) async fn logout(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
) -> Result<Response, AuthFailure> {
    auth.logout(&session.token).await?;
    let cookie = format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
        secure_cookie_suffix(&auth)
    );
    Ok((
        [(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).map_err(AuthFailure::header)?,
        )],
        Json(json!({ "ok": true })),
    )
        .into_response())
}
