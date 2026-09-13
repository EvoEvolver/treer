use super::*;
pub(crate) fn session_response(
    auth: &AuthStore,
    session: &CurrentSession,
    headers: &HeaderMap,
) -> Response {
    let cookie = format!(
        "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        session.token,
        SESSION_TTL_DAYS * 24 * 60 * 60,
        secure_cookie_suffix(auth)
    );
    let mut body = user_json(session);
    if native_client_kind(headers).is_some() {
        if let Some(object) = body.as_object_mut() {
            object.insert("token".to_string(), json!(session.token));
        }
    }
    ([(header::SET_COOKIE, cookie)], Json(body)).into_response()
}

pub(crate) fn oauth_session_redirect(auth: &AuthStore, session: &CurrentSession) -> Response {
    let cookie = format!(
        "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        session.token,
        SESSION_TTL_DAYS * 24 * 60 * 60,
        secure_cookie_suffix(auth)
    );
    let mut response = Redirect::to(auth.app_public_url.as_str()).into_response();
    match HeaderValue::from_str(&cookie) {
        Ok(cookie) => {
            response.headers_mut().insert(header::SET_COOKIE, cookie);
            response
        }
        Err(error) => AuthFailure::header(error).into_response(),
    }
}

pub(crate) fn oauth_error_redirect(auth: &AuthStore) -> Response {
    let mut url = auth.app_public_url.clone();
    url.query_pairs_mut()
        .append_pair("oauth_error", "login_failed");
    Redirect::to(url.as_str()).into_response()
}

pub(crate) fn user_json(session: &CurrentSession) -> Value {
    json!({
        "user_id": session.user_id,
        "email": session.email,
        "preferred_name": session.preferred_name,
    })
}

pub(crate) fn secure_cookie_suffix(auth: &AuthStore) -> &'static str {
    if auth.secure_cookies {
        "; Secure"
    } else {
        ""
    }
}

pub(crate) fn native_client_kind(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(NATIVE_CLIENT_HEADER)?.to_str().ok()?.trim();
    matches!(value, "mobile" | "mobile_ios" | "mobile_android").then_some(value)
}

pub(crate) fn native_client_attribution(
    headers: &HeaderMap,
    device_id: Option<&str>,
    device_name: Option<&str>,
) -> Result<Option<NativeClientAttribution>, AuthFailure> {
    let Some(client) = native_client_kind(headers) else {
        return Ok(None);
    };
    let device_id = match device_id.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => {
            Uuid::parse_str(value).map_err(|_| {
                AuthFailure::bad_request("invalid_device_id", "device_id must be a UUID")
            })?;
            Some(value.to_string())
        }
        None => None,
    };
    let device_name = match device_name.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) if value.chars().count() > MAX_DEVICE_NAME_CHARS => {
            return Err(AuthFailure::bad_request(
                "invalid_device_name",
                "device_name is too long",
            ));
        }
        Some(value) => Some(value.to_string()),
        None => None,
    };
    Ok(Some(NativeClientAttribution {
        client: client.to_string(),
        device_id,
        device_name,
    }))
}

pub(crate) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|item| item.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_string()))
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
}

pub(crate) fn optional_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Result<Option<&'a str>, AuthFailure> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map(str::trim)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(agent_auth_required)
        })
        .transpose()
}

pub(crate) fn agent_auth_required() -> AuthFailure {
    AuthFailure::unauthorized(
        "agent_authentication_required",
        "valid Agent workload credentials are required",
    )
}

