use super::*;
pub(crate) async fn oauth_start(
    Extension(auth): Extension<AuthStore>,
    AxumPath(provider): AxumPath<String>,
    Query(query): Query<OAuthStartQuery>,
) -> Result<Redirect, AuthFailure> {
    let url = auth
        .oauth_authorization_url(&provider, query.invite.as_deref())
        .await?;
    Ok(Redirect::temporary(url.as_str()))
}

pub(crate) async fn oauth_callback(
    Extension(auth): Extension<AuthStore>,
    AxumPath(provider): AxumPath<String>,
    Query(query): Query<OAuthCallbackQuery>,
) -> Response {
    let result = async {
        let state = query.state.as_deref().ok_or_else(invalid_oauth_state)?;
        let invite = auth.consume_oauth_state(&provider, state).await?;
        if query.error.is_some() {
            return Err(oauth_login_failed());
        }
        let code = query.code.as_deref().ok_or_else(oauth_login_failed)?;
        let profile = auth.exchange_oauth_code(&provider, code).await?;
        auth.complete_oauth_login(profile, invite.as_deref()).await
    }
    .await;
    match result {
        Ok(session) => oauth_session_redirect(&auth, &session),
        Err(error) => {
            tracing::warn!(?error, provider, "OAuth callback failed");
            oauth_error_redirect(&auth)
        }
    }
}
