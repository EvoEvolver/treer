use super::*;
pub(crate) async fn admin_login(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<AdminLoginRequest>,
) -> Result<Response, AuthFailure> {
    let session = auth.admin_login(&request.password).await?;
    let cookie = format!(
        "{ADMIN_SESSION_COOKIE}={}; Path=/api/admin; HttpOnly; SameSite=Strict; Max-Age={}{}",
        session.token,
        ADMIN_SESSION_TTL_HOURS * 60 * 60,
        secure_cookie_suffix(&auth)
    );
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "admin": true })),
    )
        .into_response())
}

pub(crate) async fn admin_me() -> Json<Value> {
    Json(json!({ "admin": true }))
}

pub(crate) async fn admin_logout(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<AdminSession>,
) -> Result<Response, AuthFailure> {
    if !auth.disabled {
        auth.admin_logout(&session.token).await?;
    }
    let cookie = format!(
        "{ADMIN_SESSION_COOKIE}=; Path=/api/admin; HttpOnly; SameSite=Strict; Max-Age=0{}",
        secure_cookie_suffix(&auth)
    );
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}