pub(crate) fn fast_secret_hash(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

pub(crate) fn valid_pkce_verifier(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

pub(crate) fn valid_pkce_challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub(crate) fn fast_secret_matches(secret: &str, expected_hash: &str) -> bool {
    let actual = fast_secret_hash(secret);
    actual.len() == expected_hash.len()
        && actual
            .as_bytes()
            .ct_eq(expected_hash.as_bytes())
            .unwrap_u8()
            == 1
}

pub(crate) fn machine_workspace_matches(session: &MachineSession, path: &str) -> bool {
    if path == "/agent/machine/identity" {
        return session.server_id.is_some() && session.workspace_id.is_some();
    }
    let Some(encoded_workspace) = path
        .strip_prefix("/agent/workspaces/")
        .and_then(|rest| rest.split('/').next())
    else {
        return false;
    };
    percent_encoding::percent_decode_str(encoded_workspace)
        .decode_utf8()
        .is_ok_and(|workspace| session.allows_workspace(&workspace))
}

pub(crate) fn workspace_id_from_api_path(path: &str) -> Option<String> {
    let encoded = path.strip_prefix("/api/workspaces/")?.split('/').next()?;
    percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()
        .map(|workspace| workspace.into_owned())
}

pub(crate) fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub(crate) fn parse_credential<'a>(
    token: &'a str,
    expected_prefix: &str,
) -> Result<(&'a str, &'a str), AuthFailure> {
    let (identifier, secret) = token.split_once('.').ok_or_else(machine_auth_required)?;
    if !identifier.starts_with(expected_prefix) || secret.len() != 64 {
        return Err(machine_auth_required());
    }
    Ok((identifier, secret))
}

pub(crate) fn machine_auth_required() -> AuthFailure {
    AuthFailure::unauthorized(
        "machine_authentication_required",
        "valid machine credentials are required",
    )
}

pub(crate) fn invalid_machine_enrollment() -> AuthFailure {
    AuthFailure::unauthorized(
        "invalid_machine_enrollment",
        "machine enrollment token is invalid, expired, or already used",
    )
}

pub(crate) fn invalid_password_reset() -> AuthFailure {
    AuthFailure::bad_request(
        "invalid_password_reset",
        "password reset link is invalid or expired",
    )
}

pub(crate) fn invalid_invitation() -> AuthFailure {
    AuthFailure::bad_request(
        "invalid_invitation",
        "invitation is invalid or already used",
    )
}

pub(crate) fn oauth_provider_unavailable() -> AuthFailure {
    AuthFailure::not_found(
        "oauth_provider_unavailable",
        "this OAuth provider is not configured",
    )
}

pub(crate) fn invalid_oauth_state() -> AuthFailure {
    AuthFailure::bad_request(
        "invalid_oauth_state",
        "the OAuth login request is invalid or expired",
    )
}

pub(crate) fn oauth_login_failed() -> AuthFailure {
    AuthFailure::unauthorized("oauth_login_failed", "OAuth login failed")
}

pub(crate) fn verified_email_required() -> AuthFailure {
    AuthFailure::forbidden(
        "verified_email_required",
        "the OAuth provider must provide a verified email address",
    )
}

pub(crate) fn oauth_request_failed(error: reqwest::Error) -> AuthFailure {
    tracing::warn!(%error, "OAuth provider request failed");
    oauth_login_failed()
}

pub(crate) fn provider_preferred_name(
    candidate: Option<&str>,
    fallback: &str,
    email: &str,
) -> Result<String, AuthFailure> {
    if let Some(candidate) = candidate {
        if let Ok(name) = validate_preferred_name(candidate) {
            return Ok(name);
        }
    }
    if let Ok(name) = validate_preferred_name(fallback) {
        return Ok(name);
    }
    validate_preferred_name(email.split('@').next().unwrap_or("Treer user"))
}

pub(crate) fn invalid_ingress_authorization() -> AuthFailure {
    AuthFailure::unauthorized(
        "invalid_ingress_authorization",
        "ingress authorization is invalid or expired",
    )
}

pub(crate) fn invalid_app_oauth_code() -> AuthFailure {
    AuthFailure::unauthorized(
        "invalid_app_oauth_code",
        "app OAuth code is invalid, expired, or already used",
    )
}

pub(crate) fn parse_password_reset_token(token: &str) -> Result<(&str, &str), AuthFailure> {
    let (token_id, secret) = token.split_once('.').ok_or_else(invalid_password_reset)?;
    if token_id.len() != 36
        || !token_id.starts_with("pwd_")
        || !token_id[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
        || secret.len() != 64
        || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_password_reset());
    }
    Ok((token_id, secret))
}

pub(crate) fn validate_new_password(password: &str) -> Result<String, AuthFailure> {
    if password.len() < 8 {
        return Err(AuthFailure::bad_request(
            "invalid_password",
            "password must contain at least 8 characters",
        ));
    }
    if password.len() > 1024 {
        return Err(AuthFailure::bad_request(
            "invalid_password",
            "password must contain at most 1024 characters",
        ));
    }
    Ok(password.to_string())
}
