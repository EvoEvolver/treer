use super::*;
pub(crate) async fn oauth_start(
    Extension(auth): Extension<AuthStore>,
    AxumPath(provider): AxumPath<String>,
    Query(_query): Query<OAuthStartQuery>,
) -> Result<Response, AuthFailure> {
    // Legacy GET cannot opt into an organization membership mutation.
    start_oauth(&auth, &provider, None, false, false).await
}

pub(crate) async fn oauth_start_json(
    Extension(auth): Extension<AuthStore>,
    AxumPath(provider): AxumPath<String>,
    request: Request,
) -> Result<Response, AuthFailure> {
    let headers = request.headers();
    let origin = auth.app_public_url.origin().ascii_serialization();
    if headers.get_all(header::ORIGIN).iter().count() != 1
        || headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(origin.as_str())
        || headers.get("x-treer-console").and_then(|v| v.to_str().ok()) != Some("1")
        || headers
            .get("sec-fetch-site")
            .is_some_and(|v| v == "cross-site")
    {
        return Err(AuthFailure::forbidden(
            "csrf_denied",
            "console origin required",
        ));
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if content_type != "application/json" {
        return Err(AuthFailure::bad_request(
            "invalid_oauth_request",
            "JSON body required",
        ));
    }
    let bytes = axum::body::to_bytes(request.into_body(), 4096)
        .await
        .map_err(|_| AuthFailure::bad_request("invalid_oauth_request", "invalid OAuth request"))?;
    let request: OAuthStartQuery = serde_json::from_slice(&bytes)
        .map_err(|_| AuthFailure::bad_request("invalid_oauth_request", "invalid OAuth request"))?;
    if request.invite.as_ref().is_some_and(|v| {
        v.is_empty() || v.len() > 256 || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }) {
        return Err(invalid_invitation());
    }
    start_oauth(
        &auth,
        &provider,
        request.invite.as_deref(),
        request.join_organization,
        true,
    )
    .await
}

async fn start_oauth(
    auth: &AuthStore,
    provider: &str,
    invite: Option<&str>,
    join: bool,
    json: bool,
) -> Result<Response, AuthFailure> {
    let browser = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let url = auth
        .oauth_authorization_url(provider, invite, join, &browser)
        .await?;
    let mut response = if json {
        Json(json!({"authorization_url":url.as_str()})).into_response()
    } else {
        Redirect::to(url.as_str()).into_response()
    };
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&oauth_browser_cookie(auth, provider, &browser, 600))
            .map_err(AuthFailure::header)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    Ok(response)
}

fn oauth_browser_cookie(auth: &AuthStore, provider: &str, value: &str, max_age: u16) -> String {
    // Provider is checked against configured github/google before encoding.
    format!("treer_oauth_browser_{provider}={value}; Path=/api/auth/oauth; HttpOnly; SameSite=Lax; Max-Age={max_age}{}", secure_cookie_suffix(auth))
}

pub(crate) async fn oauth_callback(
    Extension(auth): Extension<AuthStore>,
    AxumPath(provider): AxumPath<String>,
    Query(query): Query<OAuthCallbackQuery>,
    headers: HeaderMap,
) -> Response {
    let result = async {
        let state = query.state.as_deref().ok_or_else(invalid_oauth_state)?;
        let browser = cookie_value(&headers, &format!("treer_oauth_browser_{provider}"))
            .ok_or_else(invalid_oauth_state)?;
        let pending = auth.consume_oauth_state(&provider, state, &browser).await?;
        if query.error.is_some() {
            return Err(oauth_login_failed());
        }
        let code = query.code.as_deref().ok_or_else(oauth_login_failed)?;
        let profile = auth.exchange_oauth_code(&provider, code).await?;
        let session = auth
            .complete_oauth_login(
                profile,
                pending.invite.as_deref(),
                pending.join_organization,
            )
            .await?;
        if let Some(invite) = pending.invite.as_deref() {
            auth.retain_pending_invitation(&session, invite).await?;
        }
        Ok(session)
    }
    .await;
    let mut response = match result {
        Ok(session) => oauth_session_redirect(&auth, &session),
        Err(error) => {
            tracing::warn!(?error, provider, "OAuth callback failed");
            oauth_error_redirect(&auth)
        }
    };
    if matches!(provider.as_str(), "github" | "google") {
        if let Ok(cookie) = HeaderValue::from_str(&oauth_browser_cookie(&auth, &provider, "", 0)) {
            response.headers_mut().append(header::SET_COOKIE, cookie);
        }
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
