use super::*;
pub(crate) async fn request_password_reset(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<RequestPasswordResetRequest>,
) -> Result<Json<Value>, AuthFailure> {
    auth.request_password_reset(&request.email).await?;
    Ok(Json(json!({ "ok": true })))
}

pub(crate) async fn reset_password(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<ResetPasswordRequest>,
) -> Result<Json<Value>, AuthFailure> {
    auth.reset_password(&request.token, &request.password)
        .await?;
    Ok(Json(json!({ "ok": true })))
}
